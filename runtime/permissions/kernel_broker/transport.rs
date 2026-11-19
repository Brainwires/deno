// Copyright 2018-2026 the Deno authors. MIT license.

//! Broker frames over a connected `AF_UNIX` `SOCK_STREAM` socket: a header,
//! then the payload its length names; a descriptor travels as `SCM_RIGHTS`
//! with the frame's first byte. Both sides use these two functions, so what
//! one writes is what the other reads.

use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::BorrowedFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;

use super::protocol;
use super::protocol::HEADER_LEN;
use super::protocol::Header;
use super::protocol::ProtocolError;

/// A frame as read: the header, the payload, and the descriptors that came
/// with it (close-on-exec). Dropping it closes them.
#[derive(Debug)]
pub struct Frame {
  pub header: Header,
  pub payload: Vec<u8>,
  pub fds: Vec<OwnedFd>,
}

/// Why a frame couldn't be read.
#[derive(Debug)]
pub enum RecvError {
  /// The peer closed the channel between frames.
  Closed,
  Io(io::Error),
  /// Not a frame (a bad header, or the channel ended inside one).
  Protocol(ProtocolError),
}

impl From<io::Error> for RecvError {
  fn from(err: io::Error) -> Self {
    RecvError::Io(err)
  }
}

impl From<RecvError> for io::Error {
  fn from(err: RecvError) -> Self {
    match err {
      RecvError::Closed => io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "the kernel sandbox broker closed the channel",
      ),
      RecvError::Io(err) => err,
      RecvError::Protocol(err) => io::Error::new(
        io::ErrorKind::InvalidData,
        format!("the kernel sandbox broker sent a malformed frame: {err}"),
      ),
    }
  }
}

/// A new channel: a connected, close-on-exec stream socket pair.
pub fn channel_pair() -> io::Result<(OwnedFd, OwnedFd)> {
  let mut fds = [0 as libc::c_int; 2];
  #[cfg(any(target_os = "linux", target_os = "android"))]
  let ty = libc::SOCK_STREAM | libc::SOCK_CLOEXEC;
  #[cfg(not(any(target_os = "linux", target_os = "android")))]
  let ty = libc::SOCK_STREAM;
  // SAFETY: an array of two descriptors for the kernel to fill
  if unsafe { libc::socketpair(libc::AF_UNIX, ty, 0, fds.as_mut_ptr()) } < 0 {
    return Err(io::Error::last_os_error());
  }
  // SAFETY: just created, owned from here
  let pair =
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
  #[cfg(not(any(target_os = "linux", target_os = "android")))]
  for fd in [&pair.0, &pair.1] {
    // SAFETY: a valid descriptor
    unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
    // a write to a closed peer is an error, never a signal
    #[cfg(target_os = "macos")]
    {
      let one: libc::c_int = 1;
      // SAFETY: a valid socket and option value
      unsafe {
        libc::setsockopt(
          fd.as_raw_fd(),
          libc::SOL_SOCKET,
          libc::SO_NOSIGPIPE,
          &one as *const libc::c_int as *const libc::c_void,
          std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
      };
    }
  }
  Ok(pair)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
const SEND_FLAGS: libc::c_int = libc::MSG_NOSIGNAL;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
const SEND_FLAGS: libc::c_int = 0;

/// Writes one encoded frame, `fd` (if any) with its first byte.
pub fn send_frame(
  sock: BorrowedFd<'_>,
  frame: &[u8],
  fd: Option<BorrowedFd<'_>>,
) -> io::Result<()> {
  debug_assert!(frame.len() >= HEADER_LEN);
  let mut sent = 0usize;
  let mut pass = fd;
  while sent < frame.len() {
    let mut iov = libc::iovec {
      iov_base: frame[sent..].as_ptr() as *mut libc::c_void,
      iov_len: frame.len() - sent,
    };
    // room for one descriptor
    let mut control = [0u64; 4];
    // SAFETY: a msghdr over buffers that outlive the call; the control
    // message is written through the CMSG macros within the buffer
    let n = unsafe {
      let mut msg: libc::msghdr = std::mem::zeroed();
      msg.msg_iov = &mut iov;
      msg.msg_iovlen = 1;
      if let Some(fd) = pass {
        let space =
          libc::CMSG_SPACE(std::mem::size_of::<libc::c_int>() as u32) as usize;
        debug_assert!(space <= std::mem::size_of_val(&control));
        msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = space as _;
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len =
          libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as u32) as _;
        std::ptr::write_unaligned(
          libc::CMSG_DATA(cmsg) as *mut libc::c_int,
          fd.as_raw_fd(),
        );
      }
      libc::sendmsg(sock.as_raw_fd(), &msg, SEND_FLAGS)
    };
    if n < 0 {
      let err = io::Error::last_os_error();
      if err.kind() == io::ErrorKind::Interrupted {
        continue;
      }
      return Err(err);
    }
    if n == 0 {
      return Err(io::ErrorKind::WriteZero.into());
    }
    // the descriptor went with the first bytes
    pass = None;
    sent += n as usize;
  }
  Ok(())
}

/// Reads up to `buf.len()` bytes, collecting any descriptors sent with
/// them. `Ok(0)`: the peer closed.
fn recv_some(
  sock: BorrowedFd<'_>,
  buf: &mut [u8],
  fds: &mut Vec<OwnedFd>,
) -> io::Result<usize> {
  loop {
    let mut iov = libc::iovec {
      iov_base: buf.as_mut_ptr() as *mut libc::c_void,
      iov_len: buf.len(),
    };
    // room for a few descriptors: more than the protocol allows are
    // received (and closed by the caller) rather than left in the socket
    let mut control = [0u64; 16];
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let flags = libc::MSG_CMSG_CLOEXEC;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let flags = 0;
    // SAFETY: a msghdr over stack buffers; the control messages are read
    // through the CMSG macros within msg_controllen
    let (n, msg_flags) = unsafe {
      let mut msg: libc::msghdr = std::mem::zeroed();
      msg.msg_iov = &mut iov;
      msg.msg_iovlen = 1;
      msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
      msg.msg_controllen = std::mem::size_of_val(&control) as _;
      let n = libc::recvmsg(sock.as_raw_fd(), &mut msg, flags);
      if n >= 0 {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
          if (*cmsg).cmsg_level == libc::SOL_SOCKET
            && (*cmsg).cmsg_type == libc::SCM_RIGHTS
          {
            let data = libc::CMSG_DATA(cmsg) as *const libc::c_int;
            let header = libc::CMSG_LEN(0) as usize;
            let len = ((*cmsg).cmsg_len as usize).saturating_sub(header);
            for i in 0..len / std::mem::size_of::<libc::c_int>() {
              let fd = std::ptr::read_unaligned(data.add(i));
              if fd >= 0 {
                let fd = OwnedFd::from_raw_fd(fd);
                // no MSG_CMSG_CLOEXEC here: set it at once (the window is
                // the one `ipc_pipe` documents)
                #[cfg(not(any(target_os = "linux", target_os = "android")))]
                libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
                fds.push(fd);
              }
            }
          }
          cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
      }
      (n, msg.msg_flags)
    };
    if n < 0 {
      let err = io::Error::last_os_error();
      if err.kind() == io::ErrorKind::Interrupted {
        continue;
      }
      return Err(err);
    }
    if msg_flags & libc::MSG_CTRUNC != 0 {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "more descriptors than a frame carries",
      ));
    }
    return Ok(n as usize);
  }
}

fn recv_exact(
  sock: BorrowedFd<'_>,
  buf: &mut [u8],
  fds: &mut Vec<OwnedFd>,
  at_frame_start: bool,
) -> Result<(), RecvError> {
  let mut got = 0usize;
  while got < buf.len() {
    let n = recv_some(sock, &mut buf[got..], fds)?;
    if n == 0 {
      return Err(if got == 0 && at_frame_start {
        RecvError::Closed
      } else {
        RecvError::Protocol(ProtocolError::Truncated)
      });
    }
    got += n;
  }
  Ok(())
}

/// Reads one frame: exactly a header and the payload it names, never a
/// byte of the next frame.
pub fn recv_frame(sock: BorrowedFd<'_>) -> Result<Frame, RecvError> {
  let mut fds = Vec::new();
  let mut head = [0u8; HEADER_LEN];
  recv_exact(sock, &mut head, &mut fds, true)?;
  let header = protocol::decode_header(&head).map_err(RecvError::Protocol)?;
  let mut payload = vec![0u8; header.len as usize];
  recv_exact(sock, &mut payload, &mut fds, false)?;
  Ok(Frame {
    header,
    payload,
    fds,
  })
}

#[cfg(test)]
mod tests {
  use std::os::fd::AsFd;

  use super::*;
  use crate::kernel_broker::protocol::Request;
  use crate::kernel_broker::protocol::Response;

  #[test]
  fn frames_cross_a_channel_with_their_descriptor() {
    let (a, b) = channel_pair().unwrap();
    let request = protocol::encode_request(5, &Request::Channel);
    send_frame(a.as_fd(), &request, None).unwrap();
    let frame = recv_frame(b.as_fd()).unwrap();
    assert_eq!(frame.header.id, 5);
    assert!(frame.fds.is_empty());
    assert_eq!(
      protocol::decode_request(&frame.header, &frame.payload).unwrap(),
      Request::Channel
    );
    // a response with a descriptor, then one without: each frame gets its
    // own
    let (x, _y) = channel_pair().unwrap();
    let with = protocol::encode_response(5, &Response::Ok { fd: true });
    let without = protocol::encode_response(6, &Response::Denied);
    send_frame(b.as_fd(), &with, Some(x.as_fd())).unwrap();
    send_frame(b.as_fd(), &without, None).unwrap();
    let first = recv_frame(a.as_fd()).unwrap();
    assert_eq!(first.fds.len(), 1);
    assert_eq!(first.header.id, 5);
    let second = recv_frame(a.as_fd()).unwrap();
    assert!(second.fds.is_empty());
    assert_eq!(second.header.id, 6);
    // a received descriptor is close-on-exec
    // SAFETY: a valid descriptor
    let flags = unsafe { libc::fcntl(first.fds[0].as_raw_fd(), libc::F_GETFD) };
    assert!(flags & libc::FD_CLOEXEC != 0);
  }

  #[test]
  fn a_closed_channel_and_a_cut_frame_are_told_apart() {
    let (a, b) = channel_pair().unwrap();
    drop(a);
    assert!(matches!(recv_frame(b.as_fd()), Err(RecvError::Closed)));
    let (a, b) = channel_pair().unwrap();
    let frame = protocol::encode_request(1, &Request::Channel);
    // SAFETY: a valid socket and buffer
    unsafe {
      libc::send(a.as_raw_fd(), frame.as_ptr() as *const libc::c_void, 7, 0)
    };
    drop(a);
    assert!(matches!(
      recv_frame(b.as_fd()),
      Err(RecvError::Protocol(ProtocolError::Truncated))
    ));
    // a write to a closed peer is an error, not a signal
    let (a, b) = channel_pair().unwrap();
    drop(b);
    assert!(send_frame(a.as_fd(), &frame, None).is_err());
  }
}
