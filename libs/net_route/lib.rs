// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: where the runtime's sockets come from under a kernel sandbox
//! (`--kernel-sandbox`).
//!
//! A process the kernel holds to a host-scoped network grant
//! (`--allow-net=example.com`) has no IP sockets of its own: no kernel
//! mechanism can scope a socket by host, so the confined process is refused
//! `socket(AF_INET*)` outright and every socket it uses is made by the
//! supervising broker outside the sandbox, which checks the grant, resolves
//! the name itself, connects to exactly the address it checked and passes
//! the descriptor over. This crate is the one seam every crate that opens a
//! socket goes through (`deno_net`, `deno_fetch`, `deno_node`, `deno_core`'s
//! libuv surface, the WebRTC transports, the Postgres client, the inspector
//! servers, ...):
//!
//! - [`NetRoute`]: the broker's network operations, as blocking calls that
//!   return sockets. [`install`]ed once, before any user code runs, by the
//!   process that applied a broker policy; [`route`] is `None` everywhere
//!   else, and every caller then opens its sockets as it always did.
//! - Listeners bound before confinement ([`add_prebound_tcp`],
//!   [`prebound_tcp`]): an address the grant names for listening is bound
//!   by the image that isn't confined yet and handed to the operation that
//!   asks for it, so a listen-only server needs no IP socket of its own on
//!   any kernel.
//!
//! It has no dependencies, so that every crate can take it, and no policy of
//! its own: what is allowed is decided by the broker (which never asks the
//! confined side) and by Deno's own checks before a caller gets here. A
//! caller that doesn't come through here under a broker policy isn't an
//! escape, it is a failure: the kernel refuses its `socket(2)`.

use std::io;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;

/// A socket handed over by the broker (or bound before confinement).
#[cfg(unix)]
pub type OwnedSock = std::os::fd::OwnedFd;
/// A socket handed over by the broker (or bound before confinement).
#[cfg(windows)]
pub type OwnedSock = std::os::windows::io::OwnedSocket;

/// A TCP connection to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpConnect {
  /// The address to connect to. The broker connects to exactly this
  /// address, after checking that the grant reaches it: as an address the
  /// grant names itself, or as one a granted name resolved to (in the
  /// broker's own resolution: [`NetRoute::resolve`], or of `host` now).
  pub addr: SocketAddr,
  /// The name the caller resolved `addr` from, when it knows it.
  pub host: Option<String>,
  /// The local address to bind first (Node.js' `localAddress`).
  pub local: Option<SocketAddr>,
}

/// A TCP listener to bind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpBind {
  pub addr: SocketAddr,
  /// The name the caller resolved `addr` from, when it knows it.
  pub host: Option<String>,
  pub reuse_port: bool,
  pub backlog: i32,
  /// `IPV6_V6ONLY`; `None`: the system's default.
  pub ipv6_only: Option<bool>,
}

/// A UDP socket to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpOpen {
  pub bind: SocketAddr,
  /// The one peer the socket talks to. A socket with no peer can send
  /// anywhere, so under a host-scoped grant the broker refuses it.
  pub connect: Option<SocketAddr>,
  /// The name the caller resolved `connect` from, when it knows it.
  pub host: Option<String>,
  pub reuse_address: bool,
  pub reuse_port: bool,
  /// `IPV6_V6ONLY`; `None`: the system's default.
  pub ipv6_only: Option<bool>,
}

/// The broker's network operations. Every call blocks until the broker
/// answers (a connect: until it connected or failed); async callers run it
/// on a blocking thread. A refusal is [`io::ErrorKind::PermissionDenied`].
/// Sockets come back in whatever blocking mode the broker left them in:
/// callers set the mode they need (the helpers below set non-blocking).
pub trait NetRoute: Send + Sync + 'static {
  /// The addresses of a name the grant names (for any port), as the broker
  /// resolves it. An IP literal is its own answer.
  fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>>;
  /// A connected TCP socket.
  fn connect_tcp(&self, request: &TcpConnect) -> io::Result<OwnedSock>;
  /// A bound, listening TCP socket.
  fn bind_tcp(&self, request: &TcpBind) -> io::Result<OwnedSock>;
  /// A bound (and, with a peer, connected) UDP socket. Under a host-scoped
  /// grant the broker hands out none: on Linux even a connected UDP socket
  /// can send to any address (`sendto` with a destination), so it would
  /// not hold the grant.
  fn open_udp(&self, request: &UdpOpen) -> io::Result<OwnedSock>;
  /// One datagram to `server` and the first datagram it answers with (a
  /// DNS query to a name server the grant names): the broker keeps the
  /// socket, so it can't be pointed anywhere else.
  fn udp_exchange(
    &self,
    server: SocketAddr,
    payload: &[u8],
    timeout: Duration,
  ) -> io::Result<Vec<u8>>;
  /// A connected Unix stream socket.
  fn connect_unix(&self, path: &Path) -> io::Result<OwnedSock>;
  /// A bound Unix socket: a listening stream socket, or a datagram one.
  fn bind_unix(&self, path: &Path, datagram: bool) -> io::Result<OwnedSock>;
}

static ROUTE: OnceLock<Box<dyn NetRoute>> = OnceLock::new();

/// Routes this process's sockets through `route` from now on. Once per
/// process, before any user code runs; a second call changes nothing and
/// returns `false` (a route can't be replaced or removed).
pub fn install(route: Box<dyn NetRoute>) -> bool {
  ROUTE.set(route).is_ok()
}

/// The broker's route, when this process runs under a broker policy.
pub fn route() -> Option<&'static dyn NetRoute> {
  ROUTE.get().map(|route| &**route)
}

/// A refusal, as every route reports one.
pub fn refused(why: impl std::fmt::Display) -> io::Error {
  io::Error::new(
    io::ErrorKind::PermissionDenied,
    format!("refused by the kernel sandbox's broker: {why}"),
  )
}

// ---------------------------------------------------------------------------
// Listeners bound before confinement
// ---------------------------------------------------------------------------

static PREBOUND: Mutex<Vec<(SocketAddr, OwnedSock)>> = Mutex::new(Vec::new());

/// Records a TCP listener bound (and listening) on `addr` before the
/// process confined itself. It stays bound for the life of the process:
/// every operation that listens on `addr` gets a duplicate, so listening
/// again after a close works, and so do several listeners on one address
/// (`reusePort`, `deno serve --parallel`).
pub fn add_prebound_tcp(addr: SocketAddr, listener: OwnedSock) {
  let mut prebound = PREBOUND.lock().unwrap_or_else(|e| e.into_inner());
  if !prebound.iter().any(|(a, _)| *a == addr) {
    prebound.push((addr, listener));
  }
}

/// The addresses bound before confinement.
pub fn prebound_addrs() -> Vec<SocketAddr> {
  PREBOUND
    .lock()
    .unwrap_or_else(|e| e.into_inner())
    .iter()
    .map(|(addr, _)| *addr)
    .collect()
}

/// A duplicate of the listener bound on exactly `addr` before confinement,
/// if there is one.
pub fn prebound_tcp(addr: &SocketAddr) -> io::Result<Option<OwnedSock>> {
  let prebound = PREBOUND.lock().unwrap_or_else(|e| e.into_inner());
  match prebound.iter().find(|(a, _)| a == addr) {
    Some((_, sock)) => Ok(Some(sock.try_clone()?)),
    None => Ok(None),
  }
}

// ---------------------------------------------------------------------------
// What the callers use
// ---------------------------------------------------------------------------

/// The addresses of `host` from the broker; `None` when this process's
/// sockets aren't routed (the caller resolves as it always did).
pub fn resolve(host: &str) -> Option<io::Result<Vec<IpAddr>>> {
  route().map(|route| route.resolve(host))
}

/// [`resolve`] with a port, in the shape most callers hold.
pub fn resolve_with_port(
  host: &str,
  port: u16,
) -> Option<io::Result<Vec<SocketAddr>>> {
  resolve(host).map(|addrs| {
    addrs.map(|addrs| {
      addrs
        .into_iter()
        .map(|ip| SocketAddr::new(ip, port))
        .collect()
    })
  })
}

/// A connected, non-blocking TCP stream from the broker; `None` when this
/// process's sockets aren't routed. Blocks until connected: call it on a
/// blocking thread.
pub fn connect_tcp(
  request: &TcpConnect,
) -> Option<io::Result<std::net::TcpStream>> {
  let route = route()?;
  Some(route.connect_tcp(request).and_then(|sock| {
    let stream = std::net::TcpStream::from(sock);
    stream.set_nonblocking(true)?;
    Ok(stream)
  }))
}

/// A listening, non-blocking TCP listener: the one bound on this address
/// before confinement, else one the broker binds; `None` when there is
/// neither (the caller binds as it always did).
pub fn tcp_listener(
  request: &TcpBind,
) -> Option<io::Result<std::net::TcpListener>> {
  let sock = match prebound_tcp(&request.addr) {
    Ok(Some(sock)) => Ok(sock),
    Ok(None) => route()?.bind_tcp(request),
    Err(err) => Err(err),
  };
  Some(sock.and_then(|sock| {
    let listener = std::net::TcpListener::from(sock);
    listener.set_nonblocking(true)?;
    Ok(listener)
  }))
}

/// A non-blocking UDP socket from the broker; `None` when this process's
/// sockets aren't routed.
pub fn open_udp(request: &UdpOpen) -> Option<io::Result<std::net::UdpSocket>> {
  let route = route()?;
  Some(route.open_udp(request).and_then(|sock| {
    let socket = std::net::UdpSocket::from(sock);
    socket.set_nonblocking(true)?;
    Ok(socket)
  }))
}

/// A connected, non-blocking Unix stream from the broker; `None` when this
/// process's sockets aren't routed.
#[cfg(unix)]
pub fn connect_unix(
  path: &Path,
) -> Option<io::Result<std::os::unix::net::UnixStream>> {
  let route = route()?;
  Some(route.connect_unix(path).and_then(|sock| {
    let stream = std::os::unix::net::UnixStream::from(sock);
    stream.set_nonblocking(true)?;
    Ok(stream)
  }))
}

/// A listening, non-blocking Unix listener from the broker; `None` when
/// this process's sockets aren't routed.
#[cfg(unix)]
pub fn unix_listener(
  path: &Path,
) -> Option<io::Result<std::os::unix::net::UnixListener>> {
  let route = route()?;
  Some(route.bind_unix(path, false).and_then(|sock| {
    let listener = std::os::unix::net::UnixListener::from(sock);
    listener.set_nonblocking(true)?;
    Ok(listener)
  }))
}

/// A bound, non-blocking Unix datagram socket from the broker; `None` when
/// this process's sockets aren't routed.
#[cfg(unix)]
pub fn unix_datagram(
  path: &Path,
) -> Option<io::Result<std::os::unix::net::UnixDatagram>> {
  let route = route()?;
  Some(route.bind_unix(path, true).and_then(|sock| {
    let socket = std::os::unix::net::UnixDatagram::from(sock);
    socket.set_nonblocking(true)?;
    Ok(socket)
  }))
}

#[cfg(test)]
mod tests {
  use std::io::Read;
  use std::io::Write;
  use std::net::Ipv4Addr;
  use std::net::TcpListener;
  use std::net::TcpStream;

  use super::*;

  #[test]
  fn unrouted_callers_get_nothing() {
    // no route is installed in this test binary
    assert!(route().is_none());
    assert!(resolve("example.com").is_none());
    assert!(
      connect_tcp(&TcpConnect {
        addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 9)),
        host: None,
        local: None,
      })
      .is_none()
    );
    assert!(
      open_udp(&UdpOpen {
        bind: SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        connect: None,
        host: None,
        reuse_address: false,
        reuse_port: false,
        ipv6_only: None,
      })
      .is_none()
    );
  }

  #[test]
  fn a_prebound_listener_is_handed_out_again_and_again() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    add_prebound_tcp(addr, listener.into());
    assert!(prebound_addrs().contains(&addr));
    let bind = TcpBind {
      addr,
      host: None,
      reuse_port: false,
      backlog: 16,
      ipv6_only: None,
    };
    for _ in 0..2 {
      // each caller gets its own descriptor of the one socket; closing it
      // leaves the address bound for the next
      let handed = tcp_listener(&bind).unwrap().unwrap();
      assert_eq!(handed.local_addr().unwrap(), addr);
      let mut client = TcpStream::connect(addr).unwrap();
      client.write_all(b"x").unwrap();
      handed.set_nonblocking(false).unwrap();
      let (mut accepted, _) = handed.accept().unwrap();
      let mut byte = [0u8; 1];
      accepted.read_exact(&mut byte).unwrap();
      assert_eq!(&byte, b"x");
    }
    // another address has no listener, and no route binds one
    let other = SocketAddr::from((Ipv4Addr::LOCALHOST, 1));
    assert!(prebound_tcp(&other).unwrap().is_none());
    assert!(
      tcp_listener(&TcpBind {
        addr: other,
        ..bind
      })
      .is_none()
    );
  }

  #[test]
  fn a_refusal_is_permission_denied() {
    let err = refused("example.org:443 is not in the grant");
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    assert!(err.to_string().contains("example.org:443"));
  }
}
