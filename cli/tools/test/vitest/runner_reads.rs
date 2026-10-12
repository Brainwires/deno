// Copyright 2018-2026 the Deno authors. MIT license.

//! The Vitest / Jest runner's own reads, made without `--allow-read`.
//!
//! Setting a run up reads files the way Deno itself reads `deno.json`, the
//! module graph or `node_modules` before any test code runs: the config file
//! the runner found (and the project files and Jest presets it names), the
//! tsconfig / `.env` / Babel files it reduces to options, the `__mocks__`
//! directories a mock is looked up in, and the modules the runner imports
//! itself (setup files, `__mocks__` files, automocked modules). Those are
//! runner behaviour, so they need no permission, as long as they stay within
//! the run's root ([`set_scope`]): the working directory and the config
//! file's directory.
//!
//! Like Deno's internal reads, this is a privileged path, not a permission:
//!
//! - Only a worker that runs the runner (the config worker, a Vitest test
//!   file's worker) has this extension. Its ops are reachable from user code
//!   (like the other ops `cli/js/40_test.js` uses, they cannot be imported
//!   from `ext:core/ops`), so they read with privilege only when given the
//!   worker's secret token. The runner's entry module (`config_loader.js`,
//!   or a test file's `preload.js`) gets the token, once, through
//!   `Deno[Deno.internal].takeVitestRunnerFs()`, before any user code runs;
//!   a call from any other module (or a second call) gets nothing. The
//!   runner keeps the functions bound to the token to itself and hands them
//!   only to the functions that do its reads.
//! - A path is privileged only when it lies within the scope once
//!   canonicalized (so a symlink out of it, or a `..`, is not), like
//!   `NpmRegistryReadPermissionChecker` does for `node_modules`. Any other path goes through the worker's permissions,
//!   as a read of the test would.
//! - Only the config worker reads file contents; a test file's worker only
//!   asks whether files exist and lists `__mocks__` directories.
//! - A module the runner imports is loaded like a static import (see
//!   [`super::mocks::VitestMocks::allow_runner_import`]): its own static
//!   imports are part of the module graph, while whatever its code does at
//!   runtime (a read, a dynamic import) is checked as usual.

use std::borrow::Cow;
use std::io::ErrorKind;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;

use deno_core::OpState;
use deno_core::op2;
use deno_core::url::Url;
use deno_core::v8;
use deno_error::JsErrorBox;
use deno_runtime::deno_permissions::OpenAccessKind;
use deno_runtime::deno_permissions::PermissionsContainer;
use uuid::Uuid;

use super::SCHEME;
use crate::util::fs::canonicalize_path;
use crate::util::fs::canonicalize_path_maybe_not_exists;

/// The roots the runner may read within, canonicalized.
static SCOPE: RwLock<Option<Arc<Vec<PathBuf>>>> = RwLock::new(None);

/// Sets the run's root directories (set by `VitestCompat::setup`). A root
/// that does not exist, or the file system root, is left out.
pub fn set_scope<'a>(roots: impl IntoIterator<Item = &'a Path>) {
  let mut canonical = Vec::new();
  for root in roots {
    let Ok(root) = canonicalize_path(root) else {
      continue;
    };
    if root.parent().is_none() || canonical.contains(&root) {
      continue;
    }
    canonical.push(root);
  }
  *SCOPE.write().unwrap() = Some(Arc::new(canonical));
}

/// `path`, canonicalized, when it lies within the scope. A path that does
/// not exist (yet) is within the scope when its nearest existing ancestor
/// is.
fn in_scope(roots: &[PathBuf], path: &Path) -> Option<PathBuf> {
  if roots.is_empty()
    || !path.is_absolute()
    || path
      .components()
      .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
  {
    return None;
  }
  let canonical = canonicalize_path_maybe_not_exists(path).ok()?;
  roots
    .iter()
    .any(|root| canonical.starts_with(root))
    .then_some(canonical)
}

/// Which worker runs the runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerWorker {
  /// The worker that evaluates the config (`config_loader.js`).
  Config,
  /// A Vitest test file's worker.
  Test,
}

/// The runner's privileged reads in this worker (see the module docs).
struct RunnerReads {
  worker: RunnerWorker,
  /// The token that makes a call privileged, once the runner took it.
  token: Option<String>,
}

deno_core::extension!(deno_vitest_runner,
  ops = [
    op_vitest_runner_take,
    op_vitest_runner_stat,
    op_vitest_runner_read_dir,
    op_vitest_runner_read_text,
    op_vitest_runner_allow_import,
  ],
  options = {
    worker: RunnerWorker,
  },
  state = |state, options| {
    state.put(RunnerReads {
      worker: options.worker,
      token: None,
    });
  },
);

/// The runner's entry modules, the only ones that may take the ops: the
/// config worker's main module and a test file's preload.
const ENTRY_MODULES: [&str; 2] = ["/config_loader.js", "/preload.js"];

/// Whether the innermost frame outside Deno's own `ext:` modules is one of
/// the runner's entry modules ([`ENTRY_MODULES`]).
fn called_by_runner(scope: &mut v8::PinScope<'_, '_>) -> bool {
  let Some(stack) = v8::StackTrace::current_stack_trace(scope, 16) else {
    return false;
  };
  for i in 0..stack.get_frame_count() {
    let Some(frame) = stack.get_frame(scope, i) else {
      return false;
    };
    let Some(name) = frame.get_script_name(scope) else {
      return false;
    };
    let name = name.to_rust_string_lossy(scope);
    if name.starts_with("ext:") {
      continue;
    }
    return Url::parse(&name).is_ok_and(|url| {
      url.scheme() == SCHEME && ENTRY_MODULES.contains(&url.path())
    });
  }
  false
}

/// Hands the token of the runner's privileged reads to the calling entry
/// module, once per worker; `null` for anyone else.
#[op2]
#[string]
pub fn op_vitest_runner_take(
  scope: &mut v8::PinScope<'_, '_>,
  state: &mut OpState,
) -> Option<String> {
  let reads = state.try_borrow_mut::<RunnerReads>()?;
  if reads.token.is_some() || !called_by_runner(scope) {
    return None;
  }
  let token = Uuid::new_v4().to_string();
  reads.token = Some(token.clone());
  Some(token)
}

/// Whether a call with `token` is the runner's (and, for `contents`, made in
/// the config worker).
fn is_runner(state: &OpState, token: &str, contents: bool) -> bool {
  state.try_borrow::<RunnerReads>().is_some_and(|reads| {
    reads.token.as_deref() == Some(token)
      && (!contents || reads.worker == RunnerWorker::Config)
  })
}

/// The path to read: canonicalized when the runner may read it without a
/// permission check, else as given once the worker's permissions allow it.
fn checked_path<'a>(
  state: &OpState,
  token: &str,
  path: &'a str,
  contents: bool,
) -> Result<Cow<'a, Path>, JsErrorBox> {
  let path = Path::new(path);
  let privileged = is_runner(state, token, contents);
  if privileged {
    let roots = SCOPE.read().unwrap().clone();
    if let Some(path) = roots.and_then(|roots| in_scope(&roots, path)) {
      return Ok(Cow::Owned(path));
    }
  }
  state
    .borrow::<PermissionsContainer>()
    .check_open(Cow::Borrowed(path), OpenAccessKind::Read, None)
    .map(|path| path.into_path())
    .map_err(JsErrorBox::from_err)
}

/// What `path` is: 0 when it does not exist, 1 a file, 2 a directory, 3
/// something else (symlinks followed).
#[op2(fast)]
#[smi]
pub fn op_vitest_runner_stat(
  state: &mut OpState,
  #[string] token: &str,
  #[string] path: &str,
) -> Result<u8, JsErrorBox> {
  let path = checked_path(state, token, path, false)?;
  Ok(match std::fs::metadata(&path) {
    Ok(metadata) if metadata.is_file() => 1,
    Ok(metadata) if metadata.is_dir() => 2,
    Ok(_) => 3,
    Err(err) if err.kind() == ErrorKind::NotFound => 0,
    Err(err) => return Err(JsErrorBox::from_err(err)),
  })
}

/// The names of the entries of directory `path`; with `recursive`, also
/// those of its subdirectories (as relative paths; symlinks to directories
/// are not followed), like `fs.readdirSync(path, { recursive: true })`.
#[op2]
#[serde]
pub fn op_vitest_runner_read_dir(
  state: &mut OpState,
  #[string] token: &str,
  #[string] path: &str,
  recursive: bool,
) -> Result<Vec<String>, JsErrorBox> {
  let path = checked_path(state, token, path, false)?;
  let mut names = Vec::new();
  read_dir_into(&path, None, recursive, &mut names)
    .map_err(JsErrorBox::from_err)?;
  Ok(names)
}

fn read_dir_into(
  dir: &Path,
  prefix: Option<&Path>,
  recursive: bool,
  names: &mut Vec<String>,
) -> std::io::Result<()> {
  let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
  entries.sort_by_key(|entry| entry.file_name());
  for entry in entries {
    let name = match prefix {
      Some(prefix) => prefix.join(entry.file_name()),
      None => PathBuf::from(entry.file_name()),
    };
    names.push(name.to_string_lossy().into_owned());
    if recursive && entry.file_type()?.is_dir() {
      read_dir_into(&entry.path(), Some(&name), true, names)?;
    }
  }
  Ok(())
}

/// The text of file `path` (in the config worker only).
#[op2]
#[string]
pub fn op_vitest_runner_read_text(
  state: &mut OpState,
  #[string] token: &str,
  #[string] path: &str,
) -> Result<String, JsErrorBox> {
  let path = checked_path(state, token, path, true)?;
  std::fs::read_to_string(&path).map_err(JsErrorBox::from_err)
}

/// Lets the runner import the module at `url` (a `file:` URL) like a static
/// import, when it lies within the scope. Returns whether it does.
#[op2(fast)]
pub fn op_vitest_runner_allow_import(
  state: &mut OpState,
  #[string] token: &str,
  #[string] url: &str,
) -> bool {
  if !is_runner(state, token, false) {
    return false;
  }
  let Ok(url) = Url::parse(url) else {
    return false;
  };
  let Ok(path) = deno_path_util::url_to_file_path(&url) else {
    return false;
  };
  let Some(roots) = SCOPE.read().unwrap().clone() else {
    return false;
  };
  if in_scope(&roots, &path).is_none() {
    return false;
  }
  let Ok(mocks) = super::mocks::mocks(state) else {
    return false;
  };
  mocks.allow_runner_import(url);
  true
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn scope_is_canonical_and_confined() {
    let dir = tempfile::tempdir().unwrap();
    let base = canonicalize_path(dir.path()).unwrap();
    let root = base.join("project");
    let outside = base.join("outside");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(root.join("src/a.ts"), "").unwrap();
    std::fs::write(outside.join("secret.txt"), "").unwrap();
    let roots = vec![root.clone()];

    assert_eq!(
      in_scope(&roots, &root.join("src/a.ts")),
      Some(root.join("src/a.ts"))
    );
    // a path that does not exist yet, below an existing directory
    assert_eq!(
      in_scope(&roots, &root.join("src/__mocks__/a.ts")),
      Some(root.join("src/__mocks__/a.ts"))
    );
    assert_eq!(in_scope(&roots, &outside.join("secret.txt")), None);
    // `..` is never privileged, even when it stays inside
    assert_eq!(in_scope(&roots, &root.join("src/../src/a.ts")), None);
    assert_eq!(in_scope(&roots, &root.join("../outside/secret.txt")), None);
    // relative paths are not privileged
    assert_eq!(in_scope(&roots, Path::new("src/a.ts")), None);
    // nor is anything without a root
    assert_eq!(in_scope(&[], &root.join("src/a.ts")), None);
    // a sibling directory sharing the root's name as a prefix
    std::fs::create_dir_all(base.join("project-other")).unwrap();
    assert_eq!(in_scope(&roots, &base.join("project-other")), None);

    #[cfg(unix)]
    {
      // a symlink out of the root
      std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
      assert_eq!(in_scope(&roots, &root.join("link/secret.txt")), None);
      assert_eq!(in_scope(&roots, &root.join("link/missing.txt")), None);
      // a symlink within the root
      std::os::unix::fs::symlink(root.join("src"), root.join("src_link"))
        .unwrap();
      assert_eq!(
        in_scope(&roots, &root.join("src_link/a.ts")),
        Some(root.join("src/a.ts"))
      );
    }
  }
}
