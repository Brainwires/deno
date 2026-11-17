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
  fn an_unconfined_process_refuses_nothing() {
    // the test process runs without a sandbox (a container's own filter
    // may make the kernel report it confined; the probes still open)
    assert!(!refuses_read(Path::new("/")));
    assert!(!refuses_read(Path::new("/no/such/path/here")));
  }
}
