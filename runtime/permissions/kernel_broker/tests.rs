// Copyright 2018-2026 the Deno authors. MIT license.

//! The broker end to end: a real [`Server`] with the file handler on one
//! end of a channel, the [`BrokerClient`] (or raw bytes) on the other.

#![allow(
  clippy::disallowed_methods,
  reason = "tests act on a real temporary directory"
)]

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::io::Write;
use std::os::fd::AsFd;
use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use super::client::BrokerClient;
use super::fs::FsBroker;
use super::fs::FsPolicy;
use super::protocol;
use super::protocol::OpenFlags;
use super::protocol::Remove;
use super::protocol::Request;
use super::protocol::Response;
use super::protocol::Time;
use super::protocol::WirePath;
use super::protocol::tests::Rng;
use super::protocol::tests::fuzz_iterations;
use super::protocol::tests::mutate;
use super::protocol::tests::sample_requests;
use super::server::Limits;
use super::server::Server;
use super::transport;

/// A tree with an allowed folder, a denied folder inside it, and a folder
/// outside every grant:
///
/// ```text
/// <root>/allowed/            write + read root
///        allowed/file
///        allowed/sub/inner
///        allowed/secret/     denied (read and write)
///        allowed/secret/key
///        allowed/to-outside -> <root>/outside
///        allowed/to-secret  -> secret
///        allowed/to-key     -> secret/key
///        allowed/to-file    -> file
///        allowed/ro-only/    (read root only: see `policy`)
/// <root>/outside/            no grant
///        outside/data
/// <root>/readonly/           read root, not write
///        readonly/doc
/// ```
struct Bed {
  _tmp: tempfile::TempDir,
  root: PathBuf,
}

impl Bed {
  fn new() -> Self {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let a = root.join("allowed");
    fs::create_dir_all(a.join("sub")).unwrap();
    fs::create_dir_all(a.join("secret")).unwrap();
    fs::create_dir_all(root.join("outside")).unwrap();
    fs::create_dir_all(root.join("readonly")).unwrap();
    fs::write(a.join("file"), "file").unwrap();
    fs::write(a.join("sub/inner"), "inner").unwrap();
    fs::write(a.join("secret/key"), "key").unwrap();
    fs::write(root.join("outside/data"), "data").unwrap();
    fs::write(root.join("readonly/doc"), "doc").unwrap();
    symlink(root.join("outside"), a.join("to-outside")).unwrap();
    symlink("secret", a.join("to-secret")).unwrap();
    symlink("secret/key", a.join("to-key")).unwrap();
    symlink("file", a.join("to-file")).unwrap();
    Self { _tmp: tmp, root }
  }

  fn p(&self, rel: &str) -> PathBuf {
    self.root.join(rel)
  }

  fn policy(&self) -> FsPolicy {
    FsPolicy {
      read: Some(vec![self.p("allowed"), self.p("readonly")]),
      deny_read: vec![self.p("allowed/secret")],
      write: Some(vec![self.p("allowed")]),
      deny_write: vec![self.p("allowed/secret")],
    }
  }

  /// Every file outside the write scope or inside the deny, with its
  /// content and mode: what no request may change.
  fn protected(&self) -> BTreeMap<PathBuf, (Vec<u8>, u32)> {
    let mut out = BTreeMap::new();
    for dir in ["outside", "readonly", "allowed/secret"] {
      snapshot(&self.p(dir), &mut out);
    }
    out
  }
}

fn snapshot(dir: &Path, out: &mut BTreeMap<PathBuf, (Vec<u8>, u32)>) {
  let meta = fs::symlink_metadata(dir).unwrap();
  out.insert(dir.to_path_buf(), (Vec::new(), meta.permissions().mode()));
  for entry in fs::read_dir(dir).unwrap() {
    let path = entry.unwrap().path();
    let meta = fs::symlink_metadata(&path).unwrap();
    if meta.is_dir() {
      snapshot(&path, out);
    } else if meta.is_file() {
      out.insert(
        path.clone(),
        (fs::read(&path).unwrap(), meta.permissions().mode()),
      );
    } else {
      out.insert(path.clone(), (Vec::new(), meta.permissions().mode()));
    }
  }
}

struct Live {
  server: Arc<Server<FsBroker>>,
  client: BrokerClient,
}

fn start(policy: &FsPolicy) -> Live {
  start_with(policy, Limits::default())
}

fn start_with(policy: &FsPolicy, limits: Limits) -> Live {
  let server = Server::with_limits(FsBroker::new(policy).unwrap(), limits);
  let (ours, theirs) = transport::channel_pair().unwrap();
  server.spawn(ours).unwrap();
  Live {
    server,
    client: BrokerClient::new(theirs),
  }
}

fn read_fd(fd: OwnedFd) -> String {
  let mut s = String::new();
  fs::File::from(fd).read_to_string(&mut s).unwrap();
  s
}

fn errno<T: std::fmt::Debug>(result: std::io::Result<T>) -> i32 {
  result.unwrap_err().raw_os_error().unwrap_or(-1)
}

const RW: OpenFlags = OpenFlags::READ.with(OpenFlags::WRITE);

#[test]
fn opens_inside_the_grant_and_refuses_the_deny_inside_it() {
  let bed = Bed::new();
  let live = start(&bed.policy());
  let c = &live.client;
  assert!(c.hello().unwrap() & protocol::features::FS != 0);
  // read, through a link that stays inside
  assert_eq!(
    read_fd(c.open(&bed.p("allowed/file"), OpenFlags::READ, 0).unwrap()),
    "file"
  );
  assert_eq!(
    read_fd(
      c.open(&bed.p("allowed/to-file"), OpenFlags::READ, 0)
        .unwrap()
    ),
    "file"
  );
  // a new file directly in the allowed folder (what Landlock's expansion
  // refuses the confined process itself)
  let created = c
    .open(
      &bed.p("allowed/new"),
      OpenFlags::WRITE.with(OpenFlags::CREATE_NEW),
      0o640,
    )
    .unwrap();
  fs::File::from(created).write_all(b"new").unwrap();
  assert_eq!(fs::read(bed.p("allowed/new")).unwrap(), b"new");
  // the descriptor that leaves is blocking, whatever the broker opened with
  let fd = c.open(&bed.p("allowed/new"), RW, 0).unwrap();
  // SAFETY: a valid descriptor
  let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
  assert_eq!(flags & libc::O_NONBLOCK, 0);
  assert_eq!(flags & libc::O_ACCMODE, libc::O_RDWR);
  // the deny inside the allow: by its name, through a link to the folder,
  // through a link to the file, for reading and for writing
  for path in [
    "allowed/secret/key",
    "allowed/to-secret/key",
    "allowed/to-key",
    "allowed/secret",
  ] {
    for flags in [OpenFlags::READ, OpenFlags::WRITE, RW] {
      assert_eq!(
        errno(c.open(&bed.p(path), flags, 0)),
        libc::EACCES,
        "{path} {flags:?}"
      );
    }
  }
  assert_eq!(
    errno(c.open(
      &bed.p("allowed/secret/planted"),
      OpenFlags::WRITE.with(OpenFlags::CREATE),
      0o600
    )),
    libc::EACCES
  );
  assert!(!bed.p("allowed/secret/planted").exists());
  // outside every grant: by name and through a link from inside
  for path in ["outside/data", "allowed/to-outside/data", "outside/new"] {
    assert_eq!(
      errno(c.open(
        &bed.p(path),
        OpenFlags::WRITE.with(OpenFlags::CREATE),
        0o600
      )),
      libc::EACCES,
      "{path}"
    );
  }
  assert!(!bed.p("outside/new").exists());
  // read-only root: reads, never writes, and a read + write open needs both
  assert_eq!(
    read_fd(c.open(&bed.p("readonly/doc"), OpenFlags::READ, 0).unwrap()),
    "doc"
  );
  assert_eq!(
    errno(c.open(&bed.p("readonly/doc"), OpenFlags::WRITE, 0)),
    libc::EACCES
  );
  assert_eq!(errno(c.open(&bed.p("readonly/doc"), RW, 0)), libc::EACCES);
  // no-follow refuses a link as the last component
  assert_eq!(
    errno(c.open(
      &bed.p("allowed/to-file"),
      OpenFlags::READ.with(OpenFlags::NOFOLLOW),
      0
    )),
    libc::ELOOP
  );
  // an OS error is the OS's
  assert_eq!(
    errno(c.open(&bed.p("allowed/missing"), OpenFlags::READ, 0)),
    libc::ENOENT
  );
  // a directory opens for listing
  let dir = c
    .open(
      &bed.p("allowed/sub"),
      OpenFlags::READ.with(OpenFlags::DIRECTORY),
      0,
    )
    .unwrap();
  assert!(fs::File::from(dir).metadata().unwrap().is_dir());
}

#[test]
fn name_operations_hold_the_deny() {
  let bed = Bed::new();
  let live = start(&bed.policy());
  let c = &live.client;
  let before = bed.protected();
  c.mkdir(&bed.p("allowed/made"), 0o755).unwrap();
  assert!(bed.p("allowed/made").is_dir());
  c.rename(&bed.p("allowed/file"), &bed.p("allowed/made/moved"), false)
    .unwrap();
  assert_eq!(fs::read(bed.p("allowed/made/moved")).unwrap(), b"file");
  // noreplace
  fs::write(bed.p("allowed/other"), "other").unwrap();
  assert_eq!(
    errno(c.rename(
      &bed.p("allowed/other"),
      &bed.p("allowed/made/moved"),
      true
    )),
    libc::EEXIST
  );
  c.link(&bed.p("allowed/other"), &bed.p("allowed/hard"))
    .unwrap();
  assert_eq!(fs::read(bed.p("allowed/hard")).unwrap(), b"other");
  c.truncate(&bed.p("allowed/other"), 2).unwrap();
  assert_eq!(fs::read(bed.p("allowed/other")).unwrap(), b"ot");
  c.remove(&bed.p("allowed/hard"), Remove::File).unwrap();
  c.remove(&bed.p("allowed/made/moved"), Remove::File)
    .unwrap();
  c.remove(&bed.p("allowed/made"), Remove::Dir).unwrap();
  assert!(!bed.p("allowed/made").exists());
  // into, out of and inside the deny; out of and into the outside
  let denied: &[(&str, &str)] = &[
    ("allowed/other", "allowed/secret/in"),
    ("allowed/secret/key", "allowed/out"),
    ("allowed/secret", "allowed/renamed"),
    ("allowed/other", "outside/in"),
    ("outside/data", "allowed/taken"),
    ("allowed/other", "allowed/to-outside/in"),
    ("allowed/other", "allowed/to-secret/in"),
    // read-only root: not a rename's source, nor a link's
    ("readonly/doc", "allowed/doc"),
  ];
  for (from, to) in denied {
    assert_eq!(
      errno(c.rename(&bed.p(from), &bed.p(to), false)),
      libc::EACCES,
      "rename {from} -> {to}"
    );
    assert_eq!(
      errno(c.link(&bed.p(from), &bed.p(to))),
      libc::EACCES,
      "link {from} -> {to}"
    );
  }
  for path in [
    "allowed/secret/dir",
    "outside/dir",
    "allowed/to-outside/dir",
    "allowed/to-secret/dir",
  ] {
    assert_eq!(errno(c.mkdir(&bed.p(path), 0o755)), libc::EACCES, "{path}");
  }
  for path in [
    "allowed/secret/key",
    "outside/data",
    "allowed/to-outside/data",
  ] {
    assert_eq!(
      errno(c.remove(&bed.p(path), Remove::File)),
      libc::EACCES,
      "{path}"
    );
    assert_eq!(errno(c.truncate(&bed.p(path), 0)), libc::EACCES, "{path}");
  }
  // a link is removed itself, never what it points at
  c.remove(&bed.p("allowed/to-key"), Remove::File).unwrap();
  // truncation never follows a link
  assert_eq!(errno(c.truncate(&bed.p("allowed/to-file"), 0)), libc::ELOOP);
  // a tree goes with everything in it, links removed as links
  fs::create_dir_all(bed.p("allowed/tree/deep")).unwrap();
  fs::write(bed.p("allowed/tree/deep/f"), "f").unwrap();
  symlink(bed.p("outside"), bed.p("allowed/tree/out")).unwrap();
  assert_eq!(
    errno(c.remove(&bed.p("allowed/tree"), Remove::Dir)),
    libc::ENOTEMPTY
  );
  c.remove(&bed.p("allowed/tree"), Remove::Tree).unwrap();
  assert!(!bed.p("allowed/tree").exists());
  // never the tree that holds the deny, nor one outside
  for path in ["allowed", "allowed/secret", "outside"] {
    assert_eq!(
      errno(c.remove(&bed.p(path), Remove::Tree)),
      libc::EACCES,
      "{path}"
    );
  }
  // a link to a folder is not a tree: it is never entered
  assert_eq!(
    errno(c.remove(&bed.p("allowed/to-outside"), Remove::Tree)),
    libc::ENOTDIR
  );
  // a folder that holds a denied path doesn't move (the deny would move
  // with it)
  fs::create_dir_all(bed.p("allowed/box")).unwrap();
  let mut policy = bed.policy();
  policy.deny_write.push(bed.p("allowed/box/inner-deny"));
  let strict = start(&policy);
  assert_eq!(
    errno(strict.client.rename(
      &bed.p("allowed/box"),
      &bed.p("allowed/box2"),
      false
    )),
    libc::EACCES
  );
  // creating a symlink takes unscoped read and write
  assert_eq!(
    errno(c.symlink(Path::new("/etc/passwd"), &bed.p("allowed/ln"))),
    libc::EACCES
  );
  assert!(fs::symlink_metadata(bed.p("allowed/ln")).is_err());
  assert_eq!(before, bed.protected());
}

#[test]
fn symlinks_need_the_whole_permission_and_still_hold_the_deny() {
  let bed = Bed::new();
  let policy = FsPolicy {
    read: None,
    deny_read: vec![],
    write: None,
    deny_write: vec![bed.p("allowed/secret")],
  };
  let live = start(&policy);
  let c = &live.client;
  c.symlink(Path::new("file"), &bed.p("allowed/ln")).unwrap();
  assert_eq!(
    fs::read_link(bed.p("allowed/ln")).unwrap(),
    Path::new("file")
  );
  assert_eq!(
    errno(c.symlink(Path::new("x"), &bed.p("allowed/secret/ln"))),
    libc::EACCES
  );
  // unscoped write: the outside is writable, the deny is not
  let fd = c
    .open(
      &bed.p("outside/new"),
      OpenFlags::WRITE.with(OpenFlags::CREATE),
      0o600,
    )
    .unwrap();
  drop(fd);
  assert!(bed.p("outside/new").exists());
  assert_eq!(
    errno(c.open(&bed.p("allowed/to-key"), OpenFlags::WRITE, 0)),
    libc::EACCES
  );
}

#[test]
fn the_kernels_own_trees_are_never_entered() {
  let policy = FsPolicy {
    read: None,
    deny_read: vec![],
    write: None,
    deny_write: vec![],
  };
  let live = start(&policy);
  let c = &live.client;
  for path in ["/dev/null", "/dev/zero", "/dev/fd/0", "/dev/stdin"] {
    assert_eq!(
      errno(c.open(Path::new(path), OpenFlags::READ, 0)),
      libc::EACCES,
      "{path}"
    );
  }
  #[cfg(target_os = "linux")]
  for path in [
    "/proc/self/environ",
    "/proc/self/fd/0",
    "/proc/self/root/etc/passwd",
    "/proc/self/cwd",
    "/sys/kernel",
  ] {
    assert_eq!(
      errno(c.open(Path::new(path), OpenFlags::READ, 0)),
      libc::EACCES,
      "{path}"
    );
  }
  // a link that leads there
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path().canonicalize().unwrap();
  symlink("/dev/null", root.join("null")).unwrap();
  assert_eq!(
    errno(c.open(&root.join("null"), OpenFlags::WRITE, 0)),
    libc::EACCES
  );
  #[cfg(target_os = "linux")]
  {
    symlink("/proc/self/fd", root.join("fds")).unwrap();
    assert_eq!(
      errno(c.open(&root.join("fds/0"), OpenFlags::READ, 0)),
      libc::EACCES
    );
  }
  // a FIFO with no reader doesn't hold the channel
  let fifo = root.join("fifo");
  let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
  // SAFETY: a valid C string
  assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
  assert_eq!(errno(c.open(&fifo, OpenFlags::WRITE, 0)), libc::ENXIO);
}

#[cfg(target_os = "linux")]
#[test]
fn metadata_changes_apply_to_the_checked_object() {
  let bed = Bed::new();
  let live = start(&bed.policy());
  let c = &live.client;
  let before = bed.protected();
  assert!(c.hello().unwrap() & protocol::features::FS_METADATA != 0);
  c.chmod(&bed.p("allowed/file"), 0o600, false).unwrap();
  assert_eq!(
    fs::metadata(bed.p("allowed/file"))
      .unwrap()
      .permissions()
      .mode()
      & 0o777,
    0o600
  );
  // through a link that stays inside
  c.chmod(&bed.p("allowed/to-file"), 0o640, false).unwrap();
  assert_eq!(
    fs::metadata(bed.p("allowed/file"))
      .unwrap()
      .permissions()
      .mode()
      & 0o777,
    0o640
  );
  // a file nobody can open still takes a mode (no read right is needed)
  c.chmod(&bed.p("allowed/file"), 0o000, false).unwrap();
  c.chmod(&bed.p("allowed/file"), 0o644, false).unwrap();
  let t = |sec| Time { sec, nsec: 5 };
  c.utime(&bed.p("allowed/file"), t(1000), t(2000), false)
    .unwrap();
  let meta = fs::metadata(bed.p("allowed/file")).unwrap();
  use std::os::unix::fs::MetadataExt;
  assert_eq!((meta.atime(), meta.mtime()), (1000, 2000));
  assert_eq!(meta.mtime_nsec(), 5);
  // keeping both ids is a no-op that still checks the path
  c.chown(&bed.p("allowed/file"), None, None, false).unwrap();
  for path in [
    "allowed/secret/key",
    "allowed/to-key",
    "outside/data",
    "allowed/to-outside/data",
    "readonly/doc",
  ] {
    assert_eq!(errno(c.chmod(&bed.p(path), 0o777, false)), libc::EACCES);
    assert_eq!(
      errno(c.chown(&bed.p(path), None, None, false)),
      libc::EACCES
    );
    assert_eq!(
      errno(c.utime(&bed.p(path), t(1), t(1), false)),
      libc::EACCES
    );
  }
  // no mode on a link itself
  assert_eq!(
    errno(c.chmod(&bed.p("allowed/to-file"), 0o600, true)),
    libc::EOPNOTSUPP
  );
  assert_eq!(before, bed.protected());
}

#[test]
fn channels_are_capped_and_independent() {
  let bed = Bed::new();
  let live = start_with(&bed.policy(), Limits { max_channels: 3 });
  // the control channel and two more
  let (a, fd_a) = live.client.request(&Request::Channel).unwrap();
  assert_eq!(a, Response::Ok { fd: true });
  let extra = fd_a.unwrap();
  // the pool took one channel for that request; this one is the third
  assert_eq!(live.server.channels(), 3);
  // a fourth is refused, and the channel that asked stays usable
  let frame = protocol::encode_request(1, &Request::Channel);
  transport::send_frame(extra.as_fd(), &frame, None).unwrap();
  let answer = transport::recv_frame(extra.as_fd()).unwrap();
  assert_eq!(
    protocol::decode_response(&answer.header, &answer.payload).unwrap(),
    Response::Errno(libc::EMFILE)
  );
  let frame = protocol::encode_request(
    2,
    &Request::Open {
      path: WirePath::new(
        bed.p("allowed/file").as_os_str().as_bytes().to_vec(),
      )
      .unwrap(),
      flags: OpenFlags::READ,
      mode: 0,
    },
  );
  transport::send_frame(extra.as_fd(), &frame, None).unwrap();
  let answer = transport::recv_frame(extra.as_fd()).unwrap();
  assert_eq!(answer.fds.len(), 1);
  // the descriptor is acknowledged before the channel does anything else
  let hello = protocol::encode_request(
    3,
    &Request::Hello {
      version: 1,
      features: 0,
    },
  );
  transport::send_frame(extra.as_fd(), &hello, None).unwrap();
  let refused = transport::recv_frame(extra.as_fd()).unwrap();
  assert_eq!(
    protocol::decode_response(&refused.header, &refused.payload).unwrap(),
    Response::Invalid
  );
  // closing a channel frees its slot
  drop(extra);
  for _ in 0..200 {
    if live.server.channels() == 2 {
      break;
    }
    std::thread::sleep(std::time::Duration::from_millis(5));
  }
  assert_eq!(live.server.channels(), 2);
}

#[test]
fn many_threads_share_the_client() {
  let bed = Bed::new();
  let live = Arc::new(start(&bed.policy()));
  let root = bed.p("allowed");
  let threads: Vec<_> = (0..24)
    .map(|t| {
      let live = live.clone();
      let root = root.clone();
      std::thread::spawn(move || {
        for i in 0..40 {
          let path = root.join(format!("t{t}-{i}"));
          let fd = live
            .client
            .open(&path, OpenFlags::WRITE.with(OpenFlags::CREATE_NEW), 0o600)
            .unwrap();
          fs::File::from(fd).write_all(b"x").unwrap();
          live.client.remove(&path, Remove::File).unwrap();
          assert_eq!(
            errno(live.client.open(
              &root.join("secret/key"),
              OpenFlags::READ,
              0
            )),
            libc::EACCES
          );
        }
      })
    })
    .collect();
  for t in threads {
    t.join().unwrap();
  }
}

/// Reads one response frame, or `None` when the channel closed.
fn answer(sock: &OwnedFd) -> Option<(Response, Vec<OwnedFd>)> {
  let frame = transport::recv_frame(sock.as_fd()).ok()?;
  let response =
    protocol::decode_response(&frame.header, &frame.payload).unwrap();
  Some((response, frame.fds))
}

#[test]
fn malformed_frames_close_the_channel_and_unknown_kinds_do_not() {
  let bed = Bed::new();
  let server = Server::new(FsBroker::new(&bed.policy()).unwrap());
  let connect = || {
    let (ours, theirs) = transport::channel_pair().unwrap();
    server.spawn(ours).unwrap();
    theirs
  };
  let send_raw = |sock: &OwnedFd, bytes: &[u8]| {
    // SAFETY: a valid socket and buffer
    unsafe {
      libc::send(
        sock.as_raw_fd(),
        bytes.as_ptr() as *const libc::c_void,
        bytes.len(),
        0,
      )
    };
  };
  // an unknown kind: Unsupported, and the channel goes on
  let sock = connect();
  let mut unknown = protocol::encode_request(1, &Request::Channel);
  unknown[4..6].copy_from_slice(&0x0100u16.to_le_bytes());
  send_raw(&sock, &unknown);
  assert_eq!(answer(&sock).unwrap().0, Response::Unsupported);
  send_raw(
    &sock,
    &protocol::encode_request(
      2,
      &Request::Hello {
        version: 9,
        features: u64::MAX,
      },
    ),
  );
  assert!(matches!(
    answer(&sock).unwrap().0,
    Response::Hello { version: 1, .. }
  ));
  // not this protocol
  send_raw(&sock, b"GET / HTTP/1.1\r\n\r\n");
  assert_eq!(answer(&sock).unwrap().0, Response::Invalid);
  assert!(answer(&sock).is_none());
  // a bad field
  let sock = connect();
  let mut bad = protocol::encode_request(
    3,
    &Request::Mkdir {
      path: WirePath::new(b"/a".to_vec()).unwrap(),
      mode: 0,
    },
  );
  let n = bad.len();
  bad[n - 6] = b'.'; // "/a" -> "/."
  send_raw(&sock, &bad);
  assert_eq!(answer(&sock).unwrap().0, Response::Invalid);
  assert!(answer(&sock).is_none());
  // a descriptor sent toward the broker
  let sock = connect();
  let frame = protocol::encode_request(4, &Request::Channel);
  transport::send_frame(sock.as_fd(), &frame, Some(sock.as_fd())).unwrap();
  assert_eq!(answer(&sock).unwrap().0, Response::Invalid);
  assert!(answer(&sock).is_none());
  // a response sent as a request
  let sock = connect();
  send_raw(&sock, &protocol::encode_response(5, &Response::Denied));
  assert_eq!(answer(&sock).unwrap().0, Response::Invalid);
  assert!(answer(&sock).is_none());
  // a frame cut short, then the channel closed: the server just leaves
  let sock = connect();
  send_raw(&sock, &protocol::encode_request(6, &Request::Channel)[..9]);
  drop(sock);
  for _ in 0..200 {
    if server.channels() == 0 {
      break;
    }
    std::thread::sleep(std::time::Duration::from_millis(5));
  }
  assert_eq!(server.channels(), 0);
}

/// Where a descriptor's object is, as the kernel names it.
fn real_path(fd: &OwnedFd) -> Option<PathBuf> {
  #[cfg(target_os = "linux")]
  {
    fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd())).ok()
  }
  #[cfg(target_os = "macos")]
  {
    let mut buf = vec![0u8; libc::PATH_MAX as usize + 1];
    // SAFETY: F_GETPATH writes at most PATH_MAX bytes
    let res =
      unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) };
    if res < 0 {
      return None;
    }
    let len = buf.iter().position(|b| *b == 0)?;
    buf.truncate(len);
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf)))
  }
}

/// What a descriptor the broker handed out may be: a file or folder in the
/// bed's allowed folder (or its read-only one) and outside the denied one,
/// or a new channel.
fn assert_within_policy(bed: &Bed, fd: &OwnedFd, context: &str) {
  // SAFETY: zeroed is a valid stat; the call fills it
  let mut st: libc::stat = unsafe { std::mem::zeroed() };
  // SAFETY: a valid descriptor and buffer
  assert_eq!(unsafe { libc::fstat(fd.as_raw_fd(), &mut st) }, 0);
  if st.st_mode & libc::S_IFMT == libc::S_IFSOCK {
    return; // a channel
  }
  let path = real_path(fd).unwrap_or_else(|| panic!("no path: {context}"));
  let text = path.to_string_lossy();
  let path = PathBuf::from(text.trim_end_matches(" (deleted)"));
  let allowed =
    path.starts_with(bed.p("allowed")) || path.starts_with(bed.p("readonly"));
  let denied = path.starts_with(bed.p("allowed/secret"));
  assert!(
    allowed && !denied,
    "a descriptor outside the policy: {} ({context})",
    path.display()
  );
  // a writable descriptor never names the read-only folder
  // SAFETY: a valid descriptor
  let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
  if flags & libc::O_ACCMODE != libc::O_RDONLY {
    assert!(
      path.starts_with(bed.p("allowed")),
      "a writable descriptor on {} ({context})",
      path.display()
    );
  }
}

/// The names the model's requests are built from: every interesting place
/// in the bed, by its own name and through the links.
fn vocabulary(bed: &Bed) -> Vec<PathBuf> {
  [
    "allowed",
    "allowed/file",
    "allowed/new",
    "allowed/new2",
    "allowed/dir",
    "allowed/dir/x",
    "allowed/sub",
    "allowed/sub/inner",
    "allowed/secret",
    "allowed/secret/key",
    "allowed/secret/new",
    "allowed/to-outside",
    "allowed/to-outside/data",
    "allowed/to-outside/new",
    "allowed/to-secret",
    "allowed/to-secret/key",
    "allowed/to-secret/new",
    "allowed/to-key",
    "allowed/to-file",
    "allowed/ln",
    "allowed/ln/data",
    "allowed/ln/key",
    "outside",
    "outside/data",
    "outside/new",
    "readonly",
    "readonly/doc",
    "readonly/new",
  ]
  .iter()
  .map(|rel| bed.p(rel))
  .chain(
    [
      "/",
      "/etc/passwd",
      "/dev/null",
      "/proc/self/environ",
      "/tmp",
    ]
    .iter()
    .map(PathBuf::from),
  )
  .collect()
}

fn random_request(rng: &mut Rng, bed: &Bed, vocab: &[PathBuf]) -> Request {
  let path = |rng: &mut Rng| {
    WirePath::new(
      vocab[rng.below(vocab.len())]
        .as_os_str()
        .as_bytes()
        .to_vec(),
    )
    .unwrap()
  };
  let flag = |rng: &mut Rng| rng.below(2) == 0;
  match rng.below(11) {
    0..=2 => loop {
      if let Some(flags) = OpenFlags::from_bits(rng.below(256) as u32) {
        break Request::Open {
          path: path(rng),
          flags,
          mode: [0o600, 0o644, 0o777, 0o4755][rng.below(4)],
        };
      }
    },
    3 => Request::Mkdir {
      path: path(rng),
      mode: 0o755,
    },
    4 => Request::Remove {
      path: path(rng),
      what: [Remove::File, Remove::Dir, Remove::Tree][rng.below(3)],
    },
    5 => Request::Rename {
      from: path(rng),
      to: path(rng),
      noreplace: flag(rng),
    },
    6 => {
      // a link from the bed to anywhere: the bed's own places, the
      // outside, the denied folder, relative ways up
      let targets: [&[u8]; 6] = [
        b"secret",
        b"secret/key",
        b"../outside",
        b"/etc",
        b"..",
        b"file",
      ];
      let outside = bed.p("outside");
      let target = if rng.below(4) == 0 {
        outside.as_os_str().as_bytes().to_vec()
      } else {
        targets[rng.below(targets.len())].to_vec()
      };
      Request::Symlink {
        target,
        link: path(rng),
      }
    }
    7 => Request::Link {
      existing: path(rng),
      new: path(rng),
    },
    8 => Request::Chmod {
      path: path(rng),
      mode: [0o600, 0o644, 0o777][rng.below(3)],
      nofollow: flag(rng),
    },
    9 => Request::Utime {
      path: path(rng),
      atime: Time { sec: 1, nsec: 0 },
      mtime: Time { sec: 2, nsec: 0 },
      nofollow: flag(rng),
    },
    _ => Request::Truncate {
      path: path(rng),
      len: rng.below(3) as u64,
    },
  }
}

/// The model test: no sequence of well-formed requests gets a descriptor
/// outside the policy or changes a file the policy protects. Symlinks are
/// the interesting part, so half the runs start with a link planted from
/// the allowed folder to the outside or the denied folder (as native code
/// in the confined process could plant one itself, the kernel permitting).
#[test]
fn model_no_request_sequence_leaves_the_policy() {
  let rounds = fuzz_iterations(60).clamp(8, 4000) / 4;
  let mut rng = Rng(0x6a09_e667_f3bc_c908);
  for round in 0..rounds {
    let bed = Bed::new();
    match round % 4 {
      1 => symlink(bed.p("outside"), bed.p("allowed/ln")).unwrap(),
      2 => symlink("secret", bed.p("allowed/ln")).unwrap(),
      3 => symlink("..", bed.p("allowed/ln")).unwrap(),
      _ => {}
    }
    let before = bed.protected();
    let live = start(&bed.policy());
    let vocab = vocabulary(&bed);
    for step in 0..400 {
      let request = random_request(&mut rng, &bed, &vocab);
      let context = format!("round {round} step {step}: {request:?}");
      let (response, fd) = live
        .client
        .request(&request)
        .unwrap_or_else(|err| panic!("{err}: {context}"));
      assert_ne!(response, Response::Invalid, "{context}");
      if let Some(fd) = fd {
        assert_within_policy(&bed, &fd, &context);
      }
      // the bed's policy scopes read and write: no symlink is ever made
      if matches!(request, Request::Symlink { .. }) {
        assert_eq!(response, Response::Denied, "{context}");
      }
    }
    // a request may have taken the search bit off the allowed folder
    // (its mode is the policy's to change); the check needs to walk it
    fs::set_permissions(bed.p("allowed"), fs::Permissions::from_mode(0o755))
      .unwrap();
    assert_eq!(before, bed.protected(), "round {round}");
  }
}

/// The next frame of a byte stream as the server reads it.
enum Next<'a> {
  /// Every byte was consumed.
  End,
  /// The stream ends inside a frame: the server waits for the rest.
  Partial,
  /// Not a frame, or a malformed one: `Invalid`, then the close.
  Malformed,
  /// A whole frame (`None`: of an unknown kind) and the bytes after it.
  Frame(Option<Request>, &'a [u8]),
}

fn next_frame(bytes: &[u8]) -> Next<'_> {
  if bytes.is_empty() {
    return Next::End;
  }
  let Some(head) = bytes.get(..protocol::HEADER_LEN) else {
    return Next::Partial;
  };
  let Ok(header) = protocol::decode_header(head.try_into().unwrap()) else {
    return Next::Malformed;
  };
  let body = &bytes[protocol::HEADER_LEN..];
  let Some(payload) = body.get(..header.len as usize) else {
    return Next::Partial;
  };
  let rest = &body[header.len as usize..];
  match protocol::decode_request(&header, payload) {
    Ok(request) => Next::Frame(Some(request), rest),
    Err(err) if err.is_unknown_kind() => Next::Frame(None, rest),
    Err(_) => Next::Malformed,
  }
}

/// The server fuzz: mutated frames and raw bytes on live channels, checked
/// frame by frame against the protocol's own rules ([`next_frame`]): the
/// server answers exactly the frames it should, closes exactly when it
/// should (never hangs, never dies), any descriptor it sends is inside the
/// policy, and nothing protected changes.
#[test]
fn fuzz_live_server() {
  let iterations = fuzz_iterations(20_000);
  let bed = Bed::new();
  let before = bed.protected();
  let server = Server::new(FsBroker::new(&bed.policy()).unwrap());
  let vocab = vocabulary(&bed);
  let mut rng = Rng(0xbb67_ae85_84ca_a73b);
  let mut samples: Vec<Vec<u8>> = sample_requests()
    .iter()
    .map(|r| protocol::encode_request(1, r))
    .collect();
  for _ in 0..64 {
    samples.push(protocol::encode_request(
      1,
      &random_request(&mut rng, &bed, &vocab),
    ));
  }
  let connect = || {
    let (ours, theirs) = transport::channel_pair().unwrap();
    // the fuzz opens and drops channels faster than their threads leave
    loop {
      match server.spawn(ours.try_clone().unwrap()) {
        Ok(_) => break,
        Err(_) => std::thread::sleep(std::time::Duration::from_millis(1)),
      }
    }
    // a server that stops answering fails the test instead of hanging it
    let timeout = libc::timeval {
      tv_sec: 120,
      tv_usec: 0,
    };
    // SAFETY: a valid socket and option value
    unsafe {
      libc::setsockopt(
        theirs.as_raw_fd(),
        libc::SOL_SOCKET,
        libc::SO_RCVTIMEO,
        &timeout as *const libc::timeval as *const libc::c_void,
        std::mem::size_of::<libc::timeval>() as libc::socklen_t,
      )
    };
    theirs
  };
  let hello = protocol::encode_request(
    0xfeed,
    &Request::Hello {
      version: 1,
      features: 0,
    },
  );
  let mut sock = connect();
  let (mut answered, mut closed, mut waiting) = (0usize, 0usize, 0usize);
  for i in 0..iterations {
    let mut bytes = if i % 16 == 0 {
      let n = rng.below(48);
      rng.bytes(n)
    } else {
      samples[rng.below(samples.len())].clone()
    };
    if i % 3 != 0 {
      for _ in 0..1 + rng.below(3) {
        mutate(&mut rng, &mut bytes);
      }
    }
    // then a well-formed Hello: whatever came before, the server answers
    // the stream frame by frame or closes
    bytes.extend_from_slice(&hello);
    // SAFETY: a valid socket and buffer
    let sent = unsafe {
      libc::send(
        sock.as_raw_fd(),
        bytes.as_ptr() as *const libc::c_void,
        bytes.len(),
        transport_send_flags(),
      )
    };
    assert_eq!(sent, bytes.len() as isize, "step {i}");
    // whether the last answer carried a descriptor (then the next frame
    // must acknowledge it)
    let mut unacknowledged = false;
    let mut rest = &bytes[..];
    let mut reconnect = false;
    loop {
      let invalid = match next_frame(rest) {
        Next::End => break,
        Next::Partial => {
          waiting += 1;
          reconnect = true;
          break;
        }
        Next::Malformed => true,
        Next::Frame(request, after) => {
          rest = after;
          match (unacknowledged, &request) {
            (true, Some(Request::Received)) => {
              unacknowledged = false;
              continue;
            }
            (false, Some(Request::Received)) | (true, _) => true,
            (false, _) => false,
          }
        }
      };
      let got = transport::recv_frame(sock.as_fd())
        .unwrap_or_else(|err| panic!("step {i}: {err:?}"));
      let response = protocol::decode_response(&got.header, &got.payload)
        .unwrap_or_else(|err| panic!("step {i}: {err}"));
      assert_eq!(got.fds.len(), response.fds(), "step {i}");
      for fd in &got.fds {
        assert_within_policy(&bed, fd, &format!("step {i}"));
      }
      assert_eq!(
        response == Response::Invalid,
        invalid,
        "step {i}: {response:?}"
      );
      answered += 1;
      unacknowledged = !got.fds.is_empty();
      if invalid {
        // nothing after an Invalid but the close
        assert!(
          matches!(
            transport::recv_frame(sock.as_fd()),
            Err(transport::RecvError::Closed | transport::RecvError::Io(_))
          ),
          "step {i}"
        );
        closed += 1;
        reconnect = true;
        break;
      }
    }
    if reconnect || unacknowledged {
      sock = connect();
    }
  }
  assert!(answered > iterations / 2, "{answered} answers");
  assert!(
    closed > 0 && waiting > 0,
    "{closed} closed, {waiting} waiting"
  );
  drop(sock);
  assert_eq!(before, bed.protected());
}

fn transport_send_flags() -> libc::c_int {
  #[cfg(target_os = "linux")]
  {
    libc::MSG_NOSIGNAL
  }
  #[cfg(not(target_os = "linux"))]
  {
    0
  }
}
