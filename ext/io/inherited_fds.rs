// Copyright 2018-2026 the Deno authors. MIT license.

//! Descriptors the process inherited from its parent (fd 3 and up).
//!
//! A descriptor that was already open when the process started belongs to
//! the process the same way stdin, stdout and stderr do: a service manager's
//! socket activation (`server.listen({ fd: 3 })`) or a pipe a parent passes
//! on fd 3 are the common cases. Node adopts such fds with `net.Socket({ fd })`
//! / `tty.ReadStream(fd)` / `server.listen({ fd })`, and so do the libuv
//! wraps here, each isolate (a worker too) holding the permission the
//! descriptor's kind implies (`ext/node/ops/fd_adopt.rs`).
//!
//! The set is captured once, at the very start of `main`, before the runtime
//! opens any descriptor of its own. Each entry remembers the descriptor's
//! device and inode, so a number that is closed and later reused by the
//! runtime (a cache database, a module file) never counts as inherited.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;

/// Device and inode of an inherited descriptor.
type FdIdentity = (u64, u64);

static INHERITED_FDS: OnceLock<Mutex<HashMap<i32, FdIdentity>>> =
  OnceLock::new();

#[cfg(unix)]
fn fd_identity(fd: i32) -> Option<FdIdentity> {
  // SAFETY: zeroed `stat` is a valid out-parameter for fstat.
  let mut st: libc::stat = unsafe { std::mem::zeroed() };
  // SAFETY: fstat only writes to `st`; an invalid fd returns -1.
  if unsafe { libc::fstat(fd, &mut st) } != 0 {
    return None;
  }
  #[allow(
    clippy::unnecessary_cast,
    reason = "dev_t / ino_t widths differ by platform"
  )]
  Some((st.st_dev as u64, st.st_ino as u64))
}

#[cfg(unix)]
fn list_open_fds() -> Vec<i32> {
  // /dev/fd (macOS, the BSDs) and /proc/self/fd (Linux) list only open
  // descriptors, so this costs one syscall per open fd instead of one per
  // possible fd up to RLIMIT_NOFILE (which can be over a million).
  #[cfg(target_os = "linux")]
  const DIR: &std::ffi::CStr = c"/proc/self/fd";
  #[cfg(not(target_os = "linux"))]
  const DIR: &std::ffi::CStr = c"/dev/fd";

  let mut fds = Vec::new();
  // SAFETY: DIR is a NUL-terminated path.
  let dir = unsafe { libc::opendir(DIR.as_ptr()) };
  if dir.is_null() {
    // No fd listing: probe a bounded range instead.
    for fd in 3..1024 {
      // SAFETY: F_GETFD on any number is safe; an invalid fd returns -1.
      if unsafe { libc::fcntl(fd, libc::F_GETFD) } != -1 {
        fds.push(fd);
      }
    }
    return fds;
  }
  // SAFETY: dir is a valid DIR* returned by opendir.
  let dir_fd = unsafe { libc::dirfd(dir) };
  loop {
    // SAFETY: dir is valid until closedir below.
    let entry = unsafe { libc::readdir(dir) };
    if entry.is_null() {
      break;
    }
    // SAFETY: d_name is a NUL-terminated name inside the dirent.
    let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
    if let Some(fd) = name.to_str().ok().and_then(|s| s.parse::<i32>().ok())
      && fd > 2
      && fd != dir_fd
    {
      fds.push(fd);
    }
  }
  // SAFETY: dir is a valid DIR* that has not been closed yet.
  unsafe { libc::closedir(dir) };
  fds
}

/// Record the descriptors (fd 3 and up) open at process start. Call it first
/// thing in `main`, before anything opens a file or socket. Descriptors named
/// by `exclude_env_vars` (fds the runtime itself consumes, such as the Node
/// IPC channel) are left out. Later calls do nothing.
pub fn capture_inherited_fds(exclude_env_vars: &[&str]) {
  #[cfg(unix)]
  {
    if INHERITED_FDS.get().is_some() {
      return;
    }
    let excluded: Vec<i32> = exclude_env_vars
      .iter()
      .filter_map(|name| std::env::var(name).ok())
      .filter_map(|value| value.trim().parse::<i32>().ok())
      .collect();
    let mut map = HashMap::new();
    for fd in list_open_fds() {
      if excluded.contains(&fd) {
        continue;
      }
      if let Some(identity) = fd_identity(fd) {
        map.insert(fd, identity);
      }
    }
    let _ = INHERITED_FDS.set(Mutex::new(map));
  }
  #[cfg(not(unix))]
  {
    let _ = exclude_env_vars;
    let _ = INHERITED_FDS.set(Mutex::new(HashMap::new()));
  }
}

#[cfg(unix)]
fn still_inherited(map: &HashMap<i32, FdIdentity>, fd: i32) -> bool {
  map
    .get(&fd)
    .is_some_and(|identity| fd_identity(fd) == Some(*identity))
}

/// Whether `fd` is still the descriptor the process inherited under that
/// number (same device and inode as at startup).
pub fn is_inherited_fd(fd: i32) -> bool {
  #[cfg(unix)]
  {
    let Some(fds) = INHERITED_FDS.get() else {
      return false;
    };
    still_inherited(&fds.lock().unwrap(), fd)
  }
  #[cfg(not(unix))]
  {
    let _ = fd;
    false
  }
}

/// Claim an inherited descriptor for a libuv wrap. Succeeds once per fd in
/// the whole process (across workers), so two wraps never own one fd. Give
/// the claim back with [`unclaim_inherited_fd`] if the wrap fails to open it.
pub fn claim_inherited_fd(fd: i32) -> bool {
  if !is_inherited_fd(fd) {
    return false;
  }
  INHERITED_FDS
    .get()
    .is_some_and(|fds| fds.lock().unwrap().remove(&fd).is_some())
}

/// Return a claim taken with [`claim_inherited_fd`] after a failed open.
pub fn unclaim_inherited_fd(fd: i32) {
  #[cfg(unix)]
  if let (Some(fds), Some(identity)) = (INHERITED_FDS.get(), fd_identity(fd)) {
    fds.lock().unwrap().insert(fd, identity);
  }
  #[cfg(not(unix))]
  let _ = fd;
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;

  #[test]
  fn identity_guards_reused_numbers() {
    use std::os::fd::AsRawFd;
    let (r, w) = std::io::pipe().unwrap();
    let fd = r.as_raw_fd();
    let mut map = HashMap::new();
    map.insert(fd, fd_identity(fd).unwrap());
    assert!(still_inherited(&map, fd));
    drop((r, w));
    assert!(!still_inherited(&map, fd));
    // Another file under the same number is not the inherited one.
    let other = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
    if other.as_raw_fd() == fd {
      assert!(!still_inherited(&map, fd));
    }
  }

  #[test]
  fn lists_open_descriptors() {
    let (r, _w) = std::io::pipe().unwrap();
    use std::os::fd::AsRawFd;
    assert!(list_open_fds().contains(&r.as_raw_fd()));
  }
}
