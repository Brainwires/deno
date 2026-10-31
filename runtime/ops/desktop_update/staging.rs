// Copyright 2018-2026 the Deno authors. MIT license.

//! The staging directory, private to this user and held open.
//!
//! The staging directory (`.<name>.denext-update/`) must sit next to the
//! install, on the same file system, so the swap can be one rename. That
//! parent may be writable by other users: `/Applications` is writable by the
//! whole `admin` group, a shared `/opt` or a group folder by others. Anyone
//! who can write the parent can rename the staging directory away and put
//! another tree at its path, at any moment: between the check of the staged
//! app and the swap, the check would have passed for one tree and the swap
//! would have moved another (5.0-gate review A, L1).
//!
//! So on Unix the staging directory is created fresh with mode 0700 (nobody
//! but this user can enter it, let alone change what is in it), and each step
//! that reads or moves the staged app does it through a directory descriptor
//! opened once ([`PrivateDir::open`]: not through a symlink, a directory this
//! user owns that no other user can read or write), not through the path:
//! the digest is computed by `openat` / `fstatat` / `readlinkat` from that
//! descriptor ([`PrivateDir::digest`]) and the swap renames from it
//! (`renameatx_np` / `renameat2` / `renameat`, [`Staged`]). A staging
//! directory renamed away and replaced is then simply not used: the staged
//! app that moves is the one in the directory this process holds. The OS
//! code-signature check still takes a path (`codesign`), so the path is
//! checked to name the held directory right before and after it
//! ([`PrivateDir::still_at_path`]).
//!
//! Windows has no such descriptor-relative calls in std; there the staging
//! directory is refused only when it is a reparse point (a symlink or a
//! junction), and who may write it is left to the install directory's ACL
//! (a per-user install under `%LOCALAPPDATA%` is private to the user; a
//! per-machine one under `Program Files` is not writable by users at all).

#![allow(
  clippy::disallowed_methods,
  reason = "the updater stages the app next to the install, outside any user \
            permission sandbox, by design"
)]

use std::ffi::OsStr;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;

/// A digest field: a tag, then the value's length and bytes, so no value can
/// be confused with a separator. The encoding of [`super::swap::tree_digest`].
pub(super) fn digest_field(out: &mut sha2::Sha256, tag: &str, value: &str) {
  use sha2::Digest;
  out.update(tag.as_bytes());
  out.update((value.len() as u64).to_le_bytes());
  out.update(value.as_bytes());
}

fn refused(path: &Path, why: &str) -> std::io::Error {
  std::io::Error::new(
    std::io::ErrorKind::PermissionDenied,
    format!("{}: {why}", path.display()),
  )
}

/// A directory private to this user, held open.
#[derive(Debug)]
pub struct PrivateDir {
  path: PathBuf,
  #[cfg(unix)]
  fd: std::os::fd::OwnedFd,
  #[cfg(unix)]
  id: (u64, u64),
}

impl PrivateDir {
  /// Create the directory `path`, which must not exist yet, private to this
  /// user (0700 on Unix), and hold it.
  pub fn create(path: &Path) -> std::io::Result<Self> {
    #[cfg(unix)]
    {
      use std::os::unix::fs::DirBuilderExt;
      std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir(path)?;
    Self::open(path)
  }

  /// Hold the existing directory `path`: refused when it is a symlink (or on
  /// Windows a reparse point) or not a directory, and on Unix when another
  /// user owns it or it lets anyone but its owner in (any of the mode's
  /// group / other bits).
  pub fn open(path: &Path) -> std::io::Result<Self> {
    #[cfg(unix)]
    {
      use std::os::unix::fs::OpenOptionsExt;
      let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| {
          if e.raw_os_error() == Some(libc::ELOOP)
            || e.raw_os_error() == Some(libc::ENOTDIR)
          {
            refused(path, "not a directory (or a symlink)")
          } else {
            e
          }
        })?;
      let st = fstat(std::os::fd::AsRawFd::as_raw_fd(&f))?;
      // SAFETY: geteuid has no preconditions and cannot fail.
      let me = unsafe { libc::geteuid() };
      if st.st_uid != me {
        return Err(refused(path, "owned by another user"));
      }
      if st.st_mode as u32 & 0o077 != 0 {
        return Err(refused(
          path,
          "other users can enter it (its mode is not 0700)",
        ));
      }
      Ok(Self {
        path: path.to_path_buf(),
        id: (st.st_dev as u64, st.st_ino as u64),
        fd: f.into(),
      })
    }
    #[cfg(not(unix))]
    {
      let meta = std::fs::symlink_metadata(path)?;
      if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(refused(path, "not a directory (or a reparse point)"));
      }
      Ok(Self {
        path: path.to_path_buf(),
      })
    }
  }

  pub fn path(&self) -> &Path {
    &self.path
  }

  /// Whether the path still names the directory this holds (it was not
  /// renamed away and replaced). Always `true` on Windows.
  pub fn still_at_path(&self) -> bool {
    #[cfg(unix)]
    {
      use std::os::unix::fs::MetadataExt;
      std::fs::symlink_metadata(&self.path)
        .is_ok_and(|m| m.is_dir() && (m.dev(), m.ino()) == self.id)
    }
    #[cfg(not(unix))]
    true
  }

  /// The staged app `entry` inside the subdirectory `sub`, held through
  /// this directory.
  pub fn staged(&self, sub: &str, entry: &OsStr) -> std::io::Result<Staged> {
    #[cfg(unix)]
    {
      let sub_c = cstr(OsStr::new(sub))?;
      let fd = openat_dir(std::os::fd::AsRawFd::as_raw_fd(&self.fd), &sub_c)?;
      Ok(Staged {
        path: self.path.join(sub).join(entry),
        dir: fd,
        entry: entry.to_os_string(),
      })
    }
    #[cfg(not(unix))]
    Ok(Staged {
      path: self.path.join(sub).join(entry),
      entry: entry.to_os_string(),
    })
  }
}

/// The staged app, inside a held [`PrivateDir`].
#[derive(Debug)]
pub struct Staged {
  /// Its path (for messages, and the checks that take a path).
  pub path: PathBuf,
  #[cfg(unix)]
  dir: std::os::fd::OwnedFd,
  entry: OsString,
}

impl Staged {
  /// [`super::swap::tree_digest`] of the staged app, read through the held
  /// directory (Unix: never through the path, never following a symlink).
  /// A device, FIFO or socket in the tree is an error.
  pub fn digest(&self) -> std::io::Result<String> {
    #[cfg(unix)]
    {
      use sha2::Digest;
      let mut out = sha2::Sha256::new();
      let name = cstr(&self.entry)?;
      fd_walk::entry(
        std::os::fd::AsRawFd::as_raw_fd(&self.dir),
        &name,
        "",
        &mut out,
      )?;
      Ok(faster_hex::hex_string(&out.finalize()))
    }
    #[cfg(not(unix))]
    super::swap::tree_digest(&self.path)
  }

  /// Atomically exchange the staged app with `target`. `Ok(false)` when the
  /// OS or file system cannot (nothing changed).
  pub fn exchange_with(&self, target: &Path) -> std::io::Result<bool> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
      let (parent, name) = open_parent(target)?;
      let from = cstr(&self.entry)?;
      let rc = exchange_at(
        std::os::fd::AsRawFd::as_raw_fd(&self.dir),
        &from,
        std::os::fd::AsRawFd::as_raw_fd(&parent),
        &name,
      );
      super::swap::exchange_result(rc)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    super::swap::exchange(&self.path, target)
  }

  /// Rename the staged app (or, after an exchange, what took its place) to
  /// `target`.
  pub fn move_to(&self, target: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
      let (parent, name) = open_parent(target)?;
      let from = cstr(&self.entry)?;
      // SAFETY: valid descriptors and NUL-terminated names.
      let rc = unsafe {
        libc::renameat(
          std::os::fd::AsRawFd::as_raw_fd(&self.dir),
          from.as_ptr(),
          std::os::fd::AsRawFd::as_raw_fd(&parent),
          name.as_ptr(),
        )
      };
      if rc == 0 {
        Ok(())
      } else {
        Err(std::io::Error::last_os_error())
      }
    }
    #[cfg(not(unix))]
    super::swap::rename_retry(&self.path, target)
  }
}

#[cfg(unix)]
fn cstr(s: &OsStr) -> std::io::Result<std::ffi::CString> {
  use std::os::unix::ffi::OsStrExt;
  Ok(std::ffi::CString::new(s.as_bytes())?)
}

#[cfg(unix)]
fn fstat(fd: std::os::fd::RawFd) -> std::io::Result<libc::stat> {
  // SAFETY: an all-zero `stat` is a valid value; fstat fills it.
  let mut st: libc::stat = unsafe { std::mem::zeroed() };
  // SAFETY: `st` is a valid out-pointer.
  if unsafe { libc::fstat(fd, &mut st) } != 0 {
    return Err(std::io::Error::last_os_error());
  }
  Ok(st)
}

/// `openat(dir, name)` as a directory, not following a symlink.
#[cfg(unix)]
fn openat_dir(
  dir: std::os::fd::RawFd,
  name: &std::ffi::CStr,
) -> std::io::Result<std::os::fd::OwnedFd> {
  use std::os::fd::FromRawFd;
  // SAFETY: a valid descriptor and NUL-terminated name.
  let fd = unsafe {
    libc::openat(
      dir,
      name.as_ptr(),
      libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
    )
  };
  if fd < 0 {
    return Err(std::io::Error::last_os_error());
  }
  // SAFETY: openat returned a new descriptor this function owns.
  Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}

/// The parent directory of `target`, opened, and its last component.
#[cfg(unix)]
fn open_parent(
  target: &Path,
) -> std::io::Result<(std::os::fd::OwnedFd, std::ffi::CString)> {
  use std::os::unix::fs::OpenOptionsExt;
  let (Some(parent), Some(name)) = (target.parent(), target.file_name()) else {
    return Err(std::io::Error::new(
      std::io::ErrorKind::InvalidInput,
      format!("{}: no parent directory", target.display()),
    ));
  };
  let f = std::fs::OpenOptions::new()
    .read(true)
    .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
    .open(parent)?;
  Ok((f.into(), cstr(name)?))
}

/// `renameatx_np(RENAME_SWAP)` / `renameat2(RENAME_EXCHANGE)`; the raw
/// return code (errno in `last_os_error`).
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn exchange_at(
  a_dir: std::os::fd::RawFd,
  a: &std::ffi::CStr,
  b_dir: std::os::fd::RawFd,
  b: &std::ffi::CStr,
) -> libc::c_int {
  #[cfg(target_os = "macos")]
  // SAFETY: valid descriptors and NUL-terminated names.
  let rc = unsafe {
    libc::renameatx_np(a_dir, a.as_ptr(), b_dir, b.as_ptr(), libc::RENAME_SWAP)
  };
  #[cfg(target_os = "linux")]
  // SAFETY: valid descriptors and NUL-terminated names; renameat2's
  // documented signature.
  let rc = unsafe {
    libc::syscall(
      libc::SYS_renameat2,
      a_dir,
      a.as_ptr(),
      b_dir,
      b.as_ptr(),
      libc::RENAME_EXCHANGE,
    ) as libc::c_int
  };
  rc
}

/// [`Staged::digest`]'s walk: the fields of `tree_digest`, read through
/// directory descriptors.
#[cfg(unix)]
mod fd_walk {
  use std::ffi::CStr;
  use std::ffi::CString;
  use std::ffi::OsString;
  use std::os::fd::AsRawFd;
  use std::os::fd::FromRawFd;
  use std::os::fd::OwnedFd;
  use std::os::fd::RawFd;
  use std::os::unix::ffi::OsStrExt;
  use std::os::unix::ffi::OsStringExt;

  use super::digest_field;
  use super::fstat;
  use super::openat_dir;

  fn changed(rel: &str) -> std::io::Error {
    std::io::Error::other(format!("{rel}: changed while it was read"))
  }

  fn fstatat(dir: RawFd, name: &CStr) -> std::io::Result<libc::stat> {
    // SAFETY: an all-zero `stat` is a valid value; fstatat fills it.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: valid descriptor, NUL-terminated name, valid out-pointer.
    let rc = unsafe {
      libc::fstatat(dir, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW)
    };
    if rc != 0 {
      return Err(std::io::Error::last_os_error());
    }
    Ok(st)
  }

  fn readlinkat(dir: RawFd, name: &CStr) -> std::io::Result<OsString> {
    let mut buf = vec![0u8; 4096];
    loop {
      // SAFETY: `buf` is valid for `buf.len()` bytes.
      let n = unsafe {
        libc::readlinkat(
          dir,
          name.as_ptr(),
          buf.as_mut_ptr() as *mut libc::c_char,
          buf.len(),
        )
      };
      if n < 0 {
        return Err(std::io::Error::last_os_error());
      }
      let n = n as usize;
      if n < buf.len() {
        buf.truncate(n);
        return Ok(OsString::from_vec(buf));
      }
      buf.resize(buf.len() * 2, 0);
    }
  }

  /// The names in the directory `dir` (not `.` / `..`).
  fn list(dir: RawFd) -> std::io::Result<Vec<OsString>> {
    // fdopendir takes the descriptor it is given: hand it a duplicate.
    // SAFETY: `dir` is a valid descriptor.
    let dup = unsafe { libc::fcntl(dir, libc::F_DUPFD_CLOEXEC, 0) };
    if dup < 0 {
      return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `dup` is a directory descriptor this function owns.
    let d = unsafe { libc::fdopendir(dup) };
    if d.is_null() {
      let e = std::io::Error::last_os_error();
      // SAFETY: fdopendir failed, so `dup` is still ours to close.
      unsafe { libc::close(dup) };
      return Err(e);
    }
    // fdopendir shares the file offset with `dir`'s duplicate: start over.
    // SAFETY: `d` is a valid DIR.
    unsafe { libc::rewinddir(d) };
    let mut names = Vec::new();
    let result = loop {
      set_errno(0);
      // SAFETY: `d` is a valid DIR.
      let ent = unsafe { libc::readdir(d) };
      if ent.is_null() {
        match errno() {
          0 => break Ok(()),
          e => break Err(std::io::Error::from_raw_os_error(e)),
        }
      }
      // SAFETY: readdir returned a valid entry with a NUL-terminated name.
      let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) };
      let bytes = name.to_bytes();
      if bytes != b"." && bytes != b".." {
        names.push(OsString::from_vec(bytes.to_vec()));
      }
    };
    // SAFETY: `d` is a valid DIR, closed once (this closes `dup`).
    unsafe { libc::closedir(d) };
    result.map(|()| names)
  }

  #[cfg(target_os = "macos")]
  fn errno_location() -> *mut libc::c_int {
    // SAFETY: no preconditions.
    unsafe { libc::__error() }
  }
  #[cfg(any(target_os = "linux", target_os = "android"))]
  fn errno_location() -> *mut libc::c_int {
    // SAFETY: no preconditions.
    unsafe { libc::__errno_location() }
  }
  #[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
  fn set_errno(v: libc::c_int) {
    // SAFETY: the thread's errno location is always valid.
    unsafe { *errno_location() = v };
  }
  #[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
  fn errno() -> libc::c_int {
    // SAFETY: the thread's errno location is always valid.
    unsafe { *errno_location() }
  }
  #[cfg(not(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "android"
  )))]
  fn set_errno(_: libc::c_int) {}
  #[cfg(not(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "android"
  )))]
  fn errno() -> libc::c_int {
    0
  }

  fn same(a: &libc::stat, b: &libc::stat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino
  }

  /// Hash the entry `name` of the directory `dir`, at `rel` in the tree.
  pub(super) fn entry(
    dir: RawFd,
    name: &CStr,
    rel: &str,
    out: &mut sha2::Sha256,
  ) -> std::io::Result<()> {
    let st = fstatat(dir, name)?;
    match st.st_mode & libc::S_IFMT {
      libc::S_IFLNK => {
        let target = readlinkat(dir, name)?;
        digest_field(out, "l", rel);
        digest_field(out, "t", &target.to_string_lossy());
      }
      libc::S_IFDIR => {
        let fd = openat_dir(dir, name)?;
        if !same(&st, &fstat(fd.as_raw_fd())?) {
          return Err(changed(rel));
        }
        digest_field(out, "d", rel);
        let mut names = list(fd.as_raw_fd())?;
        names.sort();
        for child in names {
          let child_rel = if rel.is_empty() {
            child.to_string_lossy().into_owned()
          } else {
            format!("{rel}/{}", child.to_string_lossy())
          };
          let c = CString::new(child.as_bytes())?;
          entry(fd.as_raw_fd(), &c, &child_rel, out)?;
        }
      }
      libc::S_IFREG => {
        // SAFETY: a valid descriptor and NUL-terminated name. O_NONBLOCK: a
        // FIFO swapped in after the fstatat does not block the open.
        let raw = unsafe {
          libc::openat(
            dir,
            name.as_ptr(),
            libc::O_RDONLY
              | libc::O_NOFOLLOW
              | libc::O_CLOEXEC
              | libc::O_NONBLOCK,
          )
        };
        if raw < 0 {
          return Err(std::io::Error::last_os_error());
        }
        // SAFETY: openat returned a new descriptor this function owns.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let opened = fstat(fd.as_raw_fd())?;
        if !same(&st, &opened) || opened.st_mode & libc::S_IFMT != libc::S_IFREG
        {
          return Err(changed(rel));
        }
        let mut file = std::fs::File::from(fd);
        let mut h = <sha2::Sha256 as sha2::Digest>::new();
        let len = std::io::copy(&mut file, &mut h)?;
        if len != opened.st_size as u64 {
          return Err(changed(rel));
        }
        digest_field(out, "f", rel);
        digest_field(out, "s", &len.to_string());
        digest_field(
          out,
          "h",
          &faster_hex::hex_string(&sha2::Digest::finalize(h)),
        );
      }
      _ => {
        return Err(std::io::Error::new(
          std::io::ErrorKind::InvalidData,
          format!("{rel}: not a file, directory or symlink"),
        ));
      }
    }
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn tree(root: &Path) {
    std::fs::create_dir_all(root.join("a/b")).unwrap();
    std::fs::write(root.join("a/b/f"), "x").unwrap();
    std::fs::write(root.join("top"), "hello").unwrap();
    std::fs::create_dir_all(root.join("empty")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("a/b/f", root.join("link")).unwrap();
  }

  #[test]
  fn the_held_digest_is_tree_digest() {
    let t = tempfile::tempdir().unwrap();
    let dir = PrivateDir::create(&t.path().join("staging")).unwrap();
    tree(&dir.path().join("extract/App"));
    let staged = dir.staged("extract", OsStr::new("App")).unwrap();
    assert_eq!(
      staged.digest().unwrap(),
      super::super::swap::tree_digest(&staged.path).unwrap()
    );
    // A single file as the staged app (an AppImage).
    std::fs::write(dir.path().join("extract/App.AppImage"), "img").unwrap();
    let file = dir.staged("extract", OsStr::new("App.AppImage")).unwrap();
    assert_eq!(
      file.digest().unwrap(),
      super::super::swap::tree_digest(&file.path).unwrap()
    );
  }

  #[cfg(unix)]
  #[test]
  fn only_a_private_directory_of_this_user_is_held() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("staging");
    let dir = PrivateDir::create(&p).unwrap();
    assert!(dir.still_at_path());
    // Created fresh: an existing one is never adopted.
    assert!(PrivateDir::create(&p).is_err());
    for mode in [0o750, 0o705, 0o770, 0o777] {
      std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode))
        .unwrap();
      let e = PrivateDir::open(&p).unwrap_err();
      assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied, "{mode:o}");
    }
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700))
      .unwrap();
    PrivateDir::open(&p).unwrap();
    // A symlink to a private directory is not one.
    let link = t.path().join("link");
    std::os::unix::fs::symlink(&p, &link).unwrap();
    assert!(PrivateDir::open(&link).is_err());
    // Renamed away and replaced: the held directory is no longer at the path.
    std::fs::rename(&p, t.path().join("away")).unwrap();
    std::fs::create_dir(&p).unwrap();
    assert!(!dir.still_at_path());
  }

  /// The digest reads what the held directory holds, whatever is now at the
  /// path.
  #[cfg(unix)]
  #[test]
  fn the_digest_follows_the_held_directory_not_the_path() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("staging");
    let dir = PrivateDir::create(&p).unwrap();
    tree(&p.join("extract/App"));
    let staged = dir.staged("extract", OsStr::new("App")).unwrap();
    let before = staged.digest().unwrap();
    std::fs::rename(&p, t.path().join("away")).unwrap();
    std::fs::create_dir_all(p.join("extract/App")).unwrap();
    std::fs::write(p.join("extract/App/top"), "planted").unwrap();
    assert_eq!(staged.digest().unwrap(), before);
    // A FIFO in the tree is refused, not read (and does not block).
    let fifo = t.path().join("away/extract/App/fifo");
    let c = cstr(fifo.as_os_str()).unwrap();
    // SAFETY: a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    assert!(staged.digest().is_err());
  }
}
