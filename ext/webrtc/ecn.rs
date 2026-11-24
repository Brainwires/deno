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
//! `IPV6_DONTFRAG`, Linux's `IP_MTU_DISCOVER` probe mode, as QUIC does, whose
//! own path MTU discovery copes with it). WebRTC has none: a datagram the
//! engine sizes for its 1200-byte RTP / SCTP MTU grows with SRTP, a TURN
//! header and the IP and UDP headers, and on a path whose MTU is smaller
//! (a tunnel, a VPN) it would be dropped instead of fragmented, so the
//! connection could connect and then carry nothing. Those options are put
//! back as the socket had them ([`FRAGMENTATION`]). Linux's UDP GRO, which
//! `quinn-udp` turns on, is turned back off: a coalesced read would need a
//! buffer 64 datagrams long.

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
  ///
  /// A Not-ECT datagram takes the plain `send_to`: it needs no control
  /// message (nothing sets the socket's own TOS, so it leaves as Not-ECT),
  /// and it is what goes to addresses nobody has answered from yet, an ICE
  /// check to every candidate the remote names. `sendmsg` to one the socket
  /// can't send to (Linux: a loopback socket to an off-host address, port 0)
  /// is `EINVAL`, which `quinn-udp` takes for a kernel that refuses its
  /// control messages: it then leaves the `IP_TOS` message out of every later
  /// IPv4 datagram of the socket, so the marks stop for good, and logs that
  /// it halts segmentation offload. Marked datagrams (a negotiated stream's
  /// RTP) go to the selected pair's address only, which has answered.
  pub(crate) fn try_send_to_ecn(
    &self,
    data: &[u8],
    to: SocketAddr,
    ecn: Option<EcnCodepoint>,
  ) -> io::Result<()> {
    let (Some(state), Some(ecn)) = (&self.state, ecn.and_then(to_quinn)) else {
      return self.socket.try_send_to(data, to).map(|_| ());
    };
    let transmit = Transmit {
      destination: to,
      ecn: Some(ecn),
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

/// The socket options (level, name) through which `quinn-udp` forbids IP
/// fragmentation; [`setup`] keeps the values the socket had.
#[cfg(any(target_os = "linux", target_os = "android"))]
const FRAGMENTATION: &[(i32, i32)] = &[
  (libc::IPPROTO_IP, libc::IP_MTU_DISCOVER),
  (libc::IPPROTO_IPV6, libc::IPV6_MTU_DISCOVER),
  (libc::IPPROTO_IPV6, libc::IPV6_DONTFRAG),
];
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
const FRAGMENTATION: &[(i32, i32)] = &[
  (libc::IPPROTO_IP, libc::IP_DONTFRAG),
  (libc::IPPROTO_IPV6, libc::IPV6_DONTFRAG),
];
#[cfg(windows)]
const FRAGMENTATION: &[(i32, i32)] = &[
  (
    windows_sys::Win32::Networking::WinSock::IPPROTO_IP,
    windows_sys::Win32::Networking::WinSock::IP_DONTFRAGMENT,
  ),
  (
    windows_sys::Win32::Networking::WinSock::IPPROTO_IPV6,
    windows_sys::Win32::Networking::WinSock::IPV6_DONTFRAG,
  ),
];
#[cfg(not(any(
  target_os = "linux",
  target_os = "android",
  target_os = "macos",
  target_os = "ios",
  target_os = "freebsd",
  windows
)))]
const FRAGMENTATION: &[(i32, i32)] = &[];

/// The values of the [`FRAGMENTATION`] options on `socket`, in order (`None`
/// for one the socket doesn't have, an IPv6 option on an IPv4 socket).
fn fragmentation<S: RawSocket>(socket: &S) -> Vec<Option<i32>> {
  FRAGMENTATION
    .iter()
    .map(|&(level, name)| get_option(socket, level, name))
    .collect()
}

#[cfg(unix)]
fn get_option<S: RawSocket>(socket: &S, level: i32, name: i32) -> Option<i32> {
  use std::os::fd::AsRawFd;
  let mut value: libc::c_int = 0;
  let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
  // SAFETY: a valid fd for the call's duration, and a c_int buffer of the
  // size passed.
  let rc = unsafe {
    libc::getsockopt(
      socket.as_fd().as_raw_fd(),
      level,
      name,
      &mut value as *mut libc::c_int as *mut libc::c_void,
      &mut len,
    )
  };
  (rc == 0).then_some(value)
}

#[cfg(unix)]
fn set_option<S: RawSocket>(socket: &S, level: i32, name: i32, value: i32) {
  use std::os::fd::AsRawFd;
  // SAFETY: a valid fd for the call's duration, and a c_int value of the
  // size passed.
  let rc = unsafe {
    libc::setsockopt(
      socket.as_fd().as_raw_fd(),
      level,
      name,
      &value as *const libc::c_int as *const libc::c_void,
      std::mem::size_of::<libc::c_int>() as libc::socklen_t,
    )
  };
  if rc != 0 {
    log::debug!(
      "webrtc: restoring socket option {level}/{name}: {}",
      io::Error::last_os_error()
    );
  }
}

#[cfg(windows)]
fn get_option<S: RawSocket>(socket: &S, level: i32, name: i32) -> Option<i32> {
  use std::os::windows::io::AsRawSocket;
  let mut value: i32 = 0;
  let mut len = std::mem::size_of::<i32>() as i32;
  // SAFETY: a valid socket for the call's duration, and an i32 buffer of the
  // size passed.
  let rc = unsafe {
    windows_sys::Win32::Networking::WinSock::getsockopt(
      socket.as_socket().as_raw_socket() as _,
      level,
      name,
      &mut value as *mut i32 as _,
      &mut len,
    )
  };
  (rc == 0).then_some(value)
}

#[cfg(windows)]
fn set_option<S: RawSocket>(socket: &S, level: i32, name: i32, value: i32) {
  use std::os::windows::io::AsRawSocket;
  // SAFETY: a valid socket for the call's duration, and an i32 value of the
  // size passed.
  let rc = unsafe {
    windows_sys::Win32::Networking::WinSock::setsockopt(
      socket.as_socket().as_raw_socket() as _,
      level,
      name,
      &value as *const i32 as _,
      std::mem::size_of::<i32>() as i32,
    )
  };
  if rc != 0 {
    log::debug!(
      "webrtc: restoring socket option {level}/{name}: {}",
      io::Error::last_os_error()
    );
  }
}

/// The ECN state for `socket`, or `None` when the platform refuses part of
/// it (logged; the socket keeps the plain path). The socket's fragmentation
/// options are as they were before ([`FRAGMENTATION`]).
fn setup<S: RawSocket>(socket: &S) -> Option<UdpSocketState> {
  let fragmentation_before = fragmentation(socket);
  let state = UdpSocketState::new(socket.into());
  for (&(level, name), before) in FRAGMENTATION.iter().zip(fragmentation_before)
  {
    if let Some(value) = before
      && get_option(socket, level, name) != Some(value)
    {
      set_option(socket, level, name, value);
    }
  }
  let state = match state {
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

#[cfg(test)]
mod tests {
  use super::*;

  // The ECN setup leaves the socket's IP fragmentation options as they were:
  // `quinn-udp` forbids fragmentation, which drops what a path with a small
  // MTU would otherwise carry fragmented.
  #[test]
  fn setup_keeps_the_fragmentation_options() {
    let socket =
      std::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let before = fragmentation(&socket);
    let _ = setup(&socket);
    assert_eq!(fragmentation(&socket), before);
  }

  // A datagram to an address the socket can't send to (an ICE check to a
  // candidate off this host, from a loopback socket: `EINVAL` on Linux)
  // leaves the socket's ECN as it was: the next marked datagram still
  // arrives marked. Through `quinn-udp` that failure turned the socket's
  // IPv4 marks off for good.
  #[cfg(unix)]
  #[tokio::test]
  async fn a_refused_unmarked_send_keeps_the_marks() {
    use std::net::Ipv4Addr;
    use std::time::Duration;

    let bind = || UdpSocket::bind((Ipv4Addr::LOCALHOST, 0));
    let sender = EcnUdpSocket::new(bind().await.unwrap());
    let receiver = EcnUdpSocket::new(bind().await.unwrap());
    let (Some(state), Some(_)) = (&sender.state, &receiver.state) else {
      // No per-datagram ECN here: nothing to keep.
      return;
    };
    let offload = state.max_gso_segments();
    sender.writable().await.unwrap();

    // TEST-NET-3 (RFC 5737), as WPT's failing candidate. Whether the send
    // fails is the platform's business; what it leaves behind is the test.
    let off_host = SocketAddr::from((Ipv4Addr::new(203, 0, 113, 100), 10100));
    let _ = sender.try_send_to_ecn(b"check", off_host, None);
    let port_zero = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let _ = sender.try_send_to_ecn(b"check", port_zero, None);
    assert_eq!(state.max_gso_segments(), offload);

    sender.writable().await.unwrap();
    sender
      .try_send_to_ecn(
        b"media",
        receiver.local_addr().unwrap(),
        Some(EcnCodepoint::Ect1),
      )
      .unwrap();
    let mut buf = [0u8; 16];
    let (len, from, ecn) = tokio::time::timeout(
      Duration::from_secs(5),
      receiver.recv_from(&mut buf),
    )
    .await
    .expect("the marked datagram arrives")
    .unwrap();
    assert_eq!(&buf[..len], b"media");
    assert_eq!(from, sender.local_addr().unwrap());
    assert_eq!(ecn, Some(EcnCodepoint::Ect1));
  }
}
