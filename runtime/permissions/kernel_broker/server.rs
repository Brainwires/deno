// Copyright 2018-2026 the Deno authors. MIT license.

//! The broker's side of a channel: read a request, answer it, repeat until
//! the channel closes. One thread per channel; [`Request::Channel`] starts
//! another (up to [`Limits::max_channels`]).
//!
//! The server trusts nothing it reads. A malformed frame, a descriptor sent
//! toward the broker or a transport error closes that channel and nothing
//! else; no request can make the server keep state for the requester, so a
//! closed channel leaves nothing behind. It never panics on input: the
//! release profile aborts on a panic, which would take the broker (and the
//! confined process's exact operations) down.

use std::io;
use std::os::fd::AsFd;
use std::os::fd::BorrowedFd;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use super::protocol;
use super::protocol::MAX_ERRNO;
use super::protocol::MAX_PAYLOAD;
use super::protocol::Request;
use super::protocol::Response;
use super::protocol::VERSION;
use super::transport;
use super::transport::RecvError;

/// A handler's answer: the response and the descriptor it announces.
#[derive(Debug)]
pub struct Reply {
  pub response: Response,
  pub fd: Option<OwnedFd>,
}

impl Reply {
  pub fn ok() -> Self {
    Self {
      response: Response::Ok { fd: false },
      fd: None,
    }
  }

  pub fn ok_fd(fd: OwnedFd) -> Self {
    Self {
      response: Response::Ok { fd: true },
      fd: Some(fd),
    }
  }

  pub fn denied() -> Self {
    Self {
      response: Response::Denied,
      fd: None,
    }
  }

  pub fn unsupported() -> Self {
    Self {
      response: Response::Unsupported,
      fd: None,
    }
  }

  /// The OS refused: `errno` clamped to what a response carries (`EIO` for
  /// anything else).
  pub fn errno(errno: i32) -> Self {
    let errno = if (1..=MAX_ERRNO).contains(&errno) {
      errno
    } else {
      libc::EIO
    };
    Self {
      response: Response::Errno(errno),
      fd: None,
    }
  }

  pub fn io_error(err: &io::Error) -> Self {
    Self::errno(err.raw_os_error().unwrap_or(libc::EIO))
  }

  pub fn data(bytes: Vec<u8>) -> Self {
    if bytes.len() > MAX_PAYLOAD - 4 {
      return Self::errno(libc::EMSGSIZE);
    }
    Self {
      response: Response::Data(bytes),
      fd: None,
    }
  }

  fn invalid() -> Self {
    Self {
      response: Response::Invalid,
      fd: None,
    }
  }
}

/// What answers the requests. `Hello` and `Channel` are the server's own.
pub trait Handler: Send + Sync + 'static {
  /// The [`protocol::features`] this handler serves.
  fn features(&self) -> u64;

  /// Answers one request. Whatever it returns is the handler's decision
  /// against its own policy: the request is the confined side's wish, never
  /// evidence of what is allowed.
  fn handle(&self, request: &Request) -> Reply;

  /// A well-formed frame of a kind [`protocol::decode_request`] doesn't
  /// know (the reserved network kinds, for a handler that has them).
  fn handle_unknown(&self, _kind: u16, _payload: &[u8]) -> Reply {
    Reply::unsupported()
  }
}

/// The server's bounds.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
  /// Channels served at once, the first one included. A `Channel` request
  /// past it fails with `EMFILE`.
  pub max_channels: usize,
}

impl Default for Limits {
  fn default() -> Self {
    Self { max_channels: 64 }
  }
}

pub struct Server<H> {
  handler: H,
  limits: Limits,
  channels: AtomicUsize,
}

impl<H: Handler> Server<H> {
  pub fn new(handler: H) -> Arc<Self> {
    Self::with_limits(handler, Limits::default())
  }

  pub fn with_limits(handler: H, limits: Limits) -> Arc<Self> {
    Arc::new(Self {
      handler,
      limits,
      channels: AtomicUsize::new(0),
    })
  }

  pub fn handler(&self) -> &H {
    &self.handler
  }

  /// The channels being served now.
  pub fn channels(&self) -> usize {
    self.channels.load(Ordering::SeqCst)
  }

  /// Serves `sock` on a new thread (the first channel, or one a test
  /// makes). Fails only when the thread can't start or the cap is reached.
  pub fn spawn(
    self: &Arc<Self>,
    sock: OwnedFd,
  ) -> io::Result<std::thread::JoinHandle<()>> {
    if !self.take_slot() {
      return Err(io::Error::from_raw_os_error(libc::EMFILE));
    }
    let server = self.clone();
    let spawned = std::thread::Builder::new()
      .name("deno-broker".to_string())
      .spawn(move || {
        server.run(sock.as_fd());
        server.channels.fetch_sub(1, Ordering::SeqCst);
      });
    if spawned.is_err() {
      self.channels.fetch_sub(1, Ordering::SeqCst);
    }
    spawned
  }

  /// Serves `sock` on this thread until it closes.
  pub fn serve(self: &Arc<Self>, sock: OwnedFd) {
    if !self.take_slot() {
      return;
    }
    self.run(sock.as_fd());
    self.channels.fetch_sub(1, Ordering::SeqCst);
  }

  fn take_slot(&self) -> bool {
    let mut n = self.channels.load(Ordering::SeqCst);
    loop {
      if n >= self.limits.max_channels {
        return false;
      }
      match self.channels.compare_exchange(
        n,
        n + 1,
        Ordering::SeqCst,
        Ordering::SeqCst,
      ) {
        Ok(_) => return true,
        Err(now) => n = now,
      }
    }
  }

  fn run(self: &Arc<Self>, sock: BorrowedFd<'_>) {
    // the descriptor of the last response, kept open until the requester
    // says it arrived (see the protocol's transport notes)
    let mut unacknowledged: Option<OwnedFd> = None;
    loop {
      let frame = match transport::recv_frame(sock) {
        Ok(frame) => frame,
        Err(RecvError::Closed | RecvError::Io(_)) => return,
        Err(RecvError::Protocol(_)) => {
          let _ = send(sock, 0, Reply::invalid());
          return;
        }
      };
      let id = frame.header.id;
      // nothing ever travels toward the broker but bytes
      if !frame.fds.is_empty() {
        let _ = send(sock, id, Reply::invalid());
        return;
      }
      let request = protocol::decode_request(&frame.header, &frame.payload);
      // after a descriptor, its acknowledgement and nothing else; and no
      // acknowledgement of nothing
      match (unacknowledged.take(), &request) {
        (Some(_), Ok(Request::Received)) => continue,
        (None, Ok(Request::Received)) | (Some(_), _) => {
          let _ = send(sock, id, Reply::invalid());
          return;
        }
        (None, _) => {}
      }
      let reply = match request {
        Ok(Request::Hello { version, features }) => Reply {
          response: Response::Hello {
            version: version.min(VERSION),
            features: features & self.handler.features(),
            max_payload: MAX_PAYLOAD as u32,
          },
          fd: None,
        },
        Ok(Request::Channel) => self.new_channel(),
        Ok(request) => self.handler.handle(&request),
        Err(err) if err.is_unknown_kind() => self
          .handler
          .handle_unknown(frame.header.kind, &frame.payload),
        Err(_) => {
          let _ = send(sock, id, Reply::invalid());
          return;
        }
      };
      match send(sock, id, reply) {
        Ok(sent) => unacknowledged = sent,
        Err(_) => return,
      }
    }
  }

  fn new_channel(self: &Arc<Self>) -> Reply {
    let (ours, theirs) = match transport::channel_pair() {
      Ok(pair) => pair,
      Err(err) => return Reply::io_error(&err),
    };
    match self.spawn(ours) {
      Ok(_) => Reply::ok_fd(theirs),
      Err(err) => Reply::io_error(&err),
    }
  }
}

/// Sends a reply and hands back the descriptor that went with it (the
/// caller keeps it open until it is acknowledged); one whose response and
/// descriptor disagree is an internal error and goes out as `EIO` with no
/// descriptor.
fn send(
  sock: BorrowedFd<'_>,
  id: u32,
  reply: Reply,
) -> io::Result<Option<OwnedFd>> {
  let reply = if reply.response.fds() == usize::from(reply.fd.is_some()) {
    reply
  } else {
    Reply::errno(libc::EIO)
  };
  let frame = protocol::encode_response(id, &reply.response);
  transport::send_frame(sock, &frame, reply.fd.as_ref().map(|fd| fd.as_fd()))?;
  Ok(reply.fd)
}
