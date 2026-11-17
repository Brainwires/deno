// Copyright 2018-2026 the Deno authors. MIT license.

//! The carrier of the inherited kernel policy: how a confined process tells
//! the `deno` processes it starts (children, grandchildren) which grants the
//! kernel holds them to, so their own checks refuse with `NotCapable`
//! instead of meeting a bare `EACCES` from the kernel.
//!
//! The policy travels in a descriptor every child inherits, its number in
//! [`POLICY_ENV_VAR`]:
//!
//! - Linux: a sealed memfd (no writes, no resize, no more seals).
//! - macOS: an unlinked regular file, open read-only, whose writable
//!   descriptor was closed before the first child started (macOS has no
//!   sealing; nothing holds it open for writing).
//!
//! The descriptor is never trusted on its own: the reader
//! ([`read_inherited`]) checks its kind, and the CLI accepts the policy only
//! when the kernel itself reports the process confined
//! ([`kernel_reports_confined`]) and each layer's probe path is refused
//! ([`refuses_read`]). Even then it can only narrow: the CLI intersects its
//! own grant with it, and it never switches a check or a sandbox off.
//!
//! A program between the confined process and a `deno` may drop the carrier:
//! `/usr/bin/env -i` clears the variable (the descriptor stays open, and
//! [`scan_inherited`] finds it), Python's `close_fds` closes the descriptor.
//! The enforcer marks its sandbox ([`kernel_reports_marked`]: a `prctl(2)`
//! option the seccomp program answers on Linux, a sysctl pair the Seatbelt
//! profile tells apart on macOS), so a process with no carrier can still
//! tell that this runtime's sandbox holds it, with grants it can't know; the
//! CLI then refuses every permission. The marker grants nothing, and a
//! process that fakes it only narrows itself.

#![allow(
  clippy::disallowed_methods,
  reason = "the carrier and the probes act on the real file system and the process's own environment"
)]

use std::io;
use std::os::fd::OwnedFd;
use std::path::Path;

/// The internal variable naming the descriptor that carries the inherited
/// policy. Its presence alone changes nothing (see the module docs).
pub const POLICY_ENV_VAR: &str = "DENO_INTERNAL_KERNEL_POLICY";

/// The largest policy a carrier holds (a few layers of permission lists).
pub const MAX_POLICY_BYTES: usize = 4 * 1024 * 1024;

/// A carrier read from an inherited descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Carrier {
  /// The descriptor number (left open: this process's children inherit it).
  pub fd: i32,
  pub bytes: Vec<u8>,
}

/// Creates a carrier holding `bytes`. The descriptor is not close-on-exec:
/// it is the one children inherit.
pub fn seal(bytes: &[u8]) -> io::Result<OwnedFd> {
  if bytes.is_empty() || bytes.len() > MAX_POLICY_BYTES {
    return Err(io::Error::other(format!(
      "a policy of {} bytes can't be carried",
      bytes.len()
    )));
  }
  #[cfg(target_os = "linux")]
  {
    crate::enforce_linux::seal_bytes(c"deno-kernel-policy", bytes)
  }
  #[cfg(target_os = "macos")]
  {
    macos_carrier(bytes)
  }
}

/// macOS: writes `bytes` to a new temporary file, opens it again read-only
/// (not close-on-exec), unlinks it and closes the writable descriptor, so
/// the only descriptor left can't change it.
#[cfg(target_os = "macos")]
fn macos_carrier(bytes: &[u8]) -> io::Result<OwnedFd> {
  use std::ffi::CString;
  use std::os::fd::AsRawFd;
  use std::os::fd::FromRawFd;
  use std::os::unix::ffi::OsStrExt;
  let template = std::env::temp_dir().join("deno-kernel-policy-XXXXXX");
  let Ok(c_template) = CString::new(template.as_os_str().as_bytes()) else {
    return Err(io::Error::other("the temp directory holds a NUL byte"));
  };
  let mut raw = c_template.into_bytes_with_nul();
  // SAFETY: a writable NUL-terminated template
  let fd = unsafe { libc::mkstemp(raw.as_mut_ptr() as *mut libc::c_char) };
  if fd < 0 {
    return Err(io::Error::last_os_error());
  }
  // SAFETY: just created
  let writer = unsafe { OwnedFd::from_raw_fd(fd) };
  let path = raw.as_ptr() as *const libc::c_char;
  let result = (|| {
    let mut written = 0;
    while written < bytes.len() {
      // SAFETY: a valid descriptor and buffer
      let n = unsafe {
        libc::write(
          writer.as_raw_fd(),
          bytes[written..].as_ptr() as *const libc::c_void,
          bytes.len() - written,
        )
      };
      if n < 0 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::Interrupted {
          continue;
        }
        return Err(err);
      }
      if n == 0 {
        return Err(io::Error::other("writing the policy made no progress"));
      }
      written += n as usize;
    }
    // SAFETY: the path mkstemp filled in; read-only, inherited by children
    let reader = unsafe { libc::open(path, libc::O_RDONLY | libc::O_NOFOLLOW) };
    if reader < 0 {
      return Err(io::Error::last_os_error());
    }
    // SAFETY: just opened
    Ok(unsafe { OwnedFd::from_raw_fd(reader) })
  })();
  // SAFETY: the path mkstemp filled in
  unsafe { libc::unlink(path) };
  drop(writer);
  result
}

/// The carrier [`POLICY_ENV_VAR`] names, read without closing it. `Ok(None)`:
/// no variable. `Err`: the variable names something that isn't a carrier
/// (the caller ignores it, with a warning).
pub fn read_inherited() -> Result<Option<Carrier>, String> {
  let Some(value) = std::env::var_os(POLICY_ENV_VAR) else {
    return Ok(None);
  };
  let fd: i32 = value
    .to_str()
    .and_then(|v| v.parse().ok())
    .filter(|fd| *fd > 2)
    .ok_or_else(|| format!("{POLICY_ENV_VAR}={value:?} is not a descriptor"))?;
  read_fd(fd).map(|bytes| Some(Carrier { fd, bytes }))
}

/// Reads the carrier on `fd` (see [`read_inherited`]).
#[cfg(target_os = "linux")]
pub fn read_fd(fd: i32) -> Result<Vec<u8>, String> {
  use std::os::fd::BorrowedFd;
  // SAFETY: checking the descriptor is open before borrowing it
  if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
    return Err(format!("{POLICY_ENV_VAR}: descriptor {fd} is not open"));
  }
  // SAFETY: an open descriptor, borrowed for the read only
  let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
  let bytes = crate::enforce_linux::read_sealed_fd(borrowed)?;
  if bytes.len() > MAX_POLICY_BYTES {
    return Err(format!("the policy carrier has {} bytes", bytes.len()));
  }
  Ok(bytes)
}

/// Reads the carrier on `fd` (see [`read_inherited`]): an unlinked regular
/// file open read-only.
#[cfg(target_os = "macos")]
pub fn read_fd(fd: i32) -> Result<Vec<u8>, String> {
  // SAFETY: checking the descriptor is open
  if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
    return Err(format!("{POLICY_ENV_VAR}: descriptor {fd} is not open"));
  }
  // SAFETY: a valid descriptor
  let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
  if flags < 0 || flags & libc::O_ACCMODE != libc::O_RDONLY {
    return Err("the policy carrier is not open read-only".to_string());
  }
  // SAFETY: a zeroed stat is a valid out parameter
  let mut st: libc::stat = unsafe { std::mem::zeroed() };
  // SAFETY: a valid descriptor and out parameter
  if unsafe { libc::fstat(fd, &mut st) } != 0 {
    return Err(format!("fstat: {}", io::Error::last_os_error()));
  }
  if st.st_mode & libc::S_IFMT != libc::S_IFREG || st.st_nlink != 0 {
    return Err(
      "the policy carrier is not an unlinked regular file".to_string(),
    );
  }
  let size = st.st_size as usize;
  if size == 0 || size > MAX_POLICY_BYTES {
    return Err(format!("the policy carrier has {size} bytes"));
  }
  let mut buf = vec![0u8; size];
  let mut read = 0;
  while read < size {
    // SAFETY: a valid descriptor and buffer
    let n = unsafe {
      libc::pread(
        fd,
        buf[read..].as_mut_ptr() as *mut libc::c_void,
        size - read,
        read as libc::off_t,
      )
    };
    if n < 0 {
      let err = io::Error::last_os_error();
      if err.kind() == io::ErrorKind::Interrupted {
        continue;
      }
      return Err(format!("reading the policy: {err}"));
    }
    if n == 0 {
      return Err("the policy carrier ended early".to_string());
    }
    read += n as usize;
  }
  Ok(buf)
}

/// Whether the kernel reports this process confined (Linux:
/// no-new-privileges and a seccomp filter in `/proc/self/status`, which a
/// confined parent hands every child; macOS: `sandbox_check` says the
/// process is sandboxed). The measured half of accepting a carrier.
pub fn kernel_reports_confined() -> bool {
  #[cfg(target_os = "linux")]
  {
    crate::enforce_linux::status_confined()
  }
  #[cfg(target_os = "macos")]
  {
    crate::macos::is_sandboxed() == Some(true)
  }
}

/// Whether a kernel sandbox of this runtime (`--kernel-sandbox=enforce`)
/// holds this process, told by its marker, which no other sandbox carries
/// (see the module docs). Linux: confined (no-new-privileges and a seccomp
/// filter) and `prctl(MARKER_PRCTL_OPTION)` fails with `MARKER_ERRNO`.
/// macOS: sandboxed, `MARKER_SYSCTL_ALLOWED` readable and
/// `MARKER_SYSCTL_DENIED` refused. Asked only of a process that found no
/// carrier: it decides whether the process runs with no permissions at all,
/// never whether it gets any.
pub fn kernel_reports_marked() -> bool {
  #[cfg(target_os = "linux")]
  {
    use crate::enforce_linux::MARKER_ERRNO;
    use crate::enforce_linux::MARKER_PRCTL_OPTION;
    if !crate::enforce_linux::status_confined() {
      return false;
    }
    // SAFETY: a plain query with no pointers; no kernel defines the option
    let res =
      unsafe { libc::prctl(MARKER_PRCTL_OPTION as libc::c_int, 0, 0, 0, 0) };
    res == -1 && io::Error::last_os_error().raw_os_error() == Some(MARKER_ERRNO)
  }
  #[cfg(target_os = "macos")]
  {
    use crate::seatbelt::MARKER_SYSCTL_ALLOWED;
    use crate::seatbelt::MARKER_SYSCTL_DENIED;
    crate::macos::is_sandboxed() == Some(true)
      && sysctl_readable(MARKER_SYSCTL_ALLOWED) == Some(true)
      && sysctl_readable(MARKER_SYSCTL_DENIED) == Some(false)
  }
}

/// Whether this process may read sysctl `name`: `Some(false)` when the
/// sandbox refuses it (`EPERM`), `None` when it can't be told (no such
/// sysctl).
#[cfg(target_os = "macos")]
fn sysctl_readable(name: &str) -> Option<bool> {
  let name = std::ffi::CString::new(name).ok()?;
  let mut buf = [0u8; 1024];
  let mut len = buf.len();
  // SAFETY: a NUL-terminated name, a buffer and its length; nothing written
  let res = unsafe {
    libc::sysctlbyname(
      name.as_ptr(),
      buf.as_mut_ptr() as *mut libc::c_void,
      &mut len,
      std::ptr::null_mut(),
      0,
    )
  };
  if res == 0 {
    return Some(true);
  }
  match io::Error::last_os_error().raw_os_error() {
    Some(libc::EPERM) => Some(false),
    // the value didn't fit: it was readable
    Some(libc::ENOMEM) => Some(true),
    _ => None,
  }
}

/// The highest descriptor [`scan_inherited`] looks at.
pub const SCAN_MAX_FD: i32 = 1024;

/// The carriers among this process's open descriptors 3 to `max_fd`: for a
/// process whose variable was cleared (`env -i`) while the descriptor
/// stayed open. Each is read as [`read_fd`] reads one (a sealed memfd, an
/// unlinked read-only file, at most [`MAX_POLICY_BYTES`]); whether its
/// bytes are a policy, and whether it measures, the caller decides.
pub fn scan_inherited(max_fd: i32) -> Vec<Carrier> {
  (3..=max_fd)
    .filter(|fd| {
      // SAFETY: asking whether a descriptor is open
      unsafe { libc::fcntl(*fd, libc::F_GETFD) >= 0 }
    })
    .filter_map(|fd| read_fd(fd).ok().map(|bytes| Carrier { fd, bytes }))
    .collect()
}

/// Whether the kernel refuses this process reading `path` (a layer's
/// probe): Linux, opening it fails with `EACCES` / `EPERM` (a missing file
/// doesn't count); macOS, `sandbox_check` says the profile refuses it.
pub fn refuses_read(path: &Path) -> bool {
  #[cfg(target_os = "linux")]
  {
    matches!(
      std::fs::File::open(path),
      Err(err) if err.kind() == io::ErrorKind::PermissionDenied
    )
  }
  // (sandbox_check answers non-zero for a path that doesn't exist, sandbox
  // or not; metadata stays readable under the cli profile)
  #[cfg(target_os = "macos")]
  {
    std::fs::symlink_metadata(path).is_ok()
      && crate::macos::refuses_read(path) == Some(true)
  }
}

#[cfg(test)]
mod tests {
  use std::os::fd::AsRawFd;
  #[cfg(target_os = "linux")]
  use std::os::fd::FromRawFd;

  use super::*;

  #[test]
  fn carrier_round_trip_keeps_the_descriptor_open() {
    let fd = seal(b"{\"layers\":[]}").unwrap();
    let raw = fd.as_raw_fd();
    assert_eq!(read_fd(raw).unwrap(), b"{\"layers\":[]}");
    // read again: still open, same bytes
    assert_eq!(read_fd(raw).unwrap(), b"{\"layers\":[]}");
    // SAFETY: a valid descriptor
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
    assert_eq!(flags & libc::FD_CLOEXEC, 0, "children must inherit it");
  }

  #[test]
  fn carrier_refuses_what_is_not_one() {
    assert!(seal(b"").is_err());
    // a plain file opened for writing is not a carrier
    let dir = std::env::temp_dir()
      .join(format!("deno-inherit-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("policy");
    std::fs::write(&path, b"{}").unwrap();
    let file = std::fs::OpenOptions::new()
      .read(true)
      .write(true)
      .open(&path)
      .unwrap();
    assert!(read_fd(file.as_raw_fd()).is_err());
    // read-only but still linked: not one either
    let file = std::fs::File::open(&path).unwrap();
    assert!(read_fd(file.as_raw_fd()).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(read_fd(1_000_000).is_err());
  }

  #[cfg(target_os = "linux")]
  #[test]
  fn carrier_refuses_an_unsealed_memfd() {
    // SAFETY: a valid name
    let fd = unsafe { libc::memfd_create(c"x".as_ptr(), 0) };
    assert!(fd >= 0);
    // SAFETY: just created
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: a valid descriptor and buffer
    unsafe { libc::write(fd.as_raw_fd(), b"{}".as_ptr() as *const _, 2) };
    assert!(read_fd(fd.as_raw_fd()).unwrap_err().contains("not sealed"));
  }

  #[test]
  fn a_scan_finds_the_carriers_only() {
    let carrier = seal(b"{\"scan\":1}").unwrap();
    // a plain file open read-only is no carrier
    let other = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
    let found = scan_inherited(SCAN_MAX_FD);
    assert!(
      found
        .iter()
        .any(|c| c.fd == carrier.as_raw_fd() && c.bytes == b"{\"scan\":1}"),
      "{found:?}"
    );
    assert!(found.iter().all(|c| c.fd != other.as_raw_fd()));
    // above the limit: not looked at
    assert!(
      scan_inherited(carrier.as_raw_fd() - 1)
        .iter()
        .all(|c| c.fd != carrier.as_raw_fd())
    );
  }

  #[test]
  fn an_unconfined_process_is_not_marked() {
    // a container's own filter may confine the test process; no filter but
    // this runtime's answers the marker
    assert!(!kernel_reports_marked());
  }

  /// Linux: whether a forked child that installs `program` reads as
  /// marked (`None`: this host can't install a filter).
  #[cfg(target_os = "linux")]
  fn marked_under(program: &[u8]) -> Option<bool> {
    use crate::seccomp::SockFilter;
    use crate::seccomp::SockFprog;
    let prog = SockFprog {
      len: (program.len() / 8) as u16,
      filter: program.as_ptr() as *const SockFilter,
    };
    // SAFETY: fork; the child runs only prctl and _exit
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
      // SAFETY: the child's only thread, then _exit
      unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
          || libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            &prog as *const SockFprog,
            0,
            0,
          ) != 0
        {
          libc::_exit(2);
        }
        libc::_exit(if kernel_reports_marked() { 0 } else { 1 });
      }
    }
    let mut status = 0;
    // SAFETY: waiting for the child just forked
    unsafe { libc::waitpid(pid, &mut status, 0) };
    assert!(libc::WIFEXITED(status));
    match libc::WEXITSTATUS(status) {
      2 => None,
      code => Some(code == 0),
    }
  }

  // only this runtime's enforce program carries the marker: a filter of
  // anyone else's (a container's, a service manager's) doesn't
  #[cfg(target_os = "linux")]
  #[test]
  fn the_enforce_filter_is_marked_and_others_are_not() {
    let plan = crate::plan::SeccompPlan::default();
    let enforce =
      crate::enforce_linux::seccomp_spec(&plan, &crate::plan::NetPlan::Open)
        .program();
    let foreign = crate::seccomp::SeccompSpec::new()
      .unix_sockets(true)
      .inet_sockets(crate::seccomp::InetSockets::Any)
      .program();
    if let Some(marked) = marked_under(&enforce) {
      assert!(marked, "the enforce program reads as marked");
      assert_eq!(marked_under(&foreign), Some(false));
    }
  }

  #[test]
  fn an_unconfined_process_refuses_nothing() {
    // the test process runs without a sandbox (a container's own filter
    // may make the kernel report it confined; the probes still open)
    assert!(!refuses_read(Path::new("/")));
    assert!(!refuses_read(Path::new("/no/such/path/here")));
  }
}
