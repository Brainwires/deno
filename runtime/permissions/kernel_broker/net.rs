// Copyright 2018-2026 the Deno authors. MIT license.

//! The broker's network operations (denext): the reserved kinds
//! [`super::protocol::NET_KINDS`] of the kernel-sandbox broker protocol.
//!
//! A process confined to a host-scoped network grant makes no IP socket of
//! its own; every socket it uses is asked of the broker. Three parts:
//!
//! - the payloads ([`NetRequest`], [`encode`], [`decode`]): pure, strict
//!   and fuzzed like the rest of the protocol, because the broker parses
//!   bytes a process it doesn't trust wrote;
//! - the broker's side ([`serve`]): one request carried out through a
//!   `deno_net_route::NetRoute`, which in the supervisor is the network
//!   authority (`deno_sandbox::net_authority`, over Deno's own permission
//!   checks). What a request says is its wish, never evidence;
//! - the confined side ([`BrokerRoute`]): the `NetRoute` installed in the
//!   worker, which every socket-opening crate reaches through
//!   `deno_net_route`.
//!
//! | kind | request | fields | on success |
//! | --- | --- | --- | --- |
//! | 0x0100 | `Resolve` | `host: bytes` | `Data`: `count: u32`, then each address (`bytes`, 4 or 16) |
//! | 0x0101 | `ConnectTcp` | `addr`, `host: bytes` (empty: none), `local: addr?` | `Ok` + a connected socket |
//! | 0x0102 | `BindTcp` | `addr`, `host: bytes`, `flags: u32`, `backlog: u32` | `Ok` + a listening socket |
//! | 0x0103 | `OpenUdp` | `bind: addr`, `connect: addr?`, `host: bytes`, `flags: u32` | `Ok` + a socket |
//! | 0x0104 | `UdpExchange` | `server: addr`, `timeout_ms: u32`, `payload: bytes` | `Data`: the answer |
//! | 0x0105 | `ConnectUnix` | `path` | `Ok` + a connected socket |
//! | 0x0106 | `BindUnix` | `path`, `flags: u32` (1: datagram) | `Ok` + a socket |
//!
//! An `addr` is `ip: bytes` (4 or 16), `port: u32` (at most 65535) and, for
//! IPv6, `scope: u32`; an `addr?` is a `u32` (0 or 1) and then the address.
//! `flags` of `BindTcp`: 1 reuse port, 2 IPv6-only set, 4 its value;
//! of `OpenUdp`: 1 reuse address, 2 reuse port, 4 IPv6-only set, 8 its
//! value. A refusal by the grant is `Denied`; the OS's is `Errno`.

use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::Ipv6Addr;
use std::net::SocketAddr;
use std::net::SocketAddrV6;
use std::path::PathBuf;
use std::time::Duration;

use deno_net_route::TcpBind;
use deno_net_route::TcpConnect;
use deno_net_route::UdpOpen;

use super::protocol::MAX_PATH;
use super::protocol::MAX_PAYLOAD;

pub const RESOLVE: u16 = 0x0100;
pub const CONNECT_TCP: u16 = 0x0101;
pub const BIND_TCP: u16 = 0x0102;
pub const OPEN_UDP: u16 = 0x0103;
pub const UDP_EXCHANGE: u16 = 0x0104;
pub const CONNECT_UNIX: u16 = 0x0105;
pub const BIND_UNIX: u16 = 0x0106;

/// The longest host name a request carries (a DNS name is at most 253
/// bytes; an IPv6 literal with a zone less).
pub const MAX_HOST: usize = 255;
/// The largest datagram of a `UdpExchange` (a DNS message over UDP).
pub const MAX_EXCHANGE: usize = MAX_PAYLOAD - 64;
/// The most addresses a `Resolve` answer carries.
pub const MAX_ANSWERS: usize = 64;

/// One network operation, as a request names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetRequest {
  Resolve {
    host: String,
  },
  ConnectTcp(TcpConnect),
  BindTcp(TcpBind),
  OpenUdp(UdpOpen),
  UdpExchange {
    server: SocketAddr,
    timeout: Duration,
    payload: Vec<u8>,
  },
  ConnectUnix {
    path: PathBuf,
  },
  BindUnix {
    path: PathBuf,
    datagram: bool,
  },
}

impl NetRequest {
  pub fn kind(&self) -> u16 {
    match self {
      NetRequest::Resolve { .. } => RESOLVE,
      NetRequest::ConnectTcp(_) => CONNECT_TCP,
      NetRequest::BindTcp(_) => BIND_TCP,
      NetRequest::OpenUdp(_) => OPEN_UDP,
      NetRequest::UdpExchange { .. } => UDP_EXCHANGE,
      NetRequest::ConnectUnix { .. } => CONNECT_UNIX,
      NetRequest::BindUnix { .. } => BIND_UNIX,
    }
  }

  /// Whether a success carries a descriptor.
  pub fn returns_fd(&self) -> bool {
    !matches!(
      self,
      NetRequest::Resolve { .. } | NetRequest::UdpExchange { .. }
    )
  }
}

/// Why a payload is not a network request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetProtocolError {
  /// Not a kind this module has (still a well-formed frame: `Unsupported`).
  UnknownKind(u16),
  Truncated,
  Trailing,
  Field(&'static str),
}

impl std::fmt::Display for NetProtocolError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      NetProtocolError::UnknownKind(kind) => {
        write!(f, "an unknown network kind {kind:#06x}")
      }
      NetProtocolError::Truncated => write!(f, "the payload ends early"),
      NetProtocolError::Trailing => write!(f, "bytes after the last field"),
      NetProtocolError::Field(name) => write!(f, "a bad value for {name}"),
    }
  }
}

impl std::error::Error for NetProtocolError {}

struct Writer(Vec<u8>);

impl Writer {
  fn u32(&mut self, v: u32) {
    self.0.extend_from_slice(&v.to_le_bytes());
  }

  fn bytes(&mut self, v: &[u8]) {
    self.u32(v.len() as u32);
    self.0.extend_from_slice(v);
  }

  fn addr(&mut self, addr: &SocketAddr) {
    match addr {
      SocketAddr::V4(v4) => {
        self.bytes(&v4.ip().octets());
        self.u32(v4.port().into());
      }
      SocketAddr::V6(v6) => {
        self.bytes(&v6.ip().octets());
        self.u32(v6.port().into());
        self.u32(v6.scope_id());
      }
    }
  }

  fn opt_addr(&mut self, addr: &Option<SocketAddr>) {
    match addr {
      Some(addr) => {
        self.u32(1);
        self.addr(addr);
      }
      None => self.u32(0),
    }
  }

  fn host(&mut self, host: &Option<String>) {
    self.bytes(host.as_deref().unwrap_or("").as_bytes());
  }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
  fn take(&mut self, n: usize) -> Result<&'a [u8], NetProtocolError> {
    if self.0.len() < n {
      return Err(NetProtocolError::Truncated);
    }
    let (head, rest) = self.0.split_at(n);
    self.0 = rest;
    Ok(head)
  }

  fn u32(&mut self) -> Result<u32, NetProtocolError> {
    Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
  }

  fn bytes(&mut self) -> Result<&'a [u8], NetProtocolError> {
    let len = self.u32()? as usize;
    self.take(len)
  }

  fn ip(&mut self) -> Result<IpAddr, NetProtocolError> {
    let bytes = self.bytes()?;
    if let Ok(v4) = <[u8; 4]>::try_from(bytes) {
      Ok(Ipv4Addr::from(v4).into())
    } else if let Ok(v6) = <[u8; 16]>::try_from(bytes) {
      Ok(Ipv6Addr::from(v6).into())
    } else {
      Err(NetProtocolError::Field("address"))
    }
  }

  fn addr(&mut self) -> Result<SocketAddr, NetProtocolError> {
    let ip = self.ip()?;
    let port = u16::try_from(self.u32()?)
      .map_err(|_| NetProtocolError::Field("port"))?;
    Ok(match ip {
      IpAddr::V4(v4) => SocketAddr::new(v4.into(), port),
      IpAddr::V6(v6) => {
        SocketAddr::V6(SocketAddrV6::new(v6, port, 0, self.u32()?))
      }
    })
  }

  fn opt_addr(&mut self) -> Result<Option<SocketAddr>, NetProtocolError> {
    match self.u32()? {
      0 => Ok(None),
      1 => Ok(Some(self.addr()?)),
      _ => Err(NetProtocolError::Field("optional address")),
    }
  }

  /// A host name: UTF-8, at most [`MAX_HOST`] bytes, no NUL or control
  /// byte; empty is none.
  fn host(&mut self) -> Result<Option<String>, NetProtocolError> {
    let bytes = self.bytes()?;
    if bytes.is_empty() {
      return Ok(None);
    }
    if bytes.len() > MAX_HOST || bytes.iter().any(|b| *b < 0x20 || *b == 0x7f) {
      return Err(NetProtocolError::Field("host"));
    }
    String::from_utf8(bytes.to_vec())
      .map(Some)
      .map_err(|_| NetProtocolError::Field("host"))
  }

  fn flags(
    &mut self,
    known: u32,
    name: &'static str,
  ) -> Result<u32, NetProtocolError> {
    let flags = self.u32()?;
    if flags & !known != 0 {
      return Err(NetProtocolError::Field(name));
    }
    Ok(flags)
  }

  /// A Unix socket path: absolute, 1 to [`MAX_PATH`] bytes, no NUL. (An
  /// abstract socket's name starts with NUL: not a path, not carried.)
  #[cfg(unix)]
  fn path(&mut self) -> Result<PathBuf, NetProtocolError> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = self.bytes()?;
    if bytes.is_empty()
      || bytes.len() > MAX_PATH
      || bytes[0] != b'/'
      || bytes.contains(&0)
    {
      return Err(NetProtocolError::Field("path"));
    }
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
  }

  /// Windows: the same rules, with an absolute path the OS's own (a drive or
  /// UNC root), so a relative path is refused here too.
  #[cfg(not(unix))]
  fn path(&mut self) -> Result<PathBuf, NetProtocolError> {
    let bytes = self.bytes()?;
    if bytes.is_empty() || bytes.len() > MAX_PATH || bytes.contains(&0) {
      return Err(NetProtocolError::Field("path"));
    }
    let path = String::from_utf8(bytes.to_vec())
      .map(PathBuf::from)
      .map_err(|_| NetProtocolError::Field("path"))?;
    if !path.is_absolute() {
      return Err(NetProtocolError::Field("path"));
    }
    Ok(path)
  }

  fn end(self) -> Result<(), NetProtocolError> {
    if self.0.is_empty() {
      Ok(())
    } else {
      Err(NetProtocolError::Trailing)
    }
  }
}

fn v6only_flags(ipv6_only: Option<bool>, set: u32, value: u32) -> u32 {
  match ipv6_only {
    None => 0,
    Some(false) => set,
    Some(true) => set | value,
  }
}

fn v6only(flags: u32, set: u32, value: u32) -> Option<bool> {
  (flags & set != 0).then_some(flags & value != 0)
}

#[cfg(unix)]
fn path_bytes(path: &std::path::Path) -> Vec<u8> {
  use std::os::unix::ffi::OsStrExt;
  path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_bytes(path: &std::path::Path) -> Vec<u8> {
  path.to_string_lossy().into_owned().into_bytes()
}

/// A request's kind and payload (the caller frames them).
pub fn encode(request: &NetRequest) -> (u16, Vec<u8>) {
  let mut w = Writer(Vec::new());
  match request {
    NetRequest::Resolve { host } => w.bytes(host.as_bytes()),
    NetRequest::ConnectTcp(r) => {
      w.addr(&r.addr);
      w.host(&r.host);
      w.opt_addr(&r.local);
    }
    NetRequest::BindTcp(r) => {
      w.addr(&r.addr);
      w.host(&r.host);
      w.u32(u32::from(r.reuse_port) | v6only_flags(r.ipv6_only, 2, 4));
      w.u32(r.backlog.max(0) as u32);
    }
    NetRequest::OpenUdp(r) => {
      w.addr(&r.bind);
      w.opt_addr(&r.connect);
      w.host(&r.host);
      w.u32(
        u32::from(r.reuse_address)
          | (u32::from(r.reuse_port) << 1)
          | v6only_flags(r.ipv6_only, 4, 8),
      );
    }
    NetRequest::UdpExchange {
      server,
      timeout,
      payload,
    } => {
      w.addr(server);
      w.u32(u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX));
      w.bytes(payload);
    }
    NetRequest::ConnectUnix { path } => w.bytes(&path_bytes(path)),
    NetRequest::BindUnix { path, datagram } => {
      w.bytes(&path_bytes(path));
      w.u32(u32::from(*datagram));
    }
  }
  (request.kind(), w.0)
}

/// Parses the payload of a frame of `kind`. Strict: every field in range,
/// nothing after the last one.
pub fn decode(
  kind: u16,
  payload: &[u8],
) -> Result<NetRequest, NetProtocolError> {
  let mut r = Reader(payload);
  let request = match kind {
    RESOLVE => {
      let host = r.host()?.ok_or(NetProtocolError::Field("host"))?;
      NetRequest::Resolve { host }
    }
    CONNECT_TCP => NetRequest::ConnectTcp(TcpConnect {
      addr: r.addr()?,
      host: r.host()?,
      local: r.opt_addr()?,
    }),
    BIND_TCP => {
      let addr = r.addr()?;
      let host = r.host()?;
      let flags = r.flags(7, "bind flags")?;
      let backlog = i32::try_from(r.u32()?)
        .map_err(|_| NetProtocolError::Field("backlog"))?;
      NetRequest::BindTcp(TcpBind {
        addr,
        host,
        reuse_port: flags & 1 != 0,
        backlog,
        ipv6_only: v6only(flags, 2, 4),
      })
    }
    OPEN_UDP => {
      let bind = r.addr()?;
      let connect = r.opt_addr()?;
      let host = r.host()?;
      let flags = r.flags(15, "socket flags")?;
      NetRequest::OpenUdp(UdpOpen {
        bind,
        connect,
        host,
        reuse_address: flags & 1 != 0,
        reuse_port: flags & 2 != 0,
        ipv6_only: v6only(flags, 4, 8),
      })
    }
    UDP_EXCHANGE => {
      let server = r.addr()?;
      let timeout = Duration::from_millis(r.u32()?.into());
      let payload = r.bytes()?;
      if payload.len() > MAX_EXCHANGE {
        return Err(NetProtocolError::Field("datagram"));
      }
      NetRequest::UdpExchange {
        server,
        timeout,
        payload: payload.to_vec(),
      }
    }
    CONNECT_UNIX => NetRequest::ConnectUnix { path: r.path()? },
    BIND_UNIX => {
      let path = r.path()?;
      let flags = r.flags(1, "socket flags")?;
      NetRequest::BindUnix {
        path,
        datagram: flags & 1 != 0,
      }
    }
    other => return Err(NetProtocolError::UnknownKind(other)),
  };
  r.end()?;
  Ok(request)
}

/// A `Resolve` answer's payload.
pub fn encode_answers(addrs: &[IpAddr]) -> Vec<u8> {
  let addrs = &addrs[..addrs.len().min(MAX_ANSWERS)];
  let mut w = Writer(Vec::new());
  w.u32(addrs.len() as u32);
  for addr in addrs {
    match addr {
      IpAddr::V4(v4) => w.bytes(&v4.octets()),
      IpAddr::V6(v6) => w.bytes(&v6.octets()),
    }
  }
  w.0
}

/// Parses a `Resolve` answer.
pub fn decode_answers(bytes: &[u8]) -> Result<Vec<IpAddr>, NetProtocolError> {
  let mut r = Reader(bytes);
  let count = r.u32()? as usize;
  if count > MAX_ANSWERS {
    return Err(NetProtocolError::Field("answer count"));
  }
  let mut addrs = Vec::with_capacity(count);
  for _ in 0..count {
    addrs.push(r.ip()?);
  }
  r.end()?;
  Ok(addrs)
}

#[cfg(unix)]
pub use unix::BrokerRoute;
#[cfg(unix)]
pub use unix::FEATURES;
#[cfg(unix)]
pub use unix::serve;

#[cfg(unix)]
mod unix {
  use std::io;
  use std::net::IpAddr;
  use std::net::SocketAddr;
  use std::os::fd::OwnedFd;
  use std::path::Path;
  use std::sync::Arc;
  use std::time::Duration;

  use deno_net_route::NetRoute;
  use deno_net_route::TcpBind;
  use deno_net_route::TcpConnect;
  use deno_net_route::UdpOpen;

  use super::super::client;
  use super::super::client::BrokerClient;
  use super::super::protocol::Response;
  use super::super::protocol::features;
  use super::super::server::Reply;
  use super::NetProtocolError;
  use super::NetRequest;
  use super::decode;
  use super::decode_answers;
  use super::encode;
  use super::encode_answers;

  /// The protocol features the network operations are.
  pub const FEATURES: u64 = features::NET | features::DNS | features::UNIX;

  /// A route's answer as a reply: a refusal by the grant is `Denied` (the
  /// confined side is told no more than that), anything else the OS's
  /// error.
  fn failed(err: &io::Error) -> Reply {
    if err.kind() == io::ErrorKind::PermissionDenied
      && err.raw_os_error().is_none()
    {
      Reply::denied()
    } else {
      Reply::io_error(err)
    }
  }

  fn fd(result: io::Result<OwnedFd>) -> Reply {
    match result {
      Ok(fd) => Reply::ok_fd(fd),
      Err(err) => failed(&err),
    }
  }

  /// The broker's side: carries out the request in a frame of `kind`
  /// through `authority` (the supervisor's network authority; it decides,
  /// the request only asks). A malformed payload is `Invalid`: the
  /// requester drops the channel.
  pub fn serve(authority: &dyn NetRoute, kind: u16, payload: &[u8]) -> Reply {
    let request = match decode(kind, payload) {
      Ok(request) => request,
      Err(NetProtocolError::UnknownKind(_)) => return Reply::unsupported(),
      Err(_) => {
        return Reply {
          response: Response::Invalid,
          fd: None,
        };
      }
    };
    match request {
      NetRequest::Resolve { host } => match authority.resolve(&host) {
        Ok(addrs) => Reply::data(encode_answers(&addrs)),
        Err(err) => failed(&err),
      },
      NetRequest::ConnectTcp(request) => fd(authority.connect_tcp(&request)),
      NetRequest::BindTcp(request) => fd(authority.bind_tcp(&request)),
      NetRequest::OpenUdp(request) => fd(authority.open_udp(&request)),
      NetRequest::UdpExchange {
        server,
        timeout,
        payload,
      } => match authority.udp_exchange(server, &payload, timeout) {
        Ok(answer) => Reply::data(answer),
        Err(err) => failed(&err),
      },
      NetRequest::ConnectUnix { path } => fd(authority.connect_unix(&path)),
      NetRequest::BindUnix { path, datagram } => {
        fd(authority.bind_unix(&path, datagram))
      }
    }
  }

  /// The confined side: this process's sockets, asked of its broker.
  /// Installed with `deno_net_route::install` by the worker of a plan
  /// whose network the broker holds.
  pub struct BrokerRoute(Option<Arc<BrokerClient>>);

  impl BrokerRoute {
    /// Over this process's broker ([`client::get`]).
    pub fn new() -> Self {
      Self(None)
    }

    /// Over a client of the caller's (tests).
    pub fn over(client: Arc<BrokerClient>) -> Self {
      Self(Some(client))
    }
  }

  impl Default for BrokerRoute {
    fn default() -> Self {
      Self::new()
    }
  }

  fn refused(what: std::fmt::Arguments<'_>) -> io::Error {
    deno_net_route::refused(format_args!(
      "{what} is not something the network grant allows"
    ))
  }

  fn unexpected(response: Response) -> io::Error {
    match response {
      Response::Errno(errno) => io::Error::from_raw_os_error(errno),
      Response::Unsupported => io::Error::new(
        io::ErrorKind::Unsupported,
        "the kernel sandbox broker doesn't have this network operation",
      ),
      other => io::Error::new(
        io::ErrorKind::InvalidData,
        format!("the kernel sandbox broker answered {other:?}"),
      ),
    }
  }

  impl BrokerRoute {
    fn ask(
      &self,
      request: &NetRequest,
    ) -> io::Result<(Response, Option<OwnedFd>)> {
      let broker = match &self.0 {
        Some(client) => &**client,
        None => client::get().ok_or_else(|| {
          io::Error::new(
            io::ErrorKind::BrokenPipe,
            "this process has no kernel sandbox broker",
          )
        })?,
      };
      let (kind, payload) = encode(request);
      broker.request_raw(kind, &payload, request.returns_fd())
    }

    fn socket(
      &self,
      request: &NetRequest,
      what: std::fmt::Arguments<'_>,
    ) -> io::Result<OwnedFd> {
      match self.ask(request)? {
        (Response::Ok { fd: true }, Some(fd)) => Ok(fd),
        (Response::Denied, _) => Err(refused(what)),
        (other, _) => Err(unexpected(other)),
      }
    }

    fn data(
      &self,
      request: &NetRequest,
      what: std::fmt::Arguments<'_>,
    ) -> io::Result<Vec<u8>> {
      match self.ask(request)? {
        (Response::Data(bytes), _) => Ok(bytes),
        (Response::Denied, _) => Err(refused(what)),
        (other, _) => Err(unexpected(other)),
      }
    }
  }

  impl NetRoute for BrokerRoute {
    fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>> {
      let answer = self.data(
        &NetRequest::Resolve {
          host: host.to_string(),
        },
        format_args!("resolving {host}"),
      )?;
      decode_answers(&answer)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
    }

    fn connect_tcp(&self, request: &TcpConnect) -> io::Result<OwnedFd> {
      self.socket(
        &NetRequest::ConnectTcp(request.clone()),
        format_args!("connecting to {}", request.addr),
      )
    }

    fn bind_tcp(&self, request: &TcpBind) -> io::Result<OwnedFd> {
      self.socket(
        &NetRequest::BindTcp(request.clone()),
        format_args!("listening on {}", request.addr),
      )
    }

    fn open_udp(&self, request: &UdpOpen) -> io::Result<OwnedFd> {
      self.socket(
        &NetRequest::OpenUdp(request.clone()),
        format_args!(
          "a UDP socket on {} (one can send to any address, so a host-scoped grant has none)",
          request.bind
        ),
      )
    }

    fn udp_exchange(
      &self,
      server: SocketAddr,
      payload: &[u8],
      timeout: Duration,
    ) -> io::Result<Vec<u8>> {
      self.data(
        &NetRequest::UdpExchange {
          server,
          timeout,
          payload: payload.to_vec(),
        },
        format_args!("a datagram to {server}"),
      )
    }

    fn connect_unix(&self, path: &Path) -> io::Result<OwnedFd> {
      self.socket(
        &NetRequest::ConnectUnix {
          path: path.to_path_buf(),
        },
        format_args!("connecting to {}", path.display()),
      )
    }

    fn bind_unix(&self, path: &Path, datagram: bool) -> io::Result<OwnedFd> {
      self.socket(
        &NetRequest::BindUnix {
          path: path.to_path_buf(),
          datagram,
        },
        format_args!("binding {}", path.display()),
      )
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// An absolute socket path on this OS (a drive root on Windows).
  fn sock_path() -> PathBuf {
    PathBuf::from(if cfg!(windows) {
      r"C:\run\app.sock"
    } else {
      "/run/app.sock"
    })
  }

  fn samples() -> Vec<NetRequest> {
    let v4: SocketAddr = "93.184.216.34:443".parse().unwrap();
    let v6: SocketAddr = "[2001:db8::1%7]:8443".parse().unwrap();
    vec![
      NetRequest::Resolve {
        host: "example.com".to_string(),
      },
      NetRequest::ConnectTcp(TcpConnect {
        addr: v4,
        host: Some("example.com".to_string()),
        local: None,
      }),
      NetRequest::ConnectTcp(TcpConnect {
        addr: v6,
        host: None,
        local: Some("[::]:0".parse().unwrap()),
      }),
      NetRequest::BindTcp(TcpBind {
        addr: "127.0.0.1:8000".parse().unwrap(),
        host: Some("localhost".to_string()),
        reuse_port: true,
        backlog: 511,
        ipv6_only: None,
      }),
      NetRequest::BindTcp(TcpBind {
        addr: "[::]:8000".parse().unwrap(),
        host: None,
        reuse_port: false,
        backlog: 0,
        ipv6_only: Some(true),
      }),
      NetRequest::OpenUdp(UdpOpen {
        bind: "0.0.0.0:0".parse().unwrap(),
        connect: Some(v4),
        host: Some("example.com".to_string()),
        reuse_address: true,
        reuse_port: false,
        ipv6_only: Some(false),
      }),
      NetRequest::UdpExchange {
        server: "127.0.0.53:53".parse().unwrap(),
        timeout: Duration::from_millis(5000),
        payload: vec![1, 2, 3, 4],
      },
      NetRequest::ConnectUnix { path: sock_path() },
      NetRequest::BindUnix {
        path: sock_path(),
        datagram: true,
      },
    ]
  }

  #[test]
  fn requests_round_trip() {
    for request in samples() {
      let (kind, payload) = encode(&request);
      assert!(super::super::protocol::NET_KINDS.contains(&kind));
      assert!(payload.len() <= MAX_PAYLOAD);
      assert_eq!(decode(kind, &payload), Ok(request));
    }
  }

  #[test]
  fn answers_round_trip() {
    let addrs: Vec<IpAddr> = vec![
      "93.184.216.34".parse().unwrap(),
      "2001:db8::1".parse().unwrap(),
    ];
    assert_eq!(decode_answers(&encode_answers(&addrs)), Ok(addrs));
    assert_eq!(decode_answers(&encode_answers(&[])), Ok(vec![]));
    // more than the cap is cut by the encoder and refused by the decoder
    let many: Vec<IpAddr> = (0..MAX_ANSWERS as u8 + 3)
      .map(|i| IpAddr::from([10, 0, 0, i]))
      .collect();
    assert_eq!(
      decode_answers(&encode_answers(&many)).unwrap().len(),
      MAX_ANSWERS
    );
    let mut forged = (MAX_ANSWERS as u32 + 1).to_le_bytes().to_vec();
    forged.extend_from_slice(&[0; 8]);
    assert!(decode_answers(&forged).is_err());
  }

  #[test]
  fn malformed_payloads_are_refused() {
    let bytes = |parts: &[&[u8]]| parts.concat();
    let u32le = |v: u32| v.to_le_bytes();
    // an unknown kind inside the reserved range
    assert_eq!(
      decode(0x01ff, &[]),
      Err(NetProtocolError::UnknownKind(0x01ff))
    );
    // an empty host, a host with a control byte, a host too long
    assert!(decode(RESOLVE, &bytes(&[&u32le(0)])).is_err());
    assert!(decode(RESOLVE, &bytes(&[&u32le(3), b"a\nb"])).is_err());
    let long = vec![b'a'; MAX_HOST + 1];
    assert!(
      decode(RESOLVE, &bytes(&[&u32le(long.len() as u32), &long])).is_err()
    );
    // not UTF-8
    assert!(decode(RESOLVE, &bytes(&[&u32le(2), &[0xff, 0xfe]])).is_err());
    // an address of five bytes, a port past 65535
    assert!(
      decode(
        CONNECT_TCP,
        &bytes(&[
          &u32le(5),
          &[1, 2, 3, 4, 5],
          &u32le(80),
          &u32le(0),
          &u32le(0)
        ])
      )
      .is_err()
    );
    assert!(
      decode(
        CONNECT_TCP,
        &bytes(&[
          &u32le(4),
          &[1, 2, 3, 4],
          &u32le(65536),
          &u32le(0),
          &u32le(0)
        ])
      )
      .is_err()
    );
    // unknown flag bits
    let (kind, mut payload) = encode(&samples()[3]);
    let at = payload.len() - 8;
    payload[at..at + 4].copy_from_slice(&u32le(1 << 5));
    assert_eq!(
      decode(kind, &payload),
      Err(NetProtocolError::Field("bind flags"))
    );
    // a relative Unix socket path, one with a NUL (an abstract name)
    assert!(decode(CONNECT_UNIX, &bytes(&[&u32le(3), b"a/b"])).is_err());
    assert!(decode(CONNECT_UNIX, &bytes(&[&u32le(3), b"/\0b"])).is_err());
    assert!(decode(CONNECT_UNIX, &bytes(&[&u32le(2), b"\0b"])).is_err());
    // every truncation and every extension of a good payload
    for request in samples() {
      let (kind, payload) = encode(&request);
      for cut in 0..payload.len() {
        assert!(
          decode(kind, &payload[..cut]).is_err(),
          "{request:?} cut at {cut}"
        );
      }
      let mut longer = payload.clone();
      longer.push(0);
      assert_eq!(decode(kind, &longer), Err(NetProtocolError::Trailing));
    }
    // a datagram past the cap
    let big = vec![0u8; MAX_EXCHANGE + 1];
    let mut w = Writer(Vec::new());
    w.addr(&"127.0.0.1:53".parse().unwrap());
    w.u32(1000);
    w.bytes(&big);
    assert_eq!(
      decode(UDP_EXCHANGE, &w.0),
      Err(NetProtocolError::Field("datagram"))
    );
  }

  // The operations end to end: a route over a client, the server, and an
  // authority behind `serve`.
  #[cfg(unix)]
  #[test]
  fn the_route_reaches_the_authority_through_the_broker() {
    use std::io;
    use std::io::Read;
    use std::io::Write;
    use std::net::TcpListener;
    use std::os::fd::OwnedFd;
    use std::path::Path;
    use std::sync::Arc;

    use deno_net_route::NetRoute;

    use super::super::client::BrokerClient;
    use super::super::protocol::Request;
    use super::super::server::Handler;
    use super::super::server::Reply;
    use super::super::server::Server;
    use super::super::transport;

    /// An authority that allows one name and one port.
    struct OnePort(u16);
    impl NetRoute for OnePort {
      fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>> {
        if host == "granted.test" {
          Ok(vec![Ipv4Addr::LOCALHOST.into()])
        } else {
          Err(deno_net_route::refused("not in the grant"))
        }
      }
      fn connect_tcp(&self, request: &TcpConnect) -> io::Result<OwnedFd> {
        if request.addr.port() != self.0 {
          return Err(deno_net_route::refused("not in the grant"));
        }
        Ok(std::net::TcpStream::connect(request.addr)?.into())
      }
      fn bind_tcp(&self, request: &TcpBind) -> io::Result<OwnedFd> {
        Ok(TcpListener::bind(request.addr)?.into())
      }
      fn open_udp(&self, _request: &UdpOpen) -> io::Result<OwnedFd> {
        Err(deno_net_route::refused("no UDP socket"))
      }
      fn udp_exchange(
        &self,
        _server: SocketAddr,
        payload: &[u8],
        _timeout: Duration,
      ) -> io::Result<Vec<u8>> {
        Ok(payload.iter().rev().copied().collect())
      }
      fn connect_unix(&self, _path: &Path) -> io::Result<OwnedFd> {
        // the OS's own refusal, not the grant's
        Err(io::Error::from_raw_os_error(libc::ECONNREFUSED))
      }
      fn bind_unix(
        &self,
        _path: &Path,
        _datagram: bool,
      ) -> io::Result<OwnedFd> {
        Err(deno_net_route::refused("not in the grant"))
      }
    }

    struct NetOnly(OnePort);
    impl Handler for NetOnly {
      fn features(&self) -> u64 {
        FEATURES
      }
      fn handle(&self, _request: &Request) -> Reply {
        Reply::unsupported()
      }
      fn handle_unknown(&self, kind: u16, payload: &[u8]) -> Reply {
        serve(&self.0, kind, payload)
      }
    }

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (ours, theirs) = transport::channel_pair().unwrap();
    let server = Server::new(NetOnly(OnePort(port)));
    server.spawn(ours).unwrap();
    let client = Arc::new(BrokerClient::new(theirs));
    assert_eq!(client.hello().unwrap() & FEATURES, FEATURES);
    let route = BrokerRoute::over(client.clone());

    // a name, and a refusal that says what was asked
    assert_eq!(
      route.resolve("granted.test").unwrap(),
      vec![IpAddr::from(Ipv4Addr::LOCALHOST)]
    );
    let err = route.resolve("other.test").unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    assert!(err.to_string().contains("resolving other.test"), "{err}");

    // a socket comes over the channel, connected
    let connect = |port: u16| {
      route.connect_tcp(&TcpConnect {
        addr: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        host: Some("granted.test".to_string()),
        local: None,
      })
    };
    let mut stream = std::net::TcpStream::from(connect(port).unwrap());
    let (mut accepted, _) = listener.accept().unwrap();
    stream.write_all(b"over").unwrap();
    let mut buf = [0u8; 4];
    accepted.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"over");
    let err = connect(port.wrapping_add(1)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);

    // a listener, bound by the broker
    let bound = TcpListener::from(
      route
        .bind_tcp(&TcpBind {
          addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
          host: None,
          reuse_port: false,
          backlog: 8,
          ipv6_only: None,
        })
        .unwrap(),
    );
    std::net::TcpStream::connect(bound.local_addr().unwrap()).unwrap();

    // data answers, the grant's refusal and the OS's error
    assert_eq!(
      route
        .udp_exchange(
          "127.0.0.1:53".parse().unwrap(),
          b"abc",
          Duration::from_secs(1)
        )
        .unwrap(),
      b"cba"
    );
    let err = route
      .open_udp(&UdpOpen {
        bind: "0.0.0.0:0".parse().unwrap(),
        connect: None,
        host: None,
        reuse_address: false,
        reuse_port: false,
        ipv6_only: None,
      })
      .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    let err = route.connect_unix(Path::new("/nowhere.sock")).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::ECONNREFUSED));

    // a malformed network frame closes that channel and nothing else
    let (response, _) = client.request_raw(RESOLVE, &[1, 2, 3], false).unwrap();
    assert_eq!(response, super::super::protocol::Response::Invalid);
    assert_eq!(
      route.resolve("granted.test").unwrap(),
      vec![IpAddr::from(Ipv4Addr::LOCALHOST)]
    );
    // a reserved kind nothing defines is unsupported, the channel stays
    let (response, _) = client.request_raw(0x01ff, &[], false).unwrap();
    assert_eq!(response, super::super::protocol::Response::Unsupported);
  }

  /// A small deterministic generator (the tests need no entropy source).
  struct Rng(u64);

  impl Rng {
    fn next(&mut self) -> u64 {
      // xorshift64*
      self.0 ^= self.0 >> 12;
      self.0 ^= self.0 << 25;
      self.0 ^= self.0 >> 27;
      self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
  }

  // Whatever the bytes, decoding never panics, and what decodes encodes
  // back to a payload that decodes to the same request.
  #[test]
  fn fuzzed_payloads_never_panic() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let seeds: Vec<(u16, Vec<u8>)> = samples().iter().map(encode).collect();
    for i in 0..200_000u32 {
      let (kind, mut payload) = seeds[i as usize % seeds.len()].clone();
      let kind = if rng.next().is_multiple_of(16) {
        0x0100 + (rng.next() % 0x100) as u16
      } else {
        kind
      };
      for _ in 0..(rng.next() % 4) {
        match rng.next() % 4 {
          0 if !payload.is_empty() => {
            let at = rng.next() as usize % payload.len();
            payload[at] = rng.next() as u8;
          }
          1 if !payload.is_empty() => {
            let at = rng.next() as usize % payload.len();
            payload.truncate(at);
          }
          2 => payload.push(rng.next() as u8),
          _ => {
            let at = rng.next() as usize % (payload.len() + 1);
            payload.insert(at, rng.next() as u8);
          }
        }
      }
      if let Ok(request) = decode(kind, &payload) {
        let (kind2, payload2) = encode(&request);
        assert_eq!(kind2, kind);
        assert_eq!(decode(kind2, &payload2), Ok(request));
      }
    }
  }
}
