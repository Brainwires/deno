// Copyright 2018-2026 the Deno authors. MIT license.

//! The broker's file operations. The broker runs unconfined and holds the
//! run's read and write grants and denies ([`FsPolicy`]) exactly as Deno's
//! own checks hold them; for each request it resolves the path itself with
//! the race-safe opens ([`crate::checked_open`]): beneath an allow root,
//! outside every deny, checked on the object the open reached (a symlink
//! swapped in during the lookup is followed only to a place the policy
//! allows), then acts on that object and nothing else.
//!
//! What it never does:
//! - trust a path's spelling: the request's path is the confined side's
//!   wish; the decision is made on the object opened;
//! - reach the kernel's own trees: `/proc`, `/sys` and `/dev` are denied
//!   whatever the grant says. Resolved by the broker, `/proc/self` names the
//!   broker and `/proc/self/fd/N` its descriptors, not the requester's, and
//!   a device node is not a file. (The confined process opens those itself
//!   where its sandbox lets it.) Wherever a deny could be reached from an
//!   allow root, the opened object's own path is verified after the open,
//!   and where that can't be read back (no `/proc`) the request is refused;
//! - hand out anything but a regular file, a directory or a FIFO;
//! - wait: opens are non-blocking (a FIFO with no reader fails with `ENXIO`
//!   instead of holding the channel), and the flag is cleared before the
//!   descriptor leaves.
//!
//! A hard link is path semantics, as in Deno's checks and in Landlock: a
//! link to a denied file placed in an allowed directory opens.

#![allow(
  clippy::disallowed_methods,
  reason = "the broker acts on the real file system"
)]

use std::ffi::OsString;
use std::io;
use std::os::fd::AsFd;
use std::os::fd::AsRawFd;
use std::os::fd::BorrowedFd;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

use super::protocol::OpenFlags;
use super::protocol::Remove;
use super::protocol::Request;
#[cfg(target_os = "linux")]
use super::protocol::Time;
use super::protocol::WirePath;
use super::protocol::features;
use super::server::Handler;
use super::server::Reply;
use crate::checked_open::CheckedOpenError;
use crate::checked_open::CheckedOpenOptions;
use crate::checked_open::CheckedParent;
use crate::checked_open::OpenScope;
use crate::checked_open::Symlinks;
use crate::checked_open::open_checked;
use crate::checked_open::parent_checked;

/// The run's file grants as the broker holds them. Every path is absolute;
/// a scope that is reached through a symlink is listed in its resolved form
/// too (`/tmp` and `/private/tmp`), as the checks compare them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FsPolicy {
  /// `--allow-read`: `None` is unscoped; `Some([])` grants nothing.
  pub read: Option<Vec<PathBuf>>,
  pub deny_read: Vec<PathBuf>,
  /// `--allow-write`, likewise.
  pub write: Option<Vec<PathBuf>>,
  pub deny_write: Vec<PathBuf>,
}

/// The trees the broker never enters (see the module docs).
#[cfg(target_os = "linux")]
const KERNEL_TREES: &[&str] = &["/proc", "/sys", "/dev"];
#[cfg(not(target_os = "linux"))]
const KERNEL_TREES: &[&str] = &["/dev"];

pub struct FsBroker {
  read: OpenScope,
  write: OpenScope,
  /// Both at once (an open for reading and writing, a rename's source).
  read_write: OpenScope,
  /// Read and write are unscoped (what creating a symlink takes).
  unscoped: bool,
  /// Every deny, for the operations that move a whole tree.
  denies: Vec<PathBuf>,
}

impl FsBroker {
  /// Fails on a policy path that isn't absolute.
  pub fn new(policy: &FsPolicy) -> io::Result<Self> {
    let kernel = || KERNEL_TREES.iter().map(PathBuf::from);
    let deny_read: Vec<PathBuf> =
      policy.deny_read.iter().cloned().chain(kernel()).collect();
    let deny_write: Vec<PathBuf> =
      policy.deny_write.iter().cloned().chain(kernel()).collect();
    let mut denies = deny_read.clone();
    for d in &deny_write {
      if !denies.contains(d) {
        denies.push(d.clone());
      }
    }
    Ok(Self {
      read: OpenScope::new(policy.read.clone(), deny_read)?,
      write: OpenScope::new(policy.write.clone(), deny_write)?,
      read_write: OpenScope::new(
        crate::checked_open::intersect_roots(
          policy.read.clone(),
          policy.write.clone(),
        ),
        denies.clone(),
      )?,
      unscoped: policy.read.is_none() && policy.write.is_none(),
      denies,
    })
  }

  fn open(&self, path: &WirePath, flags: OpenFlags, mode: u32) -> Reply {
    let scope = match (flags.contains(OpenFlags::READ), flags.changes()) {
      (true, false) => &self.read,
      (false, true) => &self.write,
      (true, true) => &self.read_write,
      (false, false) => return Reply::denied(),
    };
    let opts = CheckedOpenOptions::new()
      .read(flags.contains(OpenFlags::READ))
      .write(flags.contains(OpenFlags::WRITE))
      .append(flags.contains(OpenFlags::APPEND))
      .truncate(flags.contains(OpenFlags::TRUNCATE))
      .create(flags.contains(OpenFlags::CREATE))
      .create_new(flags.contains(OpenFlags::CREATE_NEW))
      .directory(flags.contains(OpenFlags::DIRECTORY))
      .nonblocking(true)
      .mode(mode)
      .symlinks(if flags.contains(OpenFlags::NOFOLLOW) {
        Symlinks::NoFollowFinal
      } else {
        Symlinks::Follow
      });
    let file = match open_checked(scope, &to_path(path), &opts) {
      Ok(file) => file,
      Err(err) => return refused(&err),
    };
    let fd = file.into_fd();
    match file_kind(fd.as_fd()) {
      Some(libc::S_IFREG | libc::S_IFDIR | libc::S_IFIFO) => {}
      _ => return Reply::denied(),
    }
    if let Err(err) = clear_nonblocking(fd.as_fd()) {
      return Reply::io_error(&err);
    }
    Reply::ok_fd(fd)
  }

  fn parent(
    &self,
    scope: &OpenScope,
    path: &WirePath,
  ) -> Result<CheckedParent, Reply> {
    parent_checked(scope, &to_path(path)).map_err(|err| refused(&err))
  }

  /// Whether a deny lies at or beneath the entry (moving it would move the
  /// denied path with it), or that can't be told.
  fn holds_a_deny(&self, entry: &CheckedParent) -> bool {
    match entry.real_path() {
      Some(real) => self.denies.iter().any(|d| d.starts_with(&real)),
      None => true,
    }
  }

  fn mkdir(&self, path: &WirePath, mode: u32) -> Reply {
    match self.parent(&self.write, path) {
      Ok(parent) => done(parent.mkdir(mode)),
      Err(reply) => reply,
    }
  }

  fn remove(&self, path: &WirePath, what: Remove) -> Reply {
    let parent = match self.parent(&self.write, path) {
      Ok(parent) => parent,
      Err(reply) => return reply,
    };
    match what {
      Remove::File => done(parent.remove_file()),
      Remove::Dir => done(parent.remove_dir()),
      // a tree that holds a denied path stays (Deno's own check refuses a
      // recursive removal over a deny too); nothing in it is followed
      Remove::Tree if self.holds_a_deny(&parent) => Reply::denied(),
      Remove::Tree => done(parent.remove_dir_all()),
    }
  }

  fn rename(&self, from: &WirePath, to: &WirePath, noreplace: bool) -> Reply {
    let from = match self.parent(&self.read_write, from) {
      Ok(parent) => parent,
      Err(reply) => return reply,
    };
    let to = match self.parent(&self.write, to) {
      Ok(parent) => parent,
      Err(reply) => return reply,
    };
    if self.holds_a_deny(&from) || self.holds_a_deny(&to) {
      return Reply::denied();
    }
    done(if noreplace {
      from.rename_noreplace_to(&to)
    } else {
      from.rename_to(&to)
    })
  }

  fn symlink(&self, target: &[u8], link: &WirePath) -> Reply {
    // a link's target is resolved only when it is followed, so creating
    // one takes the whole read and write permission, as Deno's check does
    if !self.unscoped {
      return Reply::denied();
    }
    match self.parent(&self.write, link) {
      Ok(parent) => {
        let target = PathBuf::from(OsString::from_vec(target.to_vec()));
        done(parent.symlink(&target))
      }
      Err(reply) => reply,
    }
  }

  fn link(&self, existing: &WirePath, new: &WirePath) -> Reply {
    let existing = match self.parent(&self.read_write, existing) {
      Ok(parent) => parent,
      Err(reply) => return reply,
    };
    let new = match self.parent(&self.write, new) {
      Ok(parent) => parent,
      Err(reply) => return reply,
    };
    done(existing.hard_link_to(&new))
  }

  fn truncate(&self, path: &WirePath, len: u64) -> Reply {
    let opts = CheckedOpenOptions::new()
      .write(true)
      .nonblocking(true)
      .symlinks(Symlinks::NoFollowFinal);
    let file = match open_checked(&self.write, &to_path(path), &opts) {
      Ok(file) => file,
      Err(err) => return refused(&err),
    };
    if file_kind(file.as_fd()) != Some(libc::S_IFREG) {
      return Reply::errno(libc::EINVAL);
    }
    // SAFETY: a valid descriptor; the length fits an off_t (decoded so)
    done(cvt(unsafe {
      libc::ftruncate(file.as_fd().as_raw_fd(), len as libc::off_t)
    }))
  }

  /// The object a metadata change applies to, named without being opened.
  #[cfg(target_os = "linux")]
  fn object(
    &self,
    path: &WirePath,
    nofollow: bool,
  ) -> Result<std::os::fd::OwnedFd, Reply> {
    let opts =
      CheckedOpenOptions::new()
        .path_only(true)
        .symlinks(if nofollow {
          Symlinks::NoFollowFinal
        } else {
          Symlinks::Follow
        });
    open_checked(&self.write, &to_path(path), &opts)
      .map(|file| file.into_fd())
      .map_err(|err| refused(&err))
  }

  #[cfg(target_os = "linux")]
  fn chmod(&self, path: &WirePath, mode: u32, nofollow: bool) -> Reply {
    let fd = match self.object(path, nofollow) {
      Ok(fd) => fd,
      Err(reply) => return reply,
    };
    if file_kind(fd.as_fd()) == Some(libc::S_IFLNK) {
      // Linux has no mode on a symlink
      return Reply::errno(libc::EOPNOTSUPP);
    }
    let through = proc_path(fd.as_fd());
    // SAFETY: a NUL-terminated path to this process's own descriptor
    done(cvt(unsafe {
      libc::chmod(through.as_ptr(), mode as libc::mode_t)
    }))
  }

  #[cfg(target_os = "linux")]
  fn chown(
    &self,
    path: &WirePath,
    uid: u32,
    gid: u32,
    nofollow: bool,
  ) -> Reply {
    let fd = match self.object(path, nofollow) {
      Ok(fd) => fd,
      Err(reply) => return reply,
    };
    // SAFETY: a path-only descriptor with AT_EMPTY_PATH; u32::MAX is the
    // kernel's "leave it"
    done(cvt(unsafe {
      libc::fchownat(
        fd.as_raw_fd(),
        c"".as_ptr(),
        uid,
        gid,
        libc::AT_EMPTY_PATH,
      )
    }))
  }

  #[cfg(target_os = "linux")]
  fn utime(
    &self,
    path: &WirePath,
    atime: Time,
    mtime: Time,
    nofollow: bool,
  ) -> Reply {
    let fd = match self.object(path, nofollow) {
      Ok(fd) => fd,
      Err(reply) => return reply,
    };
    let ts = |t: Time| libc::timespec {
      tv_sec: t.sec as libc::time_t,
      tv_nsec: t.nsec as _,
    };
    let times = [ts(atime), ts(mtime)];
    // SAFETY: a path-only descriptor with AT_EMPTY_PATH and two timespecs
    let res = unsafe {
      libc::utimensat(
        fd.as_raw_fd(),
        c"".as_ptr(),
        times.as_ptr(),
        libc::AT_EMPTY_PATH,
      )
    };
    if res == 0 {
      return Reply::ok();
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EINVAL)
      && file_kind(fd.as_fd()) != Some(libc::S_IFLNK)
    {
      // a kernel without AT_EMPTY_PATH here: through the descriptor
      let through = proc_path(fd.as_fd());
      // SAFETY: a NUL-terminated path and two timespecs
      return done(cvt(unsafe {
        libc::utimensat(libc::AT_FDCWD, through.as_ptr(), times.as_ptr(), 0)
      }));
    }
    Reply::io_error(&err)
  }
}

impl Handler for FsBroker {
  fn features(&self) -> u64 {
    if cfg!(target_os = "linux") {
      features::FS | features::FS_METADATA
    } else {
      features::FS
    }
  }

  fn handle(&self, request: &Request) -> Reply {
    match request {
      // the server's own
      Request::Hello { .. } | Request::Channel | Request::Received => {
        Reply::unsupported()
      }
      Request::Open { path, flags, mode } => self.open(path, *flags, *mode),
      Request::Mkdir { path, mode } => self.mkdir(path, *mode),
      Request::Remove { path, what } => self.remove(path, *what),
      Request::Rename {
        from,
        to,
        noreplace,
      } => self.rename(from, to, *noreplace),
      Request::Symlink { target, link } => self.symlink(target, link),
      Request::Link { existing, new } => self.link(existing, new),
      Request::Truncate { path, len } => self.truncate(path, *len),
      #[cfg(target_os = "linux")]
      Request::Chmod {
        path,
        mode,
        nofollow,
      } => self.chmod(path, *mode, *nofollow),
      #[cfg(target_os = "linux")]
      Request::Chown {
        path,
        uid,
        gid,
        nofollow,
      } => self.chown(path, *uid, *gid, *nofollow),
      #[cfg(target_os = "linux")]
      Request::Utime {
        path,
        atime,
        mtime,
        nofollow,
      } => self.utime(path, *atime, *mtime, *nofollow),
      #[cfg(not(target_os = "linux"))]
      Request::Chmod { .. } | Request::Chown { .. } | Request::Utime { .. } => {
        Reply::unsupported()
      }
    }
  }
}

fn to_path(path: &WirePath) -> PathBuf {
  PathBuf::from(OsString::from_vec(path.as_bytes().to_vec()))
}

fn refused(err: &CheckedOpenError) -> Reply {
  match err {
    CheckedOpenError::NotCapable(_) => Reply::denied(),
    CheckedOpenError::Io { source, .. } => Reply::io_error(source),
  }
}

fn done(result: io::Result<()>) -> Reply {
  match result {
    Ok(()) => Reply::ok(),
    Err(err) => Reply::io_error(&err),
  }
}

fn cvt(res: libc::c_int) -> io::Result<()> {
  if res < 0 {
    Err(io::Error::last_os_error())
  } else {
    Ok(())
  }
}

/// The `S_IFMT` bits of an open descriptor.
fn file_kind(fd: BorrowedFd<'_>) -> Option<libc::mode_t> {
  // SAFETY: zeroed is a valid stat; the call fills it
  let mut st: libc::stat = unsafe { std::mem::zeroed() };
  // SAFETY: a valid descriptor and stat buffer
  if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
    return None;
  }
  Some(st.st_mode & libc::S_IFMT)
}

fn clear_nonblocking(fd: BorrowedFd<'_>) -> io::Result<()> {
  // SAFETY: a valid descriptor
  let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
  if flags < 0 {
    return Err(io::Error::last_os_error());
  }
  if flags & libc::O_NONBLOCK == 0 {
    return Ok(());
  }
  // SAFETY: a valid descriptor and status flags
  cvt(unsafe {
    libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK)
  })
}

#[cfg(target_os = "linux")]
fn proc_path(fd: BorrowedFd<'_>) -> std::ffi::CString {
  std::ffi::CString::new(format!("/proc/self/fd/{}", fd.as_raw_fd()))
    .expect("no NUL in a number")
}

/// Whether `path` is one the broker can be asked about at all (outside the
/// kernel's trees, by its spelling): the confined side skips the round trip
/// for the rest.
pub fn brokerable(path: &Path) -> bool {
  !KERNEL_TREES.iter().any(|tree| path.starts_with(tree))
}
