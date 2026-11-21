// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: a compiled program's kernel sandbox (DENEXT.md item 473).
//!
//! A program built with `deno compile --kernel-sandbox=enforce` holds its
//! baked grant with the OS from its first instruction of user code: `main`
//! calls [`startup`] once the embedded metadata is read, while the process
//! has one thread and has opened nothing of its own, and the sandbox is
//! applied in place (`deno_lib::kernel_apply::start_compiled`; no re-exec:
//! the modules are embedded and nothing is fetched). Flags, the environment
//! and deno.json are not read for it; the mode is the author's.
//!
//! Where the baked grant has a rule only the supervising broker holds (a
//! host-scoped network, a write deny inside a write allow on Linux), the
//! process forks inside [`startup`] before it confines itself: the parent
//! stays behind as the supervisor and never leaves `startup` (one thread
//! at the fork; it never loads the JavaScript engine or any of the embedded
//! modules, serves the broker, passes signals on and exits as the program
//! exits); the child returns from `startup` as the confined program.
//! Everything in `main` after `startup` therefore runs in the worker only.

use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

use deno_core::error::AnyError;
use deno_lib::kernel_apply::CompiledOutcome;
use deno_lib::kernel_apply::CompiledStart;
use deno_lib::kernel_apply::start_compiled;
use deno_runtime::deno_permissions::PermissionsOptions;

use crate::binary::StandaloneData;

static OUTCOME: OnceLock<CompiledOutcome> = OnceLock::new();

/// The grant the kernel holds a compiled program to: its baked permissions
/// intersected with the layers a confined ancestor handed down, plus the
/// folder a self-extracting program's files were extracted to (real files
/// it loads). The embedded file system's virtual root is never part of it:
/// nothing of it is on disk.
pub fn kernel_options(data: &StandaloneData) -> PermissionsOptions {
  let mut options = data.metadata.permissions.clone();
  if data.metadata.self_extracting.is_some() {
    crate::run::grant_vfs_read_access(&mut options, &data.root_path);
  }
  let inherited = deno_lib::kernel_inherit::inherited_layers();
  if !inherited.is_empty() {
    #[allow(clippy::disallowed_methods, reason = "the process's real cwd")]
    let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
    deno_lib::kernel_inherit::clamp(&mut options, inherited, &cwd);
    if data.metadata.self_extracting.is_some() {
      crate::run::grant_vfs_read_access(&mut options, &data.root_path);
    }
  }
  options
}

/// Where a confined program keeps its own cache: beside the embedded file
/// system's virtual root in the temp folder, `deno-compile-<name>.sandboxed`
/// (one folder per policy beneath it).
fn cache_base() -> Option<PathBuf> {
  #[allow(clippy::disallowed_methods, reason = "the real temp folder")]
  let temp = deno_lib::kernel_apply::canonical_dir(std::env::temp_dir());
  let exe = std::env::current_exe().ok()?;
  let name = exe.file_name()?.to_string_lossy().into_owned();
  Some(temp.join(format!("deno-compile-{name}.sandboxed")))
}

/// The kernel-sandbox startup of a compiled program (not a desktop app,
/// whose process hosts a browser engine: `denort_desktop` has its own).
/// Errors mean refusing to start.
pub fn startup(data: &StandaloneData) -> Result<(), AnyError> {
  let options = kernel_options(data);
  let outcome = start_compiled(&CompiledStart {
    baked: data.metadata.kernel_sandbox,
    exec_strict: data.metadata.kernel_sandbox_exec_strict,
    dev_build: data.metadata.desktop_dev,
    options: &options,
    cache_base: cache_base(),
  })?;
  // started inside a brokered run and not confining itself: the way to
  // that run's broker its parent left (a program that confines itself gave
  // it up before it applied its sandbox, in `start_compiled`)
  #[cfg(any(target_os = "linux", target_os = "macos"))]
  if !outcome.enforcing {
    deno_lib::kernel_supervise::adopt_inherited_dial(
      !deno_lib::kernel_inherit::inherited_layers().is_empty(),
    );
  }
  if let Some(dir) = &outcome.cache_dir {
    // native addons are extracted beneath the program's own cache folder,
    // the only place it may write for itself
    deno_runtime::deno_napi::set_native_addon_cache_dir(dir.join("addons"));
  }
  let _ = OUTCOME.set(outcome);
  Ok(())
}

/// Whether the kernel sandbox this program applied at startup holds it.
pub fn enforcing() -> bool {
  OUTCOME.get().is_some_and(|o| o.enforcing)
}

/// The program's own cache folder while it is confined.
pub fn cache_dir() -> Option<&'static Path> {
  OUTCOME.get().and_then(|o| o.cache_dir.as_deref())
}

/// Whether a variable from the baked env file may be set: a confined
/// program's environment was scrubbed to the names its plan keeps, and the
/// env file's variables follow the same rule (as in the CLI, which loads
/// env files before the scrub).
pub fn keeps_env(name: &str) -> bool {
  OUTCOME
    .get()
    .and_then(|o| o.plan.as_ref())
    .is_none_or(|plan| plan.keeps_env(name))
}
