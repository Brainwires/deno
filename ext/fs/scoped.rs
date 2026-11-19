// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: the real file system's operations on a path the permission check
//! scoped ([`deno_permissions::CheckedPath::open_scope`]). Each one opens
//! the path race-safely ([`deno_permissions::checked_open`]): the open is
//! the check, so a symlink swapped in after
//! `PermissionsContainer::check_open` (by the program, another process or
//! another user) is followed only to a place the permissions allow, and the
//! operation acts on the descriptor it got, never on the name again.
//!
//! Under `--kernel-sandbox=broker` an operation the kernel sandbox refuses
//! (`EACCES` / `EPERM`: on Linux, a change directly in a folder whose write
//! allow holds a deny) is asked of the broker
//! ([`deno_permissions::kernel_broker`]), which holds Deno's rule exactly.
//!
//! Not here yet (the path-based code in `std_fs.rs` still serves them, after
//! the same permission check): `realpath`, `statfs`, `exists`, `chdir`,
//! `copyFile`, `cp`, `lchmod`; on macOS the last step of `lstat` (and of a
//! `stat` of a file that can't be opened) is by name.

#![allow(
  clippy::disallowed_methods,
  reason = "the real file system's own implementation"
)]

use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;

use deno_io::fs::FsError;
use deno_io::fs::FsResult;
use deno_io::fs::FsStat;
use deno_permissions::PermissionDeniedError;
use deno_permissions::PermissionState;
use deno_permissions::checked_open::CheckedOpenError;
use deno_permissions::checked_open::CheckedOpenOptions;
use deno_permissions::checked_open::CheckedParent;
use deno_permissions::checked_open::EntryKind;
use deno_permissions::checked_open::OpenScope;
use deno_permissions::checked_open::Symlinks;
use deno_permissions::checked_open::open_checked;
use deno_permissions::checked_open::parent_checked;
use deno_permissions::kernel_broker::client;
use deno_permissions::kernel_broker::client::BrokerClient;
use deno_permissions::kernel_broker::protocol::OpenFlags;
use deno_permissions::kernel_broker::protocol::Remove;
use deno_permissions::kernel_broker::protocol::Time;

use crate::FsDirEntry;
use crate::OpenOptions;

/// The scope of a path whose access nothing narrows, for an operation on
/// two paths of which only the other is scoped.
pub static UNSCOPED: LazyLock<OpenScope> = LazyLock::new(|| {
  OpenScope::new(None, Vec::new()).expect("no paths to refuse")
});

fn not_capable(path: &Path) -> FsError {
  FsError::PermissionCheck(
    PermissionDeniedError {
      access: format!("access to \"{}\"", path.display()),
      name: "read",
      custom_message: Some(format!(
        "Requires access to \"{}\": it leads outside the allowed scopes or into a denied one",
        path.display()
      )),
      state: PermissionState::Denied,
    }
    .into(),
  )
}

fn fs_error(err: CheckedOpenError) -> FsError {
  match err {
    CheckedOpenError::NotCapable(path) => not_capable(&path),
    CheckedOpenError::Io { source, .. } => FsError::Io(source),
  }
}

/// The broker, when this process runs under one and the OS refused what
/// the check allowed: the kernel sandbox is narrower than Deno's rule there.
fn broker_for(err: &io::Error, path: &Path) -> Option<&'static BrokerClient> {
  if !matches!(err.raw_os_error(), Some(libc::EACCES | libc::EPERM)) {
    return None;
  }
  client::get()
    .filter(|_| deno_permissions::kernel_broker::fs::brokerable(path))
}

/// `result`, or the broker's answer where the kernel sandbox refused.
fn or_broker<T>(
  result: Result<T, CheckedOpenError>,
  path: &Path,
  ask: impl FnOnce(&BrokerClient) -> io::Result<T>,
) -> FsResult<T> {
  match result {
    Ok(value) => Ok(value),
    Err(CheckedOpenError::Io { source, .. }) => {
      match broker_for(&source, path) {
        Some(broker) => match ask(broker) {
          Ok(value) => Ok(value),
          // a broker without the operation: the OS's own refusal stands
          Err(err) if err.kind() == io::ErrorKind::Unsupported => {
            Err(FsError::Io(source))
          }
          Err(err) => Err(FsError::Io(err)),
        },
        None => Err(FsError::Io(source)),
      }
    }
    Err(err) => Err(fs_error(err)),
  }
}

fn io_step<T>(
  path: &Path,
  result: io::Result<T>,
) -> Result<T, CheckedOpenError> {
  result.map_err(|source| CheckedOpenError::Io {
    path: path.to_path_buf(),
    source,
  })
}

fn checked_options(opts: &OpenOptions) -> CheckedOpenOptions {
  let mut checked = CheckedOpenOptions::new()
    .read(opts.read)
    .write(opts.write)
    .append(opts.append)
    // with create_new the file is new: nothing to truncate
    .truncate(opts.truncate && !opts.create_new)
    .create(opts.create)
    .create_new(opts.create_new);
  if let Some(mode) = opts.mode {
    checked = checked.mode(mode & 0o777);
  }
  if let Some(flags) = opts.custom_flags {
    checked = checked.custom_flags(flags);
  }
  checked
}

fn broker_flags(opts: &OpenOptions) -> Option<OpenFlags> {
  let mut bits = 0;
  let mut set = |on: bool, flag: OpenFlags| {
    if on {
      bits |= flag.bits();
    }
  };
  set(opts.read || !(opts.write || opts.append), OpenFlags::READ);
  set(opts.write, OpenFlags::WRITE);
  set(opts.append, OpenFlags::APPEND);
  set(opts.truncate && !opts.create_new, OpenFlags::TRUNCATE);
  set(opts.create, OpenFlags::CREATE);
  set(opts.create_new, OpenFlags::CREATE_NEW);
  let custom = opts.custom_flags.unwrap_or(0);
  set(custom & libc::O_NOFOLLOW != 0, OpenFlags::NOFOLLOW);
  set(custom & libc::O_DIRECTORY != 0, OpenFlags::DIRECTORY);
  OpenFlags::from_bits(bits)
}

/// The status flags of the caller's own that a descriptor from the broker
/// can still take (the broker opens with the flags it knows).
fn apply_status_flags(fd: &OwnedFd, custom: i32) {
  #[cfg(any(target_os = "linux", target_os = "android"))]
  let settable = libc::O_NONBLOCK | libc::O_DIRECT | libc::O_NOATIME;
  #[cfg(not(any(target_os = "linux", target_os = "android")))]
  let settable = libc::O_NONBLOCK;
  let wanted = custom & settable;
  if wanted == 0 {
    return;
  }
  // SAFETY: a valid descriptor; status flags only
  unsafe {
    let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
    if flags >= 0 {
      libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | wanted);
    }
  }
}

pub fn open(
  scope: &OpenScope,
  path: &Path,
  opts: OpenOptions,
) -> FsResult<fs::File> {
  // as `std::fs::OpenOptions` has it: truncating takes write access
  // (never append), so a read-only open can't empty a file
  if opts.truncate && !opts.create_new && (!opts.write || opts.append) {
    return Err(FsError::Io(io::Error::from_raw_os_error(libc::EINVAL)));
  }
  let result = open_checked(scope, path, &checked_options(&opts))
    .map(|file| file.into_std_file());
  or_broker(result, path, |broker| {
    let flags = broker_flags(&opts)
      .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
    let fd = broker.open(path, flags, opts.mode.unwrap_or(0o666) & 0o777)?;
    apply_status_flags(&fd, opts.custom_flags.unwrap_or(0));
    Ok(fs::File::from(fd))
  })
}

pub fn truncate(scope: &OpenScope, path: &Path, len: u64) -> FsResult<()> {
  // never through a link as the last component (the check is no-follow)
  let opts = CheckedOpenOptions::new()
    .write(true)
    .symlinks(Symlinks::NoFollowFinal);
  let result = open_checked(scope, path, &opts)
    .and_then(|file| io_step(path, file.into_std_file().set_len(len)));
  or_broker(result, path, |broker| broker.truncate(path, len))
}

fn mkdir_one(scope: &OpenScope, path: &Path, mode: u32) -> FsResult<()> {
  let result = parent_checked(scope, path)
    .and_then(|parent| io_step(path, parent.mkdir(mode)));
  or_broker(result, path, |broker| broker.mkdir(path, mode))
}

pub fn mkdir(
  scope: &OpenScope,
  path: &Path,
  recursive: bool,
  mode: Option<u32>,
) -> FsResult<()> {
  let mode = mode.unwrap_or(0o777);
  if !recursive {
    return mkdir_one(scope, path, mode);
  }
  // as `std::fs::DirBuilder::create` does it, each level in the scope
  fn all(scope: &OpenScope, path: &Path, mode: u32) -> FsResult<()> {
    match mkdir_one(scope, path, mode) {
      Ok(()) => return Ok(()),
      Err(err) if err.kind() == io::ErrorKind::NotFound => {}
      Err(_) if path.is_dir() => return Ok(()),
      Err(err) => return Err(err),
    }
    match path.parent() {
      Some(parent) => all(scope, parent, mode)?,
      None => {
        return Err(FsError::Io(io::Error::other(
          "failed to create whole tree",
        )));
      }
    }
    match mkdir_one(scope, path, mode) {
      Ok(()) => Ok(()),
      Err(_) if path.is_dir() => Ok(()),
      Err(err) => Err(err),
    }
  }
  all(scope, path, mode)
}

pub fn remove(scope: &OpenScope, path: &Path, recursive: bool) -> FsResult<()> {
  let parent = match parent_checked(scope, path) {
    Ok(parent) => parent,
    Err(err) => return Err(fs_error(err)),
  };
  let is_dir = parent
    .symlink_metadata()
    .map(|st| st.st_mode & libc::S_IFMT == libc::S_IFDIR)?;
  let result = if !is_dir {
    parent.remove_file()
  } else if recursive {
    parent.remove_dir_all()
  } else {
    parent.remove_dir()
  };
  or_broker(io_step(path, result), path, |broker| {
    broker.remove(
      path,
      match (is_dir, recursive) {
        (false, _) => Remove::File,
        (true, false) => Remove::Dir,
        (true, true) => Remove::Tree,
      },
    )
  })
}

pub fn rmdir(scope: &OpenScope, path: &Path) -> FsResult<()> {
  let result = parent_checked(scope, path)
    .and_then(|parent| io_step(path, parent.remove_dir()));
  or_broker(result, path, |broker| broker.remove(path, Remove::Dir))
}

pub fn rename(
  from_scope: &OpenScope,
  from: &Path,
  to_scope: &OpenScope,
  to: &Path,
) -> FsResult<()> {
  let result = two(from_scope, from, to_scope, to)
    .and_then(|(a, b)| io_step(from, a.rename_to(&b)));
  or_broker(result, to, |broker| broker.rename(from, to, false))
}

pub fn link(
  from_scope: &OpenScope,
  from: &Path,
  to_scope: &OpenScope,
  to: &Path,
) -> FsResult<()> {
  let result = two(from_scope, from, to_scope, to)
    .and_then(|(a, b)| io_step(to, a.hard_link_to(&b)));
  or_broker(result, to, |broker| broker.link(from, to))
}

fn two(
  from_scope: &OpenScope,
  from: &Path,
  to_scope: &OpenScope,
  to: &Path,
) -> Result<(CheckedParent, CheckedParent), CheckedOpenError> {
  Ok((
    parent_checked(from_scope, from)?,
    parent_checked(to_scope, to)?,
  ))
}

pub fn symlink(scope: &OpenScope, target: &Path, link: &Path) -> FsResult<()> {
  let result = parent_checked(scope, link)
    .and_then(|parent| io_step(link, parent.symlink(target)));
  or_broker(result, link, |broker| broker.symlink(target, link))
}

/// An open descriptor of the object at `path` (links followed) to change
/// its metadata through: read-only, or write-only for a file that can't be
/// read. Never waits (a FIFO).
fn metadata_fd(
  scope: &OpenScope,
  path: &Path,
) -> Result<fs::File, CheckedOpenError> {
  let opts = CheckedOpenOptions::new().read(true).nonblocking(true);
  match open_checked(scope, path, &opts) {
    Err(CheckedOpenError::Io { source, .. })
      if source.raw_os_error() == Some(libc::EACCES) =>
    {
      let opts = CheckedOpenOptions::new().write(true).nonblocking(true);
      match open_checked(scope, path, &opts) {
        Ok(file) => Ok(file.into_std_file()),
        // the first refusal is the one to report
        Err(_) => Err(CheckedOpenError::Io {
          path: path.to_path_buf(),
          source,
        }),
      }
    }
    other => other.map(|file| file.into_std_file()),
  }
}

pub fn chmod(scope: &OpenScope, path: &Path, mode: u32) -> FsResult<()> {
  use std::os::unix::fs::PermissionsExt;
  let result = metadata_fd(scope, path).and_then(|file| {
    io_step(path, file.set_permissions(fs::Permissions::from_mode(mode)))
  });
  or_broker(result, path, |broker| broker.chmod(path, mode, false))
}

pub fn chown(
  scope: &OpenScope,
  path: &Path,
  uid: Option<u32>,
  gid: Option<u32>,
) -> FsResult<()> {
  let result = metadata_fd(scope, path).and_then(|file| {
    // SAFETY: a valid descriptor; -1 is "leave it"
    let res = unsafe {
      libc::fchown(
        file.as_raw_fd(),
        uid.unwrap_or(u32::MAX) as libc::uid_t,
        gid.unwrap_or(u32::MAX) as libc::gid_t,
      )
    };
    io_step(
      path,
      if res < 0 {
        Err(io::Error::last_os_error())
      } else {
        Ok(())
      },
    )
  });
  or_broker(result, path, |broker| broker.chown(path, uid, gid, false))
}

pub fn lchown(
  scope: &OpenScope,
  path: &Path,
  uid: Option<u32>,
  gid: Option<u32>,
) -> FsResult<()> {
  let result = parent_checked(scope, path)
    .and_then(|parent| io_step(path, parent.chown_nofollow(uid, gid)));
  or_broker(result, path, |broker| broker.chown(path, uid, gid, true))
}

fn timespec(secs: i64, nanos: u32) -> libc::timespec {
  libc::timespec {
    tv_sec: secs as libc::time_t,
    tv_nsec: nanos as _,
  }
}

fn broker_time(secs: i64, nanos: u32) -> Time {
  Time {
    sec: secs,
    nsec: nanos.min(999_999_999),
  }
}

pub fn utime(
  scope: &OpenScope,
  path: &Path,
  atime: (i64, u32),
  mtime: (i64, u32),
) -> FsResult<()> {
  let result = metadata_fd(scope, path).and_then(|file| {
    let times = [timespec(atime.0, atime.1), timespec(mtime.0, mtime.1)];
    // SAFETY: a valid descriptor and two timespecs
    let res = unsafe { libc::futimens(file.as_raw_fd(), times.as_ptr()) };
    io_step(
      path,
      if res < 0 {
        Err(io::Error::last_os_error())
      } else {
        Ok(())
      },
    )
  });
  or_broker(result, path, |broker| {
    broker.utime(
      path,
      broker_time(atime.0, atime.1),
      broker_time(mtime.0, mtime.1),
      false,
    )
  })
}

pub fn lutime(
  scope: &OpenScope,
  path: &Path,
  atime: (i64, u32),
  mtime: (i64, u32),
) -> FsResult<()> {
  let result = parent_checked(scope, path).and_then(|parent| {
    io_step(
      path,
      parent
        .utime_nofollow(timespec(atime.0, atime.1), timespec(mtime.0, mtime.1)),
    )
  });
  or_broker(result, path, |broker| {
    broker.utime(
      path,
      broker_time(atime.0, atime.1),
      broker_time(mtime.0, mtime.1),
      true,
    )
  })
}

/// `stat`: the object the path leads to, links followed (each one checked).
pub fn stat(scope: &OpenScope, path: &Path) -> FsResult<FsStat> {
  // Linux names the object without opening it, so no right on the file
  // itself is needed, as for stat(2)
  #[cfg(any(target_os = "linux", target_os = "android"))]
  {
    let opts = CheckedOpenOptions::new().path_only(true);
    let file = open_checked(scope, path, &opts).map_err(fs_error)?;
    Ok(FsStat::from_std(file.into_std_file().metadata()?))
  }
  #[cfg(not(any(target_os = "linux", target_os = "android")))]
  {
    match metadata_fd(scope, path) {
      Ok(file) => Ok(FsStat::from_std(file.metadata()?)),
      Err(CheckedOpenError::NotCapable(path)) => Err(not_capable(&path)),
      // an object that can't be opened (no right on it, a socket): its
      // folder is checked, the last step is by name
      Err(CheckedOpenError::Io { source, .. })
        if matches!(
          source.raw_os_error(),
          Some(libc::EACCES | libc::EPERM | libc::EOPNOTSUPP | libc::ENXIO)
        ) =>
      {
        let real = checked_name(scope, path)?;
        Ok(FsStat::from_std(fs::metadata(real)?))
      }
      Err(err) => Err(fs_error(err)),
    }
  }
}

/// `lstat`: the entry itself.
pub fn lstat(scope: &OpenScope, path: &Path) -> FsResult<FsStat> {
  #[cfg(any(target_os = "linux", target_os = "android"))]
  {
    let opts = CheckedOpenOptions::new()
      .path_only(true)
      .symlinks(Symlinks::NoFollowFinal);
    let file = open_checked(scope, path, &opts).map_err(fs_error)?;
    Ok(FsStat::from_std(file.into_std_file().metadata()?))
  }
  #[cfg(not(any(target_os = "linux", target_os = "android")))]
  {
    let real = checked_name(scope, path)?;
    Ok(FsStat::from_std(fs::symlink_metadata(real)?))
  }
}

/// The entry's folder opened in the scope, and the entry's name there as
/// the kernel spells the folder now (for a last step that is by name).
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn checked_name(scope: &OpenScope, path: &Path) -> FsResult<PathBuf> {
  let parent = parent_checked(scope, path).map_err(fs_error)?;
  parent.real_path().ok_or_else(|| not_capable(path))
}

pub fn read_link(scope: &OpenScope, path: &Path) -> FsResult<PathBuf> {
  let parent = parent_checked(scope, path).map_err(fs_error)?;
  Ok(parent.read_link()?)
}

pub fn read_dir(scope: &OpenScope, path: &Path) -> FsResult<Vec<FsDirEntry>> {
  let opts = CheckedOpenOptions::new().read(true).directory(true);
  let dir = open_checked(scope, path, &opts).map_err(fs_error)?;
  Ok(
    dir
      .read_dir_kinds()?
      .into_iter()
      .map(|(name, kind)| FsDirEntry {
        // decoded lossily, as the path-based listing does
        name: name.to_string_lossy().into_owned(),
        is_file: kind == EntryKind::File,
        is_directory: kind == EntryKind::Dir,
        is_symlink: kind == EntryKind::Symlink,
      })
      .collect(),
  )
}

#[cfg(test)]
mod tests {
  use std::os::unix::fs::MetadataExt;
  use std::os::unix::fs::PermissionsExt;
  use std::os::unix::fs::symlink as make_link;

  use super::*;

  /// `<tmp>/allowed` (the write and read root) holding a denied `secret`
  /// folder, links to it and to `<tmp>/outside`.
  struct Bed {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    scope: OpenScope,
  }

  fn bed() -> Bed {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let a = root.join("allowed");
    fs::create_dir_all(a.join("sub")).unwrap();
    fs::create_dir_all(a.join("secret")).unwrap();
    fs::create_dir_all(root.join("outside")).unwrap();
    fs::write(a.join("file"), "file").unwrap();
    fs::write(a.join("secret/key"), "key").unwrap();
    fs::write(root.join("outside/data"), "data").unwrap();
    make_link(root.join("outside"), a.join("to-outside")).unwrap();
    make_link("secret", a.join("to-secret")).unwrap();
    make_link("file", a.join("to-file")).unwrap();
    let scope =
      OpenScope::new(Some(vec![a.clone()]), vec![a.join("secret")]).unwrap();
    Bed {
      _tmp: tmp,
      root,
      scope,
    }
  }

  impl Bed {
    fn p(&self, rel: &str) -> PathBuf {
      self.root.join(rel)
    }
  }

  fn refused<T>(result: FsResult<T>) -> bool {
    matches!(result, Err(FsError::PermissionCheck(_)))
  }

  fn write_opts() -> OpenOptions {
    OpenOptions {
      read: false,
      write: true,
      create: true,
      truncate: true,
      append: false,
      create_new: false,
      custom_flags: None,
      mode: None,
    }
  }

  /// Every path that leads out of the scope or into the deny, by name and
  /// through a link in the allowed folder.
  const ESCAPES: &[&str] = &[
    "outside/data",
    "allowed/to-outside/data",
    "allowed/secret/key",
    "allowed/to-secret/key",
  ];

  #[test]
  fn opens_stay_in_the_scope() {
    let b = bed();
    let mut file = open(&b.scope, &b.p("allowed/new"), write_opts()).unwrap();
    {
      use std::io::Write;
      file.write_all(b"new").unwrap();
    }
    assert_eq!(fs::read(b.p("allowed/new")).unwrap(), b"new");
    // through a link that stays inside
    let read = OpenOptions::read();
    let mut text = String::new();
    {
      use std::io::Read;
      open(&b.scope, &b.p("allowed/to-file"), read)
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    }
    assert_eq!(text, "file");
    for path in ESCAPES {
      assert!(refused(open(&b.scope, &b.p(path), read)), "{path}");
      assert!(refused(open(&b.scope, &b.p(path), write_opts())), "{path}");
    }
    assert_eq!(fs::read(b.p("outside/data")).unwrap(), b"data");
    assert_eq!(fs::read(b.p("allowed/secret/key")).unwrap(), b"key");
    // node's O_NOFOLLOW refuses a link as the last component
    let nofollow = OpenOptions {
      custom_flags: Some(libc::O_NOFOLLOW),
      ..read
    };
    let err = open(&b.scope, &b.p("allowed/to-file"), nofollow).unwrap_err();
    assert_eq!(err.into_io_error().raw_os_error(), Some(libc::ELOOP));
    // truncation needs write access, as before
    let read_truncate = OpenOptions {
      truncate: true,
      ..read
    };
    let err = open(&b.scope, &b.p("allowed/file"), read_truncate).unwrap_err();
    assert_eq!(err.into_io_error().raw_os_error(), Some(libc::EINVAL));
    assert_eq!(fs::read(b.p("allowed/file")).unwrap(), b"file");
    // O_RDONLY | O_CREAT creates, then reads
    let create_read = OpenOptions {
      read: true,
      write: false,
      create: true,
      ..write_opts()
    };
    let create_read = OpenOptions {
      truncate: false,
      ..create_read
    };
    open(&b.scope, &b.p("allowed/made"), create_read).unwrap();
    assert!(b.p("allowed/made").exists());
  }

  #[test]
  fn name_operations_stay_in_the_scope() {
    let b = bed();
    let s = &b.scope;
    mkdir(s, &b.p("allowed/a/b/c"), true, None).unwrap();
    assert!(b.p("allowed/a/b/c").is_dir());
    // again: already there
    mkdir(s, &b.p("allowed/a/b/c"), true, None).unwrap();
    assert!(mkdir(s, &b.p("allowed/a/b/c"), false, None).is_err());
    fs::write(b.p("allowed/a/b/f"), "x").unwrap();
    rename(s, &b.p("allowed/a/b/f"), s, &b.p("allowed/a/g")).unwrap();
    link(s, &b.p("allowed/a/g"), s, &b.p("allowed/a/h")).unwrap();
    assert_eq!(fs::metadata(b.p("allowed/a/g")).unwrap().nlink(), 2);
    symlink(s, Path::new("g"), &b.p("allowed/a/l")).unwrap();
    assert_eq!(
      read_link(s, &b.p("allowed/a/l")).unwrap(),
      PathBuf::from("g")
    );
    truncate(s, &b.p("allowed/a/g"), 0).unwrap();
    assert_eq!(fs::metadata(b.p("allowed/a/g")).unwrap().len(), 0);
    // truncation never follows a link as the last component
    assert!(truncate(s, &b.p("allowed/a/l"), 0).is_err());
    let listed = read_dir(s, &b.p("allowed/a")).unwrap();
    let kind = |name: &str| {
      let e = listed.iter().find(|e| e.name == name).unwrap();
      (e.is_file, e.is_directory, e.is_symlink)
    };
    assert_eq!(kind("g"), (true, false, false));
    assert_eq!(kind("b"), (false, true, false));
    assert_eq!(kind("l"), (false, false, true));
    assert!(stat(s, &b.p("allowed/a/l")).unwrap().is_file);
    assert!(lstat(s, &b.p("allowed/a/l")).unwrap().is_symlink);
    rmdir(s, &b.p("allowed/a/b/c")).unwrap();
    remove(s, &b.p("allowed/a/h"), false).unwrap();
    // a link to the outside inside the tree: removed as a link
    make_link(b.p("outside"), b.p("allowed/a/out")).unwrap();
    remove(s, &b.p("allowed/a"), true).unwrap();
    assert!(!b.p("allowed/a").exists());
    assert!(b.p("outside/data").exists());
    for path in ESCAPES {
      let path = b.p(path);
      assert!(refused(remove(s, &path, false)), "{path:?}");
      assert!(refused(truncate(s, &path, 0)), "{path:?}");
      assert!(refused(stat(s, &path)), "{path:?}");
      assert!(refused(lstat(s, &path)), "{path:?}");
      assert!(refused(read_link(s, &path)), "{path:?}");
      assert!(refused(rename(s, &path, s, &b.p("allowed/x"))), "{path:?}");
      assert!(
        refused(rename(s, &b.p("allowed/file"), s, &path)),
        "{path:?}"
      );
      assert!(refused(link(s, &path, s, &b.p("allowed/x"))), "{path:?}");
      assert!(refused(symlink(s, Path::new("t"), &path)), "{path:?}");
    }
    for dir in ["outside", "allowed/to-outside", "allowed/secret"] {
      assert!(refused(read_dir(s, &b.p(dir))), "{dir}");
      assert!(
        refused(mkdir(s, &b.p(dir).join("made"), true, None)),
        "{dir}"
      );
      assert!(!b.p(dir).join("made").exists());
    }
    // a recursive mkdir never makes a folder outside the scope on its way
    let outside_scope =
      OpenScope::new(Some(vec![b.p("allowed/deep/er")]), vec![]).unwrap();
    assert!(
      mkdir(&outside_scope, &b.p("allowed/deep/er/x"), true, None).is_err()
    );
    assert!(!b.p("allowed/deep").exists());
    assert_eq!(fs::read(b.p("outside/data")).unwrap(), b"data");
    assert_eq!(fs::read(b.p("allowed/secret/key")).unwrap(), b"key");
  }

  #[test]
  fn metadata_changes_stay_in_the_scope() {
    let b = bed();
    let s = &b.scope;
    chmod(s, &b.p("allowed/file"), 0o600).unwrap();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&b.p("allowed/file")), 0o600);
    // through a link that stays inside
    chmod(s, &b.p("allowed/to-file"), 0o640).unwrap();
    assert_eq!(mode(&b.p("allowed/file")), 0o640);
    // a file that can't be read still takes a mode (opened for writing)
    chmod(s, &b.p("allowed/file"), 0o200).unwrap();
    chmod(s, &b.p("allowed/file"), 0o644).unwrap();
    // a folder
    chmod(s, &b.p("allowed/sub"), 0o700).unwrap();
    assert_eq!(mode(&b.p("allowed/sub")), 0o700);
    utime(s, &b.p("allowed/file"), (1000, 7), (2000, 9)).unwrap();
    let meta = fs::metadata(b.p("allowed/file")).unwrap();
    assert_eq!((meta.atime(), meta.mtime()), (1000, 2000));
    assert_eq!(meta.mtime_nsec(), 9);
    // the link's own times, not its target's
    lutime(s, &b.p("allowed/to-file"), (5, 0), (6, 0)).unwrap();
    assert_eq!(fs::metadata(b.p("allowed/file")).unwrap().mtime(), 2000);
    assert_eq!(
      fs::symlink_metadata(b.p("allowed/to-file"))
        .unwrap()
        .mtime(),
      6
    );
    chown(s, &b.p("allowed/file"), None, None).unwrap();
    lchown(s, &b.p("allowed/to-file"), None, None).unwrap();
    let before = mode(&b.p("outside/data"));
    for path in ESCAPES {
      let path = b.p(path);
      assert!(refused(chmod(s, &path, 0o777)), "{path:?}");
      assert!(refused(chown(s, &path, None, None)), "{path:?}");
      assert!(refused(lchown(s, &path, None, None)), "{path:?}");
      assert!(refused(utime(s, &path, (1, 0), (1, 0))), "{path:?}");
      assert!(refused(lutime(s, &path, (1, 0), (1, 0))), "{path:?}");
    }
    assert_eq!(mode(&b.p("outside/data")), before);
    assert_ne!(fs::metadata(b.p("outside/data")).unwrap().mtime(), 1);
  }
}
