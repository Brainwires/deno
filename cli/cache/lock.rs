// Copyright 2018-2026 the Deno authors. MIT license.

//! The CLI side of `deno_cache_dir::cache_lock`: which package cache lock a
//! subcommand holds, the download lock taken around network fetches, and
//! locks on the artifacts a command writes. See that module for the rules.

use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

use deno_cache_dir::cache_lock::CacheLockGuard;
pub use deno_cache_dir::cache_lock::CacheLockMode;
use deno_cache_dir::cache_lock::CacheLocker;
use deno_cache_dir::cache_lock::FileLockKind;
use deno_cache_dir::cache_lock::PathLockGuard;

use crate::args::DenoSubcommand;
use crate::args::Flags;
use crate::colors;
use crate::sys::CliSys;

/// One locker per process, for the `DENO_DIR` resolved at startup.
static PACKAGE_CACHE_LOCKER: OnceLock<CacheLocker<CliSys>> = OnceLock::new();
/// The root of that `DENO_DIR`, which also holds the artifact lock files.
static DENO_DIR_ROOT: OnceLock<PathBuf> = OnceLock::new();

pub type PackageCacheLockGuard = CacheLockGuard<'static, CliSys>;
pub type ArtifactLockGuard =
  PathLockGuard<<CliSys as sys_traits::BaseFsOpen>::File>;

fn report_blocking(description: &str) {
  log::info!(
    "{} waiting for file lock on {}",
    colors::cyan("Blocking"),
    description
  );
}

/// Sets up the package cache locker for this process. Called once at
/// startup with the `DENO_DIR` the command will use.
pub fn init(deno_dir_root: &Path) {
  _ = DENO_DIR_ROOT.get_or_init(|| deno_dir_root.to_path_buf());
  _ = PACKAGE_CACHE_LOCKER.get_or_init(|| {
    CacheLocker::new(
      CliSys::default(),
      deno_dir_root,
      Box::new(report_blocking),
    )
  });
}

fn locker() -> Option<&'static CacheLocker<CliSys>> {
  PACKAGE_CACHE_LOCKER.get()
}

/// The package cache lock a subcommand holds for as long as it runs, if any.
///
/// Commands that finish on their own read the cache under `Shared` for their
/// whole run, the way a Cargo build does. Commands that may run indefinitely
/// (a server, a REPL, a watcher, the language server) do not: holding a lock
/// for the lifetime of a dev server would make `deno clean` wait on it until
/// it is stopped. They still take `DownloadExclusive` around every download
/// (see [`lock_download`]), which `deno clean` waits on.
///
/// `deno clean` deletes cache entries, so it takes `MutateExclusive`.
pub fn subcommand_lock_mode(flags: &Flags) -> Option<CacheLockMode> {
  if flags.watch.is_some() {
    return None;
  }
  match &flags.subcommand {
    DenoSubcommand::Clean(_) => Some(CacheLockMode::MutateExclusive),
    DenoSubcommand::Add(_)
    | DenoSubcommand::Audit(_)
    | DenoSubcommand::ApproveScripts(_)
    | DenoSubcommand::Remove(_)
    | DenoSubcommand::Bench(_)
    | DenoSubcommand::Cache(_)
    | DenoSubcommand::Check(_)
    | DenoSubcommand::Ci(_)
    | DenoSubcommand::Compile(_)
    | DenoSubcommand::Coverage(_)
    | DenoSubcommand::Doc(_)
    | DenoSubcommand::Info(_)
    | DenoSubcommand::Install(_)
    | DenoSubcommand::Link(_)
    | DenoSubcommand::Unlink(_)
    | DenoSubcommand::Lint(_)
    | DenoSubcommand::Test(_)
    | DenoSubcommand::Outdated(_)
    | DenoSubcommand::Why(_)
    | DenoSubcommand::Publish(_)
    | DenoSubcommand::Pack(_) => Some(CacheLockMode::Shared),
    DenoSubcommand::Bundle(bundle_flags) => {
      (!bundle_flags.watch).then_some(CacheLockMode::Shared)
    }
    DenoSubcommand::Desktop(desktop_flags) => {
      (!desktop_flags.hmr).then_some(CacheLockMode::Shared)
    }
    // may run indefinitely
    DenoSubcommand::Eval(_)
    | DenoSubcommand::Jupyter(_)
    | DenoSubcommand::Lsp
    | DenoSubcommand::Repl(_)
    | DenoSubcommand::Run(_)
    | DenoSubcommand::Serve(_)
    | DenoSubcommand::Task(_)
    | DenoSubcommand::X(_)
    | DenoSubcommand::Deploy(_) => None,
    // don't read the package caches
    DenoSubcommand::Completions(_)
    | DenoSubcommand::Fmt(_)
    | DenoSubcommand::Init(_)
    | DenoSubcommand::List(_)
    | DenoSubcommand::JSONReference(_)
    | DenoSubcommand::Uninstall(_)
    | DenoSubcommand::Transpile(_)
    | DenoSubcommand::Types
    | DenoSubcommand::Upgrade(_)
    | DenoSubcommand::Vendor
    | DenoSubcommand::BumpVersion(_)
    | DenoSubcommand::Help(_) => None,
  }
}

/// Takes the package cache lock for a whole subcommand. Blocks (after saying
/// so) while another process holds a conflicting lock.
pub fn lock_for_subcommand(
  mode: CacheLockMode,
) -> Option<PackageCacheLockGuard> {
  let locker = locker()?;
  match locker.lock(mode) {
    Ok(guard) => Some(guard),
    Err(err) => {
      log::debug!("Not taking the package cache lock: {err:#}");
      None
    }
  }
}

/// Takes `DownloadExclusive` for the duration of a download into the package
/// cache. Waiting for another process happens on a blocking thread, so the
/// event loop keeps running.
pub async fn lock_download() -> Option<PackageCacheLockGuard> {
  let locker = locker()?;
  match locker.try_lock(CacheLockMode::DownloadExclusive) {
    Ok(Some(guard)) => Some(guard),
    Ok(None) => {
      let result = deno_core::unsync::spawn_blocking(move || {
        locker.lock(CacheLockMode::DownloadExclusive)
      })
      .await;
      match result {
        Ok(Ok(guard)) => Some(guard),
        Ok(Err(err)) => {
          log::debug!("Not taking the package cache download lock: {err:#}");
          None
        }
        Err(err) => {
          log::debug!("Failed waiting for the package cache lock: {err:#}");
          None
        }
      }
    }
    Err(err) => {
      // For example, a `deno run` that holds no lock while its graph is
      // built never gets here; this only guards against misuse.
      log::debug!("Not taking the package cache download lock: {err:#}");
      None
    }
  }
}

/// The lock file used to guard writes to `target` (see
/// `deno_cache_dir::cache_lock::path_lock_file`), relative to `cwd`.
fn artifact_lock_file(
  deno_dir_root: &Path,
  cwd: &Path,
  target: &Path,
) -> PathBuf {
  let sys = CliSys::default();
  let target =
    deno_cache_dir::cache_lock::normalize_lock_target(&sys, cwd, target);
  deno_cache_dir::cache_lock::path_lock_file(deno_dir_root, &target)
}

/// Locks an output (a file or directory a command writes, resolved against
/// `cwd`) exclusively for as long as the guard lives, so two commands never
/// write the same output at once. `description` says what it is in the
/// "Blocking" message, e.g. "output file".
///
/// Returns `None` when no `DENO_DIR` was resolved at startup; the command
/// then proceeds without the lock.
pub fn lock_artifact(
  cwd: &Path,
  target: &Path,
  description: &str,
) -> Option<ArtifactLockGuard> {
  lock_artifact_with_mode(cwd, target, description, FileLockKind::Exclusive)
}

/// Like [`lock_artifact`], for a command that only reads the output.
pub fn lock_artifact_shared(
  cwd: &Path,
  target: &Path,
  description: &str,
) -> Option<ArtifactLockGuard> {
  lock_artifact_with_mode(cwd, target, description, FileLockKind::Shared)
}

fn lock_artifact_with_mode(
  cwd: &Path,
  target: &Path,
  description: &str,
  mode: FileLockKind,
) -> Option<ArtifactLockGuard> {
  let deno_dir_root = DENO_DIR_ROOT.get()?;
  let lock_file = artifact_lock_file(deno_dir_root, cwd, target);
  let description = format!("{} {}", description, target.display());
  Some(deno_cache_dir::cache_lock::lock_path(
    &CliSys::default(),
    &lock_file,
    mode,
    &description,
    &report_blocking,
  ))
}

#[cfg(test)]
mod test {
  use super::*;
  use crate::args::flags_from_vec;

  fn mode(args: &[&str]) -> Option<CacheLockMode> {
    let mut argv = vec![std::ffi::OsString::from("deno")];
    argv.extend(args.iter().map(std::ffi::OsString::from));
    subcommand_lock_mode(&flags_from_vec(argv).unwrap())
  }

  #[test]
  fn subcommand_lock_modes() {
    assert_eq!(mode(&["clean"]), Some(CacheLockMode::MutateExclusive));
    assert_eq!(
      mode(&["clean", "--except", "main.ts"]),
      Some(CacheLockMode::MutateExclusive)
    );
    for args in [
      &["test"][..],
      &["check", "main.ts"],
      &["cache", "main.ts"],
      &["cache", "--reload", "main.ts"],
      &["compile", "main.ts"],
      &["bundle", "main.ts"],
      &["install"],
      &["info"],
      &["doc", "main.ts"],
      &["coverage", "cov"],
      &["bench"],
      &["publish"],
    ] {
      assert_eq!(mode(args), Some(CacheLockMode::Shared), "{args:?}");
    }
    for args in [
      &["run", "main.ts"][..],
      &["serve", "main.ts"],
      &["eval", "1"],
      &["repl"],
      &["lsp"],
      &["task"],
      &["test", "--watch"],
      &["check", "--watch", "main.ts"],
      &["bundle", "--watch", "--outdir", "out", "main.ts"],
      &["fmt"],
      &["upgrade"],
    ] {
      assert_eq!(mode(args), None, "{args:?}");
    }
  }

  #[test]
  fn artifact_lock_file_is_stable() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let a = artifact_lock_file(dir.path(), dir.path(), Path::new("sub/out"));
    let b = artifact_lock_file(dir.path(), &sub, Path::new("out"));
    let c = artifact_lock_file(dir.path(), &sub, Path::new("other"));
    assert_eq!(a, b);
    assert_ne!(a, c);
  }
}
