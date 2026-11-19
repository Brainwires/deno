// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: this crate's sockets under a kernel sandbox with a broker
//! (`deno_net_route`). A process confined to a host-scoped network grant
//! can't make an IP socket of its own; the operations here ask the broker
//! for one, and are what they always were when no route is installed.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;
use std::time::Duration;

use deno_net_route::TcpConnect;
use deno_net_route::UdpOpen;
use hickory_resolver::net::runtime::DnsUdpSocket;
use hickory_resolver::net::runtime::RuntimeProvider;
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::net::runtime::TokioTime;
use hickory_resolver::net::runtime::iocompat::AsyncIoTokioAsStd;
use tokio::net::TcpStream;
use tokio::task::JoinHandle;

/// The longest the broker waits for a name server's answer (the resolver
/// has its own, shorter, timeouts and retries).
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(10);

fn route_gone() -> io::Error {
  io::Error::other("the kernel sandbox's network route is gone")
}

/// Whether this process's sockets come from the broker.
pub fn is_routed() -> bool {
  deno_net_route::route().is_some()
}

/// A TCP connection to `addr`: from the broker when this process's sockets
/// are routed (`host` is the name `addr` was resolved from, when the
/// caller has it), else `TcpStream::connect`.
pub async fn connect_tcp_stream(
  addr: SocketAddr,
  host: Option<&str>,
) -> io::Result<TcpStream> {
  if !is_routed() {
    return TcpStream::connect(addr).await;
  }
  let request = TcpConnect {
    addr,
    host: host.map(str::to_string),
    local: None,
  };
  // the broker answers when it connected (or failed): off this thread
  let stream =
    tokio::task::spawn_blocking(move || deno_net_route::connect_tcp(&request))
      .await
      .map_err(io::Error::other)?
      .ok_or_else(route_gone)??;
  TcpStream::from_std(stream)
}

/// A Unix stream connection to `path`: from the broker when this process's
/// sockets are routed, else `UnixStream::connect`.
#[cfg(unix)]
pub async fn connect_unix_stream(
  path: &std::path::Path,
) -> io::Result<tokio::net::UnixStream> {
  if !is_routed() {
    return tokio::net::UnixStream::connect(path).await;
  }
  let path = path.to_path_buf();
  let stream =
    tokio::task::spawn_blocking(move || deno_net_route::connect_unix(&path))
      .await
      .map_err(io::Error::other)?
      .ok_or_else(route_gone)??;
  tokio::net::UnixStream::from_std(stream)
}

/// A UDP socket bound to `bind` from the broker; `None` when this
/// process's sockets aren't routed (the caller binds its own).
pub fn open_udp_socket(
  bind: SocketAddr,
  reuse_address: bool,
) -> Option<io::Result<std::net::UdpSocket>> {
  deno_net_route::open_udp(&UdpOpen {
    bind,
    connect: None,
    host: None,
    reuse_address,
    reuse_port: false,
    ipv6_only: None,
  })
}

/// A hickory [`RuntimeProvider`] for a routed process: a DNS query over
/// UDP is one exchange carried out by the broker (the socket never comes
/// here: a UDP socket could send anywhere), and DNS over TCP is a
/// connection from the broker. Both reach only a name server the grant
/// names, as the `Deno.resolveDns()` check requires anyway.
#[derive(Clone, Default)]
pub(crate) struct RoutedRuntimeProvider(TokioRuntimeProvider);

impl RuntimeProvider for RoutedRuntimeProvider {
  type Handle = <TokioRuntimeProvider as RuntimeProvider>::Handle;
  type Timer = <TokioRuntimeProvider as RuntimeProvider>::Timer;
  type Udp = ExchangeSocket;
  type Tcp = <TokioRuntimeProvider as RuntimeProvider>::Tcp;

  fn create_handle(&self) -> Self::Handle {
    self.0.create_handle()
  }

  fn connect_tcp(
    &self,
    server_addr: SocketAddr,
    _bind_addr: Option<SocketAddr>,
    timeout: Option<Duration>,
  ) -> Pin<Box<dyn Send + Future<Output = io::Result<Self::Tcp>>>> {
    Box::pin(async move {
      let connect = connect_tcp_stream(server_addr, None);
      let stream = match timeout {
        Some(timeout) => tokio::time::timeout(timeout, connect)
          .await
          .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??,
        None => connect.await?,
      };
      Ok(AsyncIoTokioAsStd(stream))
    })
  }

  fn bind_udp(
    &self,
    _local_addr: SocketAddr,
    _server_addr: SocketAddr,
  ) -> Pin<Box<dyn Send + Future<Output = io::Result<Self::Udp>>>> {
    Box::pin(std::future::ready(Ok(ExchangeSocket::default())))
  }
}

#[derive(Default)]
struct Exchange {
  /// The exchange under way: the server asked, and its answer.
  pending: Option<(SocketAddr, JoinHandle<io::Result<Vec<u8>>>)>,
  /// A receiver that polled before anything was sent.
  waiting: Option<Waker>,
}

/// The "socket" of one DNS query over UDP in a routed process: sending
/// starts an exchange with the broker, receiving is its answer.
#[derive(Default)]
pub(crate) struct ExchangeSocket(Mutex<Exchange>);

impl DnsUdpSocket for ExchangeSocket {
  type Time = TokioTime;

  fn poll_recv_from(
    &self,
    cx: &mut Context<'_>,
    buf: &mut [u8],
  ) -> Poll<io::Result<(usize, SocketAddr)>> {
    let mut exchange = self.0.lock().unwrap_or_else(|e| e.into_inner());
    let Some((server, answer)) = exchange.pending.as_mut() else {
      exchange.waiting = Some(cx.waker().clone());
      return Poll::Pending;
    };
    let server = *server;
    let answer = std::task::ready!(Pin::new(answer).poll(cx));
    exchange.pending = None;
    let answer = answer.map_err(io::Error::other)??;
    let len = answer.len().min(buf.len());
    buf[..len].copy_from_slice(&answer[..len]);
    Poll::Ready(Ok((len, server)))
  }

  fn poll_send_to(
    &self,
    _cx: &mut Context<'_>,
    buf: &[u8],
    target: SocketAddr,
  ) -> Poll<io::Result<usize>> {
    let payload = buf.to_vec();
    let answer = tokio::task::spawn_blocking(move || {
      deno_net_route::route()
        .ok_or_else(route_gone)?
        .udp_exchange(target, &payload, EXCHANGE_TIMEOUT)
    });
    let mut exchange = self.0.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, previous)) = exchange.pending.replace((target, answer)) {
      // a retry on the same socket: the earlier answer is no longer wanted
      previous.abort();
    }
    if let Some(waker) = exchange.waiting.take() {
      waker.wake();
    }
    Poll::Ready(Ok(buf.len()))
  }
}
