// Copyright 2018-2026 the Deno authors. MIT license.

//! ECN on the connection's UDP sockets (RFC 3168, L4S per RFC 9331).
//!
//! The engine is sans-I/O: it tells the socket layer which codepoint each
//! datagram it sends carries (`TransportContext::ecn` on `poll_write`) and
//! learns the codepoint each received datagram arrived with
//! (`TransportContext::ecn` on `handle_read`). This module does both halves
//! with `quinn-udp`, which reads the `IP_TOS` / `IPV6_TCLASS` control message
//! of every datagram (`IP_RECVTOS` / `IPV6_RECVTCLASS`; `IP_RECVECN` /
//! `IPV6_RECVECN` through `WSARecvMsg` on Windows) and sets the codepoint per
//! datagram as an `IP_TOS` / `IPV6_TCLASS` (Windows: `IP_ECN` /
//! `IPV6_ECN`) control message on `sendmsg` / `WSASendMsg`, so no socket
//! option has to follow the engine's codepoint.
//!
//! [`supported`] says whether this platform does both, which is when a
//! connection turns the engine's ECN on (`SettingEngineBuilder::with_ecn`):
//! Linux, Android, macOS, iOS and FreeBSD, and Windows where the network
//! stack has `WSARecvMsg`. A socket whose ECN setup fails keeps the plain
//! `recv_from` / `send_to` path (what it sends is then Not-ECT, which the
//! remote's feedback reports as bleached).
//!
//! `quinn-udp` also forbids IP fragmentation on the socket (`IP_DONTFRAG` /
//! `IP_MTU_DISCOVER` probe mode, as QUIC does): the engine's datagrams stay
//! under its 1200-byte RTP / SCTP MTU, so this changes nothing on a working
//! path. Linux's UDP GRO, which `quinn-udp` turns on, is turned back off: a
//! coalesced read would need a buffer 64 datagrams long.

use std::io;
use std::io::IoSliceMut;
use std::net::SocketAddr;
use std::sync::OnceLock;

use quinn_udp::RecvMeta;
use quinn_udp::Transmit;
use quinn_udp::UdpSocketState;
use rtc::shared::EcnCodepoint;
use tokio::io::Interest;
use tokio::net::UdpSocket;

/// The platforms whose `quinn-udp` reads and writes the codepoint.
const PLATFORM: bool = cfg!(any(
  target_os = "linux",
  target_os = "android",
  target_os = "macos",
  target_os = "ios",
  target_os = "freebsd",
  windows
));

/// A UDP socket and, when its ECN setup worked, the state that reads and
/// writes each datagram's codepoint.
pub(crate) struct EcnUdpSocket {
  socket: UdpSocket,
  state: Option<UdpSocketState>,
}

impl std::ops::Deref for EcnUdpSocket {
  type Target = UdpSocket;

  fn deref(&self) -> &UdpSocket {
    &self.socket
  }
}

impl EcnUdpSocket {
  /// The socket with ECN where [`supported`] (else as it is).
  pub(crate) fn new(socket: UdpSocket) -> Self {
    let state = if supported() { setup(&socket) } else { None };
    Self { socket, state }
  }

  /// The socket without ECN (the mDNS socket).
  pub(crate) fn plain(socket: UdpSocket) -> Self {
    Self {
      socket,
      state: None,
    }
  }

  /// Receives one datagram: its length, source and ECN codepoint (`None`:
  /// Not-ECT, or a socket without ECN).
  pub(crate) async fn recv_from(
    &self,
    buf: &mut [u8],
  ) -> io::Result<(usize, SocketAddr, Option<EcnCodepoint>)> {
    let Some(state) = &self.state else {
      let (n, from) = self.socket.recv_from(buf).await?;
      return Ok((n, from, None));
    };
    let mut meta = [RecvMeta::default()];
    self
      .socket
      .async_io(Interest::READABLE, || {
        state.recv(
          (&self.socket).into(),
          &mut [IoSliceMut::new(buf)],
          &mut meta,
        )
      })
      .await?;
    let meta = meta[0];
    // GRO is off (`setup`), so a read is one datagram.
    let len = meta.len.min(buf.len());
    Ok((len, meta.addr, meta.ecn.and_then(from_quinn)))
  }

  /// Sends one datagram with `ecn` (`None`: Not-ECT) without waiting: a full
  /// send buffer is a `WouldBlock` error, as with `try_send_to`.
  pub(crate) fn try_send_to_ecn(
    &self,
    data: &[u8],
    to: SocketAddr,
    ecn: Option<EcnCodepoint>,
  ) -> io::Result<()> {
    let Some(state) = &self.state else {
      return self.socket.try_send_to(data, to).map(|_| ());
    };
    let transmit = Transmit {
      destination: to,
      ecn: ecn.and_then(to_quinn),
      contents: data,
      segment_size: None,
      src_ip: None,
    };
    self.socket.try_io(Interest::WRITABLE, || {
      state.try_send((&self.socket).into(), &transmit)
    })
  }
}

fn to_quinn(ecn: EcnCodepoint) -> Option<quinn_udp::EcnCodepoint> {
  quinn_udp::EcnCodepoint::from_bits(ecn as u8)
}

fn from_quinn(ecn: quinn_udp::EcnCodepoint) -> Option<EcnCodepoint> {
  EcnCodepoint::from_bits(ecn as u8)
}

/// A socket handle `quinn-udp` takes.
#[cfg(unix)]
trait RawSocket: std::os::fd::AsFd {}
#[cfg(unix)]
impl<S: std::os::fd::AsFd> RawSocket for S {}
#[cfg(windows)]
trait RawSocket: std::os::windows::io::AsSocket {}
#[cfg(windows)]
impl<S: std::os::windows::io::AsSocket> RawSocket for S {}

/// The ECN state for `socket`, or `None` when the platform refuses part of
/// it (logged; the socket keeps the plain path).
fn setup<S: RawSocket>(socket: &S) -> Option<UdpSocketState> {
  let state = match UdpSocketState::new(socket.into()) {
    Ok(state) => state,
    Err(e) => {
      log::debug!("webrtc: ECN unavailable on a socket: {e}");
      return None;
    }
  };
  #[cfg(any(target_os = "linux", target_os = "android"))]
  if state.gro_segments() > 1 && !disable_gro(socket) {
    log::debug!("webrtc: ECN unavailable on a socket: UDP GRO stays on");
    return None;
  }
  Some(state)
}

/// Turns `UDP_GRO` off again; whether it is off.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn disable_gro<S: RawSocket>(socket: &S) -> bool {
  use std::os::fd::AsRawFd;
  // Android's libc has no `UDP_GRO` (quinn-udp hard-codes it too).
  #[cfg(target_os = "android")]
  const UDP_GRO: libc::c_int = 104;
  #[cfg(not(target_os = "android"))]
  const UDP_GRO: libc::c_int = libc::UDP_GRO;
  let off: libc::c_int = 0;
  // SAFETY: a valid fd for the call's duration and a c_int option value of
  // the size passed.
  let rc = unsafe {
    libc::setsockopt(
      socket.as_fd().as_raw_fd(),
      libc::SOL_UDP,
      UDP_GRO,
      &off as *const libc::c_int as *const libc::c_void,
      std::mem::size_of::<libc::c_int>() as libc::socklen_t,
    )
  };
  rc == 0
}

/// Whether this platform reads and writes per-datagram ECN codepoints: a
/// probe socket takes the setup (on Windows that needs `WSARecvMsg`). When
/// even a probe can't be bound (a sandbox), the platform's answer stands.
pub(crate) fn supported() -> bool {
  static SUPPORTED: OnceLock<bool> = OnceLock::new();
  *SUPPORTED.get_or_init(|| {
    if !PLATFORM {
      return false;
    }
    match std::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0)) {
      Ok(probe) => setup(&probe).is_some(),
      Err(_) => cfg!(not(windows)),
    }
  })
}
