// Copyright 2018-2026 the Deno authors. MIT license.

//! The confined side of the broker: the process-wide client the file
//! operations turn to for what the kernel sandbox refuses them but Deno's
//! rule allows (see [`super`]).
//!
//! The client holds one control channel (used only to ask for more) and a
//! small pool of working channels, one request on each at a time, so a slow
//! operation holds up nothing else. Everything fails closed: a channel that
//! misbehaves is dropped, and with the broker gone every brokered operation
//! is an error (the kernel sandbox still refuses what it refused).

use std::io;
use std::os::fd::AsFd;
use std::os::fd::BorrowedFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;

use parking_lot::Condvar;
use parking_lot::Mutex;

use super::protocol;
use super::protocol::CHOWN_KEEP;
use super::protocol::MAX_PAYLOAD;
use super::protocol::OpenFlags;
use super::protocol::Request;
use super::protocol::Response;
use super::protocol::Time;
use super::protocol::VERSION;
use super::protocol::WirePath;
use super::transport;

/// The working channels a client opens (the broker serves more than this).
const MAX_CHANNELS: usize = 16;

static CLIENT: OnceLock<BrokerClient> = OnceLock::new();

/// Makes `sock` (the confined end of the first channel) this process's
/// broker. Once per process; a second call gets its socket back.
pub fn install(sock: OwnedFd) -> Result<(), OwnedFd> {
  let mut slot = Some(sock);
  CLIENT.get_or_init(|| BrokerClient::new(slot.take().unwrap()));
  match slot {
    None => Ok(()),
    Some(sock) => Err(sock),
  }
}

/// This process's broker, when it runs under one.
pub fn get() -> Option<&'static BrokerClient> {
  CLIENT.get()
}

struct Pool {
  idle: Vec<OwnedFd>,
  /// Working channels that exist, idle or in use.
  total: usize,
}

pub struct BrokerClient {
  control: Mutex<Option<OwnedFd>>,
  pool: Mutex<Pool>,
  available: Condvar,
  next_id: AtomicU32,
}

fn protocol_error(what: &str) -> io::Error {
  io::Error::new(
    io::ErrorKind::InvalidData,
    format!("the kernel sandbox broker sent a malformed response: {what}"),
  )
}

fn gone() -> io::Error {
  io::Error::new(
    io::ErrorKind::BrokenPipe,
    "the kernel sandbox broker is gone",
  )
}

impl BrokerClient {
  pub fn new(control: OwnedFd) -> Self {
    Self {
      control: Mutex::new(Some(control)),
      pool: Mutex::new(Pool {
        idle: Vec::new(),
        total: 0,
      }),
      available: Condvar::new(),
      next_id: AtomicU32::new(1),
    }
  }

  /// One request and its response on `sock`.
  fn round_trip(
    &self,
    sock: BorrowedFd<'_>,
    request: &Request,
  ) -> io::Result<(Response, Option<OwnedFd>)> {
    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
    let frame = protocol::encode_request(id, request);
    transport::send_frame(sock, &frame, None)?;
    let mut frame = transport::recv_frame(sock)?;
    if frame.header.id != id {
      return Err(protocol_error("another request's answer"));
    }
    let response = protocol::decode_response(&frame.header, &frame.payload)
      .map_err(|err| protocol_error(&err.to_string()))?;
    if frame.fds.len() != response.fds()
      || (response.fds() == 1 && !request.returns_fd())
    {
      return Err(protocol_error("an unannounced descriptor"));
    }
    let fd = frame.fds.pop();
    if fd.is_some() {
      // the broker keeps its copy open until it hears this
      let ack = protocol::encode_request(id, &Request::Received);
      transport::send_frame(sock, &ack, None)?;
    }
    Ok((response, fd))
  }

  fn new_channel(&self) -> io::Result<OwnedFd> {
    let mut control = self.control.lock();
    let Some(sock) = control.as_ref() else {
      return Err(gone());
    };
    match self.round_trip(sock.as_fd(), &Request::Channel) {
      Ok((Response::Ok { fd: true }, Some(fd))) => Ok(fd),
      Ok((Response::Errno(errno), _)) => {
        Err(io::Error::from_raw_os_error(errno))
      }
      Ok(_) => Err(protocol_error("no channel")),
      Err(err) => {
        // the control channel is out of step: nothing more comes from it
        *control = None;
        Err(err)
      }
    }
  }

  fn checkout(&self) -> io::Result<OwnedFd> {
    let mut pool = self.pool.lock();
    loop {
      if let Some(sock) = pool.idle.pop() {
        return Ok(sock);
      }
      if pool.total < MAX_CHANNELS {
        pool.total += 1;
        drop(pool);
        return self.new_channel().inspect_err(|_| {
          self.pool.lock().total -= 1;
          self.available.notify_one();
        });
      }
      self.available.wait(&mut pool);
    }
  }

  /// Sends `request` and returns the broker's answer. An `Err` is the
  /// channel failing (the request may or may not have been carried out).
  pub fn request(
    &self,
    request: &Request,
  ) -> io::Result<(Response, Option<OwnedFd>)> {
    let sock = self.checkout()?;
    let result = self.round_trip(sock.as_fd(), request);
    let mut pool = self.pool.lock();
    match &result {
      // an Invalid answer closes the channel on the broker's side
      Ok((Response::Invalid, _)) | Err(_) => pool.total -= 1,
      Ok(_) => pool.idle.push(sock),
    }
    drop(pool);
    self.available.notify_one();
    result
  }

  /// The features the broker serves.
  pub fn hello(&self) -> io::Result<u64> {
    match self.request(&Request::Hello {
      version: VERSION,
      features: u64::MAX,
    })? {
      (
        Response::Hello {
          version: VERSION,
          features,
          max_payload,
        },
        _,
      ) if max_payload as usize >= MAX_PAYLOAD => Ok(features),
      _ => Err(protocol_error("not this version's broker")),
    }
  }

  fn done(&self, request: &Request) -> io::Result<()> {
    match self.request(request)? {
      (Response::Ok { fd: false }, _) => Ok(()),
      (other, _) => Err(refusal(other)),
    }
  }

  /// Opens `path` through the broker.
  pub fn open(
    &self,
    path: &Path,
    flags: OpenFlags,
    mode: u32,
  ) -> io::Result<OwnedFd> {
    let request = Request::Open {
      path: wire_path(path)?,
      flags,
      mode: mode & 0o7777,
    };
    match self.request(&request)? {
      (Response::Ok { fd: true }, Some(fd)) => Ok(fd),
      (other, _) => Err(refusal(other)),
    }
  }

  pub fn mkdir(&self, path: &Path, mode: u32) -> io::Result<()> {
    self.done(&Request::Mkdir {
      path: wire_path(path)?,
      mode: mode & 0o7777,
    })
  }

  pub fn remove(&self, path: &Path, dir: bool) -> io::Result<()> {
    self.done(&Request::Remove {
      path: wire_path(path)?,
      dir,
    })
  }

  pub fn rename(
    &self,
    from: &Path,
    to: &Path,
    noreplace: bool,
  ) -> io::Result<()> {
    self.done(&Request::Rename {
      from: wire_path(from)?,
      to: wire_path(to)?,
      noreplace,
    })
  }

  pub fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
    let target = target.as_os_str().as_bytes();
    if target.is_empty()
      || target.len() > protocol::MAX_PATH
      || target.contains(&0)
    {
      return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    self.done(&Request::Symlink {
      target: target.to_vec(),
      link: wire_path(link)?,
    })
  }

  pub fn link(&self, existing: &Path, new: &Path) -> io::Result<()> {
    self.done(&Request::Link {
      existing: wire_path(existing)?,
      new: wire_path(new)?,
    })
  }

  pub fn chmod(
    &self,
    path: &Path,
    mode: u32,
    nofollow: bool,
  ) -> io::Result<()> {
    self.done(&Request::Chmod {
      path: wire_path(path)?,
      mode: mode & 0o7777,
      nofollow,
    })
  }

  pub fn chown(
    &self,
    path: &Path,
    uid: Option<u32>,
    gid: Option<u32>,
    nofollow: bool,
  ) -> io::Result<()> {
    self.done(&Request::Chown {
      path: wire_path(path)?,
      uid: uid.unwrap_or(CHOWN_KEEP),
      gid: gid.unwrap_or(CHOWN_KEEP),
      nofollow,
    })
  }

  pub fn utime(
    &self,
    path: &Path,
    atime: Time,
    mtime: Time,
    nofollow: bool,
  ) -> io::Result<()> {
    self.done(&Request::Utime {
      path: wire_path(path)?,
      atime,
      mtime,
      nofollow,
    })
  }

  pub fn truncate(&self, path: &Path, len: u64) -> io::Result<()> {
    self.done(&Request::Truncate {
      path: wire_path(path)?,
      len,
    })
  }
}

/// A response that isn't the success the operation has, as the error the
/// operation fails with: the policy's refusal is `EACCES`, like the kernel
/// sandbox's own.
fn refusal(response: Response) -> io::Error {
  match response {
    Response::Denied => io::Error::from_raw_os_error(libc::EACCES),
    Response::Errno(errno) => io::Error::from_raw_os_error(errno),
    Response::Unsupported => io::Error::new(
      io::ErrorKind::Unsupported,
      "the kernel sandbox broker doesn't have this operation",
    ),
    Response::Invalid => protocol_error("it refused the request as malformed"),
    Response::Ok { .. } | Response::Hello { .. } | Response::Data(_) => {
      protocol_error("an answer to another operation")
    }
  }
}

/// `path` as the protocol takes it: absolute, with `.` and `..` resolved
/// by the text (as the permission checks resolve them).
pub fn wire_path(path: &Path) -> io::Result<WirePath> {
  let invalid = || io::Error::from_raw_os_error(libc::EINVAL);
  if !path.is_absolute() {
    return Err(invalid());
  }
  let mut out = PathBuf::from("/");
  for comp in path.components() {
    match comp {
      Component::RootDir | Component::Prefix(_) | Component::CurDir => {}
      Component::ParentDir => {
        out.pop();
      }
      Component::Normal(c) => out.push(c),
    }
  }
  WirePath::new(out.into_os_string().as_bytes().to_vec()).ok_or_else(|| {
    if path.as_os_str().len() > protocol::MAX_PATH {
      io::Error::from_raw_os_error(libc::ENAMETOOLONG)
    } else {
      invalid()
    }
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn wire_paths_are_normalized_by_their_text() {
    let w = |p: &str| {
      wire_path(Path::new(p))
        .map(|w| String::from_utf8(w.into_bytes()).unwrap())
        .map_err(|e| e.raw_os_error())
    };
    assert_eq!(w("/a/b").unwrap(), "/a/b");
    assert_eq!(w("/a/./b/").unwrap(), "/a/b");
    assert_eq!(w("/a/../b").unwrap(), "/b");
    assert_eq!(w("/../..").unwrap(), "/");
    assert_eq!(w("//a//b").unwrap(), "/a/b");
    assert_eq!(w("a/b"), Err(Some(libc::EINVAL)));
    let long = format!("/{}", "a".repeat(protocol::MAX_PATH));
    assert_eq!(w(&long), Err(Some(libc::ENAMETOOLONG)));
  }
}
