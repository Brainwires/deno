// Copyright 2018-2026 the Deno authors. MIT license.

//! Cross-process locks for `DENO_DIR` and for build artifacts.
//!
//! This follows the rules Cargo uses for its package cache
//! (`cargo::util::cache_lock`) and for its build directories.
//!
//! ## Package cache
//!
//! The package caches in `DENO_DIR` (`remote/`, `npm/`, `registries/`, ...)
//! are guarded by two lock files in `DENO_DIR/locks/`, a "download" one and a
//! "mutate" one, which together give three modes ([`CacheLockMode`]):
//!
//! * [`CacheLockMode::DownloadExclusive`]: held while downloading into the
//!   cache. It excludes other downloaders, but not readers: a download only
//!   adds files (every cache write is a temp file renamed into place), so a
//!   process reading the cache can safely run alongside it.
//! * [`CacheLockMode::Shared`]: held while reading from the cache. Any number
//!   of processes can hold it at once.
//! * [`CacheLockMode::MutateExclusive`]: held while modifying or deleting
//!   existing cache entries (for example `deno clean`). It excludes every
//!   other lock, so nothing is reading a file while it is removed.
//!
//! `Shared` takes the "mutate" lock in shared mode, `DownloadExclusive` takes
//! the "download" lock exclusively, and `MutateExclusive` takes the "mutate"
//! lock exclusively and then the "download" lock exclusively.
//!
//! To avoid deadlocks the locks are always acquired in the same order:
//! "mutate" first, then "download". A process must therefore not ask for a
//! `Shared` or `MutateExclusive` lock while it only holds `DownloadExclusive`
//! (it would be waiting on "mutate" while holding "download", the reverse of
//! a `MutateExclusive` waiter in another process). Such requests are refused
//! with [`CacheLockError`]. Upgrading a held `Shared` lock to
//! `MutateExclusive` is refused too, since two processes doing that at the
//! same time would wait on each other forever.
//!
//! Locks are recursive within a [`CacheLocker`]: asking for a lock that is
//! already held just bumps a count. There should be one `CacheLocker` per
//! `DENO_DIR` per process.
//!
//! ## Artifacts
//!
//! [`lock_path`] takes a lock on an output path (a `deno compile` binary, a
//! `deno test --coverage` directory, a `deno.lock`, ...) for as long as a
//! command produces it. The lock file lives in `DENO_DIR/locks/` rather than
//! next to the output, so it never ends up in a published directory.
//!
//! ## Mechanism
//!
//! These are OS file locks (`flock` on Unix, `LockFileEx` on Windows). They
//! are released by the OS when a process exits, however it exits, so unlike
//! the `node_modules` lock there is no fallback that lets a waiter proceed
//! without the lock. A lock attempt first tries without blocking; when the
//! lock is taken, the caller's `on_blocking` callback is told what is being
//! waited on (Cargo's "Blocking waiting for file lock on package cache") and
//! the attempt then blocks.
//!
//! Locking is best effort: when a lock file cannot be created or locked (a
//! read-only `DENO_DIR`, a file system without lock support), the operation
//! proceeds without the lock, the same as Cargo.

use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;

use parking_lot::Mutex;
use sha2::Digest;
use sys_traits::FsCreateDirAll;
use sys_traits::FsFileLock;
use sys_traits::FsOpen;
use sys_traits::OpenOptions;

/// The directory inside `DENO_DIR` that holds the lock files. `deno clean`
/// must leave it in place: deleting a lock file that another process holds
/// would let a third process lock a new file at the same path.
pub const LOCKS_DIR_NAME: &str = "locks";

const DOWNLOAD_LOCK_FILE_NAME: &str = "package-cache-download.lock";
const MUTATE_LOCK_FILE_NAME: &str = "package-cache-mutate.lock";
const ARTIFACTS_DIR_NAME: &str = "artifacts";

const SHARED_DESCRIPTION: &str = "shared package cache";
const DOWNLOAD_EXCLUSIVE_DESCRIPTION: &str = "package cache";
const MUTATE_EXCLUSIVE_DESCRIPTION: &str = "package cache mutation";

/// The style of package cache lock to acquire. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheLockMode {
  /// Only one process downloads into the cache at a time. Does not block
  /// `Shared` holders.
  DownloadExclusive,
  /// Any number of processes read from the cache. Blocks only
  /// `MutateExclusive`.
  Shared,
  /// One process modifies or deletes existing cache entries while no other
  /// process holds any lock.
  MutateExclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CacheLockError {
  #[error(
    "a {0:?} package cache lock was requested while only the download lock is held, which would acquire the locks out of order"
  )]
  OutOfOrder(CacheLockMode),
  #[error(
    "upgrading a shared package cache lock to an exclusive one is not supported"
  )]
  Upgrade,
}

/// Whether a lock is shared or exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileLockKind {
  Shared,
  Exclusive,
}

impl FileLockKind {
  fn to_sys(self) -> sys_traits::FsFileLockMode {
    match self {
      Self::Shared => sys_traits::FsFileLockMode::Shared,
      Self::Exclusive => sys_traits::FsFileLockMode::Exclusive,
    }
  }
}

/// Called with a description of what is being waited on (for example
/// "package cache") right before a lock attempt blocks.
pub type OnBlocking<'a> = &'a (dyn Fn(&str) + Send + Sync);

#[derive(Clone, Copy)]
enum Blocking<'a> {
  Yes(OnBlocking<'a>),
  No,
}

enum LockOutcome<F> {
  /// The lock is held through this file.
  Locked(F),
  /// The lock file could not be opened or locked for a reason other than
  /// contention. The caller proceeds as if it held the lock.
  Unavailable,
  /// Another process holds the lock (non-blocking attempts only).
  WouldBlock,
}

fn is_would_block(err: &std::io::Error) -> bool {
  if err.kind() == ErrorKind::WouldBlock {
    return true;
  }
  // `LockFileEx` with `LOCKFILE_FAIL_IMMEDIATELY` fails with
  // ERROR_LOCK_VIOLATION, which std does not map to `WouldBlock`.
  #[cfg(windows)]
  if err.raw_os_error() == Some(33) {
    return true;
  }
  false
}

fn open_lock_file<TSys: FsOpen + FsCreateDirAll>(
  sys: &TSys,
  path: &Path,
) -> std::io::Result<TSys::File> {
  if let Some(parent) = path.parent() {
    // ignore the error; opening the file reports anything that matters
    let _ = sys.fs_create_dir_all(parent);
  }
  let mut options = OpenOptions::new();
  options.read = true;
  options.write = true;
  options.create = true;
  match sys.fs_open(path, &options) {
    Ok(file) => Ok(file),
    Err(err) => {
      // A read-only DENO_DIR may still have the lock file from a process
      // that could write it; locking works on a read-only handle too.
      sys.fs_open(path, &OpenOptions::new_read()).map_err(|_| err)
    }
  }
}

fn open_and_lock<TSys: FsOpen + FsCreateDirAll>(
  sys: &TSys,
  path: &Path,
  mode: FileLockKind,
  description: &str,
  blocking: Blocking<'_>,
) -> LockOutcome<TSys::File> {
  let mut file = match open_lock_file(sys, path) {
    Ok(file) => file,
    Err(err) => {
      log::debug!(
        "Failed to open file lock at {}, continuing without it. {:#}",
        path.display(),
        err
      );
      return LockOutcome::Unavailable;
    }
  };
  match file.fs_file_try_lock(mode.to_sys()) {
    Ok(()) => {
      log::trace!("Acquired file lock at {} ({:?})", path.display(), mode);
      LockOutcome::Locked(file)
    }
    Err(err) if is_would_block(&err) => match blocking {
      Blocking::No => LockOutcome::WouldBlock,
      Blocking::Yes(on_blocking) => {
        on_blocking(description);
        match file.fs_file_lock(mode.to_sys()) {
          Ok(()) => {
            log::trace!(
              "Acquired file lock at {} ({:?}) after waiting",
              path.display(),
              mode
            );
            LockOutcome::Locked(file)
          }
          Err(err) => {
            log::debug!(
              "Failed to lock {}, continuing without the lock. {:#}",
              path.display(),
              err
            );
            LockOutcome::Unavailable
          }
        }
      }
    },
    Err(err) => {
      log::debug!(
        "File locking is not available for {}, continuing without the lock. {:#}",
        path.display(),
        err
      );
      LockOutcome::Unavailable
    }
  }
}

/// A file lock with a count, so it can be acquired recursively.
struct RecursiveLock<F: FsFileLock> {
  path: PathBuf,
  /// `None` while unlocked, and also while "locked" when the lock file was
  /// unavailable (see [`LockOutcome::Unavailable`]); `count` is what tracks
  /// whether the lock is held.
  file: Option<F>,
  count: u32,
  is_exclusive: bool,
}

impl<F: FsFileLock> RecursiveLock<F> {
  fn new(path: PathBuf) -> Self {
    Self {
      path,
      file: None,
      count: 0,
      is_exclusive: false,
    }
  }

  /// Returns `false` when the attempt was non-blocking and the lock is held
  /// by another process.
  fn lock<TSys: FsOpen<File = F> + FsCreateDirAll>(
    &mut self,
    sys: &TSys,
    mode: FileLockKind,
    description: &str,
    blocking: Blocking<'_>,
  ) -> bool {
    if self.count == 0 {
      match open_and_lock(sys, &self.path, mode, description, blocking) {
        LockOutcome::Locked(file) => self.file = Some(file),
        LockOutcome::Unavailable => self.file = None,
        LockOutcome::WouldBlock => return false,
      }
      self.is_exclusive = mode == FileLockKind::Exclusive;
    }
    self.count += 1;
    true
  }

  fn unlock(&mut self) {
    debug_assert!(self.count > 0);
    self.count = self.count.saturating_sub(1);
    if self.count == 0
      && let Some(mut file) = self.file.take()
    {
      // closing the file releases the lock as well; this is explicit
      if let Err(err) = file.fs_file_unlock() {
        log::debug!(
          "Failed releasing file lock at {}. {:#}",
          self.path.display(),
          err
        );
      }
    }
  }
}

struct CacheState<F: FsFileLock> {
  /// Exclusive while downloading (and as part of `MutateExclusive`).
  download: RecursiveLock<F>,
  /// Shared while reading, exclusive while mutating.
  mutate: RecursiveLock<F>,
}

/// Coordinates access to the package caches in a `DENO_DIR` between
/// processes. See the module docs for the rules.
pub struct CacheLocker<TSys: FsOpen> {
  sys: TSys,
  on_blocking: Box<dyn Fn(&str) + Send + Sync>,
  state: Mutex<CacheState<TSys::File>>,
}

impl<TSys: FsOpen> std::fmt::Debug for CacheLocker<TSys> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("CacheLocker").finish_non_exhaustive()
  }
}

impl<TSys: FsOpen + FsCreateDirAll> CacheLocker<TSys> {
  /// Creates a locker for the package caches of the `DENO_DIR` at
  /// `deno_dir_root`. Nothing is created on disk until a lock is acquired.
  pub fn new(
    sys: TSys,
    deno_dir_root: &Path,
    on_blocking: Box<dyn Fn(&str) + Send + Sync>,
  ) -> Self {
    let locks_dir = deno_dir_root.join(LOCKS_DIR_NAME);
    Self {
      sys,
      on_blocking,
      state: Mutex::new(CacheState {
        download: RecursiveLock::new(locks_dir.join(DOWNLOAD_LOCK_FILE_NAME)),
        mutate: RecursiveLock::new(locks_dir.join(MUTATE_LOCK_FILE_NAME)),
      }),
    }
  }

  /// Acquires a lock, blocking while another process holds a conflicting
  /// one. The lock is released when the returned guard is dropped.
  pub fn lock(
    &self,
    mode: CacheLockMode,
  ) -> Result<CacheLockGuard<'_, TSys>, CacheLockError> {
    let acquired = self.lock_inner(mode, Blocking::Yes(&*self.on_blocking))?;
    debug_assert!(acquired);
    Ok(CacheLockGuard { locker: self, mode })
  }

  /// Acquires a lock without blocking. Returns `Ok(None)` when another
  /// process holds a conflicting lock.
  pub fn try_lock(
    &self,
    mode: CacheLockMode,
  ) -> Result<Option<CacheLockGuard<'_, TSys>>, CacheLockError> {
    if self.lock_inner(mode, Blocking::No)? {
      Ok(Some(CacheLockGuard { locker: self, mode }))
    } else {
      Ok(None)
    }
  }

  /// Whether this process holds a lock that satisfies `mode`.
  pub fn is_locked(&self, mode: CacheLockMode) -> bool {
    let state = self.state.lock();
    match mode {
      CacheLockMode::Shared => state.mutate.count > 0,
      CacheLockMode::DownloadExclusive => state.download.count > 0,
      CacheLockMode::MutateExclusive => {
        state.mutate.count > 0
          && state.mutate.is_exclusive
          && state.download.count > 0
      }
    }
  }

  fn lock_inner(
    &self,
    mode: CacheLockMode,
    blocking: Blocking<'_>,
  ) -> Result<bool, CacheLockError> {
    let mut state = self.state.lock();
    let state = &mut *state;
    match mode {
      CacheLockMode::Shared => {
        if state.download.count > 0 && state.mutate.count == 0 {
          return Err(CacheLockError::OutOfOrder(mode));
        }
        Ok(state.mutate.lock(
          &self.sys,
          FileLockKind::Shared,
          SHARED_DESCRIPTION,
          blocking,
        ))
      }
      CacheLockMode::DownloadExclusive => Ok(state.download.lock(
        &self.sys,
        FileLockKind::Exclusive,
        DOWNLOAD_EXCLUSIVE_DESCRIPTION,
        blocking,
      )),
      CacheLockMode::MutateExclusive => {
        if state.mutate.count > 0 && !state.mutate.is_exclusive {
          return Err(CacheLockError::Upgrade);
        }
        if state.download.count > 0 && state.mutate.count == 0 {
          return Err(CacheLockError::OutOfOrder(mode));
        }
        // "mutate" first, then "download"
        if !state.mutate.lock(
          &self.sys,
          FileLockKind::Exclusive,
          MUTATE_EXCLUSIVE_DESCRIPTION,
          blocking,
        ) {
          return Ok(false);
        }
        if !state.download.lock(
          &self.sys,
          FileLockKind::Exclusive,
          MUTATE_EXCLUSIVE_DESCRIPTION,
          blocking,
        ) {
          state.mutate.unlock();
          return Ok(false);
        }
        Ok(true)
      }
    }
  }

  fn unlock(&self, mode: CacheLockMode) {
    let mut state = self.state.lock();
    match mode {
      CacheLockMode::Shared => state.mutate.unlock(),
      CacheLockMode::DownloadExclusive => state.download.unlock(),
      CacheLockMode::MutateExclusive => {
        // reverse of the acquisition order
        state.download.unlock();
        state.mutate.unlock();
      }
    }
  }
}

/// Releases its package cache lock when dropped.
#[must_use = "the lock is released when the guard is dropped"]
pub struct CacheLockGuard<'a, TSys: FsOpen + FsCreateDirAll> {
  locker: &'a CacheLocker<TSys>,
  mode: CacheLockMode,
}

impl<TSys: FsOpen + FsCreateDirAll> CacheLockGuard<'_, TSys> {
  pub fn mode(&self) -> CacheLockMode {
    self.mode
  }
}

impl<TSys: FsOpen + FsCreateDirAll> std::fmt::Debug
  for CacheLockGuard<'_, TSys>
{
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("CacheLockGuard")
      .field("mode", &self.mode)
      .finish()
  }
}

impl<TSys: FsOpen + FsCreateDirAll> Drop for CacheLockGuard<'_, TSys> {
  fn drop(&mut self) {
    self.locker.unlock(self.mode);
  }
}

/// The lock file that guards `target`: a file in `DENO_DIR/locks/artifacts/`
/// named after a hash of the target's path, so the same output reached
/// through different relative paths maps to the same lock.
///
/// `target` should be absolute; pass it through [`normalize_lock_target`]
/// first when it may not exist yet or may go through a symlink.
pub fn path_lock_file(deno_dir_root: &Path, target: &Path) -> PathBuf {
  let hash = sha2::Sha256::digest(target.to_string_lossy().as_bytes());
  let mut name = String::with_capacity(36);
  for byte in &hash[..16] {
    name.push_str(&format!("{byte:02x}"));
  }
  name.push_str(".lock");
  deno_dir_root
    .join(LOCKS_DIR_NAME)
    .join(ARTIFACTS_DIR_NAME)
    .join(name)
}

/// Makes `target` absolute against `cwd`, resolves `.`/`..` lexically, and
/// canonicalizes the parent directory when it exists (the target itself may
/// not exist yet), so that different spellings of one output agree.
pub fn normalize_lock_target<TSys: sys_traits::FsCanonicalize>(
  sys: &TSys,
  cwd: &Path,
  target: &Path,
) -> PathBuf {
  let target =
    deno_path_util::normalize_path(std::borrow::Cow::Owned(cwd.join(target)))
      .into_owned();
  match (target.parent(), target.file_name()) {
    (Some(parent), Some(name)) => match sys.fs_canonicalize(parent) {
      Ok(parent) => parent.join(name),
      Err(_) => target,
    },
    _ => target,
  }
}

/// A held lock on an output path. Released when dropped.
#[must_use = "the lock is released when the guard is dropped"]
pub struct PathLockGuard<F: FsFileLock> {
  path: PathBuf,
  file: Option<F>,
}

impl<F: FsFileLock> std::fmt::Debug for PathLockGuard<F> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("PathLockGuard")
      .field("path", &self.path)
      .finish()
  }
}

impl<F: FsFileLock> Drop for PathLockGuard<F> {
  fn drop(&mut self) {
    if let Some(mut file) = self.file.take()
      && let Err(err) = file.fs_file_unlock()
    {
      log::debug!(
        "Failed releasing file lock at {}. {:#}",
        self.path.display(),
        err
      );
    }
  }
}

/// Locks `lock_file` (see [`path_lock_file`]) in `mode`, blocking while
/// another process holds a conflicting lock. `description` names what is
/// being waited on for `on_blocking`, e.g. "build directory".
///
/// The lock is best effort: when it cannot be taken for a reason other than
/// contention, the returned guard holds nothing.
pub fn lock_path<TSys: FsOpen + FsCreateDirAll>(
  sys: &TSys,
  lock_file: &Path,
  mode: FileLockKind,
  description: &str,
  on_blocking: OnBlocking<'_>,
) -> PathLockGuard<TSys::File> {
  let file = match open_and_lock(
    sys,
    lock_file,
    mode,
    description,
    Blocking::Yes(on_blocking),
  ) {
    LockOutcome::Locked(file) => Some(file),
    LockOutcome::Unavailable | LockOutcome::WouldBlock => None,
  };
  PathLockGuard {
    path: lock_file.to_path_buf(),
    file,
  }
}

/// Like [`lock_path`], but returns `None` instead of blocking when another
/// process holds a conflicting lock.
pub fn try_lock_path<TSys: FsOpen + FsCreateDirAll>(
  sys: &TSys,
  lock_file: &Path,
  mode: FileLockKind,
) -> Option<PathLockGuard<TSys::File>> {
  match open_and_lock(sys, lock_file, mode, "", Blocking::No) {
    LockOutcome::Locked(file) => Some(PathLockGuard {
      path: lock_file.to_path_buf(),
      file: Some(file),
    }),
    LockOutcome::Unavailable => Some(PathLockGuard {
      path: lock_file.to_path_buf(),
      file: None,
    }),
    LockOutcome::WouldBlock => None,
  }
}

#[cfg(test)]
mod test {
  use std::sync::atomic::AtomicUsize;
  use std::sync::atomic::Ordering;

  use sys_traits::FsWrite;
  use sys_traits::impls::RealSys;

  use super::*;

  // Two lockers on the same DENO_DIR stand in for two processes: `flock` and
  // `LockFileEx` locks belong to the open file, so two handles in one process
  // conflict the same way two processes do.
  fn locker(root: &Path) -> CacheLocker<RealSys> {
    CacheLocker::new(RealSys, root, Box::new(|_| {}))
  }

  fn can_lock(locker: &CacheLocker<RealSys>, mode: CacheLockMode) -> bool {
    locker.try_lock(mode).unwrap().is_some()
  }

  #[test]
  fn shared_and_shared_are_concurrent() {
    let dir = tempfile::tempdir().unwrap();
    let a = locker(dir.path());
    let b = locker(dir.path());
    let _a = a.lock(CacheLockMode::Shared).unwrap();
    assert!(can_lock(&b, CacheLockMode::Shared));
  }

  #[test]
  fn download_and_shared_are_concurrent() {
    let dir = tempfile::tempdir().unwrap();
    let a = locker(dir.path());
    let b = locker(dir.path());
    let _a = a.lock(CacheLockMode::DownloadExclusive).unwrap();
    assert!(can_lock(&b, CacheLockMode::Shared));
    // ...and the other way around
    let c = locker(dir.path());
    let d = locker(dir.path());
    let _c = c.lock(CacheLockMode::Shared).unwrap();
    drop(_a);
    assert!(can_lock(&d, CacheLockMode::DownloadExclusive));
  }

  #[test]
  fn download_excludes_download() {
    let dir = tempfile::tempdir().unwrap();
    let a = locker(dir.path());
    let b = locker(dir.path());
    let guard = a.lock(CacheLockMode::DownloadExclusive).unwrap();
    assert!(!can_lock(&b, CacheLockMode::DownloadExclusive));
    drop(guard);
    assert!(can_lock(&b, CacheLockMode::DownloadExclusive));
  }

  #[test]
  fn mutate_excludes_everything() {
    let dir = tempfile::tempdir().unwrap();
    let a = locker(dir.path());
    let b = locker(dir.path());
    let guard = a.lock(CacheLockMode::MutateExclusive).unwrap();
    assert!(!can_lock(&b, CacheLockMode::Shared));
    assert!(!can_lock(&b, CacheLockMode::DownloadExclusive));
    assert!(!can_lock(&b, CacheLockMode::MutateExclusive));
    drop(guard);
    assert!(can_lock(&b, CacheLockMode::Shared));
    assert!(can_lock(&b, CacheLockMode::DownloadExclusive));
  }

  #[test]
  fn mutate_waits_for_shared_and_download() {
    let dir = tempfile::tempdir().unwrap();
    let a = locker(dir.path());
    let b = locker(dir.path());

    let shared = a.lock(CacheLockMode::Shared).unwrap();
    assert!(!can_lock(&b, CacheLockMode::MutateExclusive));
    drop(shared);

    let download = a.lock(CacheLockMode::DownloadExclusive).unwrap();
    assert!(!can_lock(&b, CacheLockMode::MutateExclusive));
    // the failed attempt got "mutate" before finding "download" taken; it
    // must have released "mutate" again, or readers would now be blocked
    let c = locker(dir.path());
    assert!(can_lock(&c, CacheLockMode::Shared));
    drop(download);

    assert!(can_lock(&b, CacheLockMode::MutateExclusive));
  }

  #[test]
  fn locks_are_recursive() {
    let dir = tempfile::tempdir().unwrap();
    let a = locker(dir.path());
    let b = locker(dir.path());

    let outer = a.lock(CacheLockMode::DownloadExclusive).unwrap();
    let inner = a.lock(CacheLockMode::DownloadExclusive).unwrap();
    drop(outer);
    assert!(a.is_locked(CacheLockMode::DownloadExclusive));
    assert!(!can_lock(&b, CacheLockMode::DownloadExclusive));
    drop(inner);
    assert!(!a.is_locked(CacheLockMode::DownloadExclusive));
    assert!(can_lock(&b, CacheLockMode::DownloadExclusive));

    // MutateExclusive covers the other modes for the same locker
    let mutate = a.lock(CacheLockMode::MutateExclusive).unwrap();
    let shared = a.lock(CacheLockMode::Shared).unwrap();
    let download = a.lock(CacheLockMode::DownloadExclusive).unwrap();
    drop(mutate);
    drop(shared);
    assert!(!can_lock(&b, CacheLockMode::DownloadExclusive));
    drop(download);
    assert!(can_lock(&b, CacheLockMode::MutateExclusive));
  }

  #[test]
  fn shared_then_download_is_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let a = locker(dir.path());
    let _shared = a.lock(CacheLockMode::Shared).unwrap();
    let _download = a.lock(CacheLockMode::DownloadExclusive).unwrap();
    assert!(a.is_locked(CacheLockMode::Shared));
    assert!(a.is_locked(CacheLockMode::DownloadExclusive));
  }

  #[test]
  fn refuses_out_of_order_and_upgrades() {
    let dir = tempfile::tempdir().unwrap();
    let a = locker(dir.path());
    {
      let _download = a.lock(CacheLockMode::DownloadExclusive).unwrap();
      assert_eq!(
        a.lock(CacheLockMode::Shared).unwrap_err(),
        CacheLockError::OutOfOrder(CacheLockMode::Shared)
      );
      assert_eq!(
        a.lock(CacheLockMode::MutateExclusive).unwrap_err(),
        CacheLockError::OutOfOrder(CacheLockMode::MutateExclusive)
      );
    }
    {
      let _shared = a.lock(CacheLockMode::Shared).unwrap();
      assert_eq!(
        a.lock(CacheLockMode::MutateExclusive).unwrap_err(),
        CacheLockError::Upgrade
      );
    }
    // a refused request leaves nothing behind
    assert!(!a.is_locked(CacheLockMode::Shared));
    assert!(!a.is_locked(CacheLockMode::DownloadExclusive));
    assert!(can_lock(
      &locker(dir.path()),
      CacheLockMode::MutateExclusive
    ));
  }

  #[test]
  fn blocking_lock_reports_and_waits() {
    let dir = tempfile::tempdir().unwrap();
    let holder = locker(dir.path());
    let guard = holder.lock(CacheLockMode::MutateExclusive).unwrap();

    static REPORTED: AtomicUsize = AtomicUsize::new(0);
    let root = dir.path().to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
      let locker = CacheLocker::new(
        RealSys,
        &root,
        Box::new(move |description| {
          assert_eq!(description, "shared package cache");
          REPORTED.fetch_add(1, Ordering::SeqCst);
          tx.send(()).unwrap();
        }),
      );
      let _guard = locker.lock(CacheLockMode::Shared).unwrap();
    });
    // the waiter reports that it is blocking, then waits for the release
    rx.recv().unwrap();
    assert!(!waiter.is_finished());
    drop(guard);
    waiter.join().unwrap();
    assert_eq!(REPORTED.load(Ordering::SeqCst), 1);
  }

  #[test]
  fn uncontended_lock_does_not_report() {
    let dir = tempfile::tempdir().unwrap();
    let locker = CacheLocker::new(
      RealSys,
      dir.path(),
      Box::new(|description| panic!("unexpected blocking on {description}")),
    );
    let _guard = locker.lock(CacheLockMode::MutateExclusive).unwrap();
  }

  #[test]
  fn unwritable_deno_dir_proceeds_without_lock() {
    let dir = tempfile::tempdir().unwrap();
    // a file where the DENO_DIR should be: nothing can be created inside
    let root = dir.path().join("not_a_dir");
    RealSys.fs_write(&root, "").unwrap();
    let a = locker(&root);
    let b = locker(&root);
    let _a = a.lock(CacheLockMode::MutateExclusive).unwrap();
    // neither side holds a real lock, so neither blocks
    assert!(can_lock(&b, CacheLockMode::MutateExclusive));
  }

  #[test]
  fn path_locks_exclude_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    RealSys.fs_create_dir_all(&out).unwrap();
    let a = normalize_lock_target(&RealSys, dir.path(), Path::new("out/bin"));
    let b =
      normalize_lock_target(&RealSys, &out, Path::new("../out/./sub/../bin"));
    assert_eq!(a, b);
    let lock_file = path_lock_file(dir.path(), &a);
    assert!(lock_file.starts_with(dir.path().join("locks").join("artifacts")));

    let guard = lock_path(
      &RealSys,
      &lock_file,
      FileLockKind::Exclusive,
      "build directory",
      &|_| panic!("should not block"),
    );
    assert!(
      try_lock_path(&RealSys, &lock_file, FileLockKind::Exclusive).is_none()
    );
    assert!(
      try_lock_path(&RealSys, &lock_file, FileLockKind::Shared).is_none()
    );
    drop(guard);
    let shared =
      try_lock_path(&RealSys, &lock_file, FileLockKind::Shared).unwrap();
    assert!(
      try_lock_path(&RealSys, &lock_file, FileLockKind::Shared).is_some()
    );
    assert!(
      try_lock_path(&RealSys, &lock_file, FileLockKind::Exclusive).is_none()
    );
    drop(shared);

    // a different output does not contend
    let other = path_lock_file(
      dir.path(),
      &normalize_lock_target(&RealSys, dir.path(), Path::new("out/other")),
    );
    let _guard = lock_path(
      &RealSys,
      &lock_file,
      FileLockKind::Exclusive,
      "build directory",
      &|_| panic!("should not block"),
    );
    assert!(try_lock_path(&RealSys, &other, FileLockKind::Exclusive).is_some());
  }
}
