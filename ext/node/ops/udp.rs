// Copyright 2018-2026 the Deno authors. MIT license.

use std::borrow::Cow;
use std::cell::RefCell;
use std::net::Ipv4Addr;
use std::net::Ipv6Addr;
use std::net::SocketAddr;
use std::rc::Rc;
use std::str::FromStr;

use deno_core::CancelFuture;
use deno_core::CancelHandle;
use deno_core::JsBuffer;
use deno_core::OpState;
use deno_core::RcRef;
use deno_core::Resource;
use deno_core::ResourceId;
use deno_core::op2;
use deno_permissions::PermissionsContainer;
use socket2::Domain;
use socket2::Protocol;
use socket2::Socket;
use socket2::Type;
use tokio::net::UdpSocket;

#[derive(Debug, thiserror::Error, deno_error::JsError)]
pub enum NodeUdpError {
  #[class(inherit)]
  #[error("{0}")]
  Io(#[from] std::io::Error),
  #[class(generic)]
  #[error("{0}")]
  AddrParse(#[from] std::net::AddrParseError),
  #[class(inherit)]
  #[error("{0}")]
  Resource(#[from] deno_core::error::ResourceError),
  #[class(inherit)]
  #[error("{0}")]
  Canceled(#[from] deno_core::Canceled),
  #[class(generic)]
  #[error("No resolved address found")]
  NoResolvedAddress,
  #[class(type)]
  #[error("Invalid hostname: '{0}'")]
  InvalidHostname(String),
  #[class(inherit)]
  #[error(transparent)]
  Permission(#[from] deno_permissions::PermissionCheckError),
}

pub struct NodeUdpSocketResource {
  pub socket: UdpSocket,
  pub cancel: CancelHandle,
}

impl Resource for NodeUdpSocketResource {
  fn name(&self) -> Cow<'_, str> {
    "nodeUdpSocket".into()
  }

  fn close(self: Rc<Self>) {
    self.cancel.cancel()
  }
}

#[op2]
#[serde]
pub fn op_node_udp_bind(
  state: &mut OpState,
  #[string] hostname: &str,
  #[smi] port: u16,
  reuse_address: bool,
  ipv6_only: bool,
  reuse_port: bool,
) -> Result<(ResourceId, String, u16), NodeUdpError> {
  state
    .borrow_mut::<PermissionsContainer>()
    .check_net_listen(&(hostname, Some(port)), "dgram.createSocket()")?;
  // denext: `reusePort` (UV_UDP_REUSEPORT) load-balances datagrams across
  // the sockets bound to a port, which only Linux's SO_REUSEPORT does; libuv
  // refuses it with ENOTSUP everywhere else (macOS's SO_REUSEPORT is
  // BSD's address sharing, which `reuseAddr` already uses there).
  if reuse_port && !cfg!(any(target_os = "linux", target_os = "android")) {
    return Err(NodeUdpError::Io(not_supported_error()));
  }

  let addr = deno_net::resolve_addr::resolve_addr_sync(hostname, port)?
    .next()
    .ok_or(NodeUdpError::NoResolvedAddress)?;
  state
    .borrow_mut::<PermissionsContainer>()
    .check_net_listen_resolved(
      &addr.ip(),
      addr.port(),
      "dgram.createSocket()",
    )?;

  let domain = if addr.is_ipv4() {
    Domain::IPV4
  } else {
    Domain::IPV6
  };
  // denext: under a kernel sandbox with a broker the socket is the
  // broker's to make (bound already), when the grant lets it hand one out.
  let routed = deno_net_route::open_udp(&deno_net_route::UdpOpen {
    bind: addr,
    connect: None,
    host: None,
    reuse_address,
    reuse_port,
    ipv6_only: (addr.is_ipv6() && ipv6_only).then_some(true),
  })
  .transpose()?
  .map(Socket::from);
  let sock = match routed {
    Some(sock) => sock,
    None => {
      let sock = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
      if reuse_address {
        #[cfg(any(
          target_os = "windows",
          target_os = "android",
          target_os = "linux"
        ))]
        sock.set_reuse_address(true)?;
        #[cfg(all(
          unix,
          not(any(target_os = "android", target_os = "linux"))
        ))]
        sock.set_reuse_port(true)?;
      }
      #[cfg(any(target_os = "android", target_os = "linux"))]
      if reuse_port {
        sock.set_reuse_port(true)?;
      }
      if addr.is_ipv6() && ipv6_only {
        sock.set_only_v6(true)?;
      }
      let socket_addr = socket2::SockAddr::from(addr);
      sock.bind(&socket_addr)?;
      sock
    }
  };
  sock.set_nonblocking(true)?;

  let std_socket: std::net::UdpSocket = sock.into();
  let socket = UdpSocket::from_std(std_socket)?;
  let local_addr = socket.local_addr()?;

  let resource = NodeUdpSocketResource {
    socket,
    cancel: Default::default(),
  };
  let rid = state.resource_table.add(resource);

  Ok((rid, local_addr.ip().to_string(), local_addr.port()))
}

#[op2]
pub fn op_node_udp_join_multi_v4(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] address: &str,
  #[string] multi_iface: Option<String>,
) -> Result<(), NodeUdpError> {
  let resource = state.resource_table.get::<NodeUdpSocketResource>(rid)?;

  let addr = Ipv4Addr::from_str(address)?;
  let iface = multi_iface
    .as_deref()
    .map(Ipv4Addr::from_str)
    .transpose()?
    .unwrap_or(Ipv4Addr::UNSPECIFIED);

  let port = resource.socket.local_addr()?.port();
  deno_net::check_multicast_membership_permission(
    state.borrow_mut::<PermissionsContainer>(),
    std::net::IpAddr::V4(addr),
    port,
    "dgram.addMembership()",
  )?;

  resource.socket.join_multicast_v4(addr, iface)?;
  Ok(())
}

#[op2]
pub fn op_node_udp_leave_multi_v4(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] address: &str,
  #[string] multi_iface: Option<String>,
) -> Result<(), NodeUdpError> {
  let resource = state.resource_table.get::<NodeUdpSocketResource>(rid)?;

  let addr = Ipv4Addr::from_str(address)?;
  let iface = multi_iface
    .as_deref()
    .map(Ipv4Addr::from_str)
    .transpose()?
    .unwrap_or(Ipv4Addr::UNSPECIFIED);

  resource.socket.leave_multicast_v4(addr, iface)?;
  Ok(())
}

/// Resolve an IPv6 interface address to an interface index.
///
/// This mirrors libuv's `uv_ip6_addr()`, which Node.js uses to translate the
/// `multicastInterface` argument of `addMembership()`/`dropMembership()` into
/// an interface index (the `sin6_scope_id` of the parsed address):
///
/// - The interface is selected by the zone id following `%`. On Windows the
///   zone is a numeric index (`atoi`), while on Unix it is an interface *name*
///   resolved via `if_nametoindex()`.
/// - An unknown zone id resolves to index `0`, which the kernel treats as
///   "use the default interface". Node.js silently accepts such values rather
///   than failing, so `"::%12"` (no interface literally named `12`) joins on
///   the default interface instead of raising `EINVAL`.
///
/// As an extension over libuv, when no zone id is supplied we additionally try
/// to map a bare interface address (e.g. `"fe80::1"`) to its index by scanning
/// the local interfaces, falling back to the default interface.
fn resolve_ipv6_interface(
  interface_addr: Option<&str>,
) -> Result<u32, NodeUdpError> {
  let Some(addr_str) = interface_addr else {
    return Ok(0);
  };

  // Check if the address contains a zone ID (e.g. "fe80::1%eth0" or "::1%12")
  if let Some(zone_idx) = addr_str.find('%') {
    // The address part must be a valid IPv6 address (libuv calls
    // `uv_inet_pton` and reports `EINVAL` when it fails to parse).
    Ipv6Addr::from_str(&addr_str[..zone_idx])?;

    let zone_id = &addr_str[zone_idx + 1..];
    #[cfg(windows)]
    {
      // `atoi`-style parsing: a non-numeric zone yields the default interface.
      return Ok(zone_id.parse::<u32>().unwrap_or(0));
    }
    #[cfg(unix)]
    {
      use std::ffi::CString;
      // Resolve the zone as an interface name. `if_nametoindex` returns 0 for
      // unknown interfaces (including numeric strings that aren't names),
      // which is silently treated as the default interface.
      let idx = CString::new(zone_id)
        .ok()
        // SAFETY: if_nametoindex is safe to call with a valid C string
        .map(|name| unsafe { libc::if_nametoindex(name.as_ptr()) })
        .unwrap_or(0);
      return Ok(idx);
    }
    #[cfg(not(any(unix, windows)))]
    return Ok(0);
  }

  // No zone id was provided. Try to find interface by matching the address.
  #[cfg(unix)]
  {
    resolve_ipv6_interface_by_address(addr_str, |name| {
      // SAFETY: if_nametoindex is safe with a valid C string from getifaddrs.
      unsafe { libc::if_nametoindex(name.as_ptr()) }
    })
  }
  #[cfg(not(unix))]
  {
    // Default to 0 if we couldn't resolve.
    Ok(0)
  }
}

#[cfg(unix)]
struct IfAddrsList(*mut libc::ifaddrs);

#[cfg(unix)]
impl Drop for IfAddrsList {
  fn drop(&mut self) {
    if !self.0.is_null() {
      // SAFETY: this guard is created only for a list returned by
      // getifaddrs, and it owns the list until this single drop.
      unsafe { libc::freeifaddrs(self.0) };
    }
  }
}

#[cfg(unix)]
fn resolve_ipv6_interface_by_address(
  addr_str: &str,
  if_nametoindex: impl Fn(&std::ffi::CStr) -> u32,
) -> Result<u32, NodeUdpError> {
  use std::ffi::CStr;

  let target_addr =
    Ipv6Addr::from_str(addr_str.split('%').next().unwrap_or(addr_str))?;

  // Get all interfaces and find one with matching address.
  let mut addrs: *mut libc::ifaddrs = std::ptr::null_mut();
  // SAFETY: getifaddrs is safe to call with a valid pointer.
  if unsafe { libc::getifaddrs(&mut addrs) } != 0 {
    return Ok(0);
  }
  let addrs = IfAddrsList(addrs);

  let mut result = 0;
  let mut current = addrs.0;
  while !current.is_null() {
    // SAFETY: we checked current is not null.
    let ifa = unsafe { &*current };
    if !ifa.ifa_addr.is_null() {
      // SAFETY: we checked ifa_addr is not null.
      let family = unsafe { (*ifa.ifa_addr).sa_family };
      if family == libc::AF_INET6 as libc::sa_family_t {
        // SAFETY: we verified this is AF_INET6.
        let sockaddr_in6 =
          unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in6) };
        let addr = Ipv6Addr::from(sockaddr_in6.sin6_addr.s6_addr);
        if addr == target_addr {
          // SAFETY: ifa_name is a valid C string for the lifetime of addrs.
          let name = unsafe { CStr::from_ptr(ifa.ifa_name) };
          if let Ok(name_str) = name.to_str()
            && let Ok(c_name) = std::ffi::CString::new(name_str)
          {
            result = if_nametoindex(&c_name);
            if result != 0 {
              break;
            }
          }
        }
      }
    }
    current = ifa.ifa_next;
  }

  Ok(result)
}

#[op2]
pub fn op_node_udp_join_multi_v6(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] address: &str,
  #[string] interface_addr: Option<String>,
) -> Result<(), NodeUdpError> {
  let resource = state.resource_table.get::<NodeUdpSocketResource>(rid)?;

  let addr = Ipv6Addr::from_str(address)?;
  let iface = resolve_ipv6_interface(interface_addr.as_deref())?;
  let port = resource.socket.local_addr()?.port();
  deno_net::check_multicast_membership_permission(
    state.borrow_mut::<PermissionsContainer>(),
    std::net::IpAddr::V6(addr),
    port,
    "dgram.addMembership()",
  )?;
  resource.socket.join_multicast_v6(&addr, iface)?;
  Ok(())
}

#[op2]
pub fn op_node_udp_leave_multi_v6(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] address: &str,
  #[string] interface_addr: Option<String>,
) -> Result<(), NodeUdpError> {
  let resource = state.resource_table.get::<NodeUdpSocketResource>(rid)?;

  let addr = Ipv6Addr::from_str(address)?;
  let iface = resolve_ipv6_interface(interface_addr.as_deref())?;
  resource.socket.leave_multicast_v6(&addr, iface)?;
  Ok(())
}

#[op2(fast)]
pub fn op_node_udp_set_broadcast(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  on: bool,
) -> Result<(), NodeUdpError> {
  let resource = state.resource_table.get::<NodeUdpSocketResource>(rid)?;
  resource.socket.set_broadcast(on)?;
  Ok(())
}

#[op2(fast)]
pub fn op_node_udp_set_multicast_loopback(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  is_v4: bool,
  on: bool,
) -> Result<(), NodeUdpError> {
  let resource = state.resource_table.get::<NodeUdpSocketResource>(rid)?;
  if is_v4 {
    resource.socket.set_multicast_loop_v4(on)?;
  } else {
    resource.socket.set_multicast_loop_v6(on)?;
  }
  Ok(())
}

#[op2(fast)]
pub fn op_node_udp_set_multicast_ttl(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[smi] ttl: u32,
) -> Result<(), NodeUdpError> {
  let resource = state.resource_table.get::<NodeUdpSocketResource>(rid)?;
  resource.socket.set_multicast_ttl_v4(ttl)?;
  Ok(())
}

#[op2(fast)]
pub fn op_node_udp_set_ttl(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[smi] ttl: u32,
) -> Result<(), NodeUdpError> {
  let resource = state.resource_table.get::<NodeUdpSocketResource>(rid)?;
  let sock_ref = socket2::SockRef::from(&resource.socket);
  sock_ref.set_ttl(ttl)?;
  Ok(())
}

#[op2(fast)]
pub fn op_node_udp_set_multicast_interface(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  is_ipv6: bool,
  #[string] interface_address: &str,
) -> Result<(), NodeUdpError> {
  let resource = state.resource_table.get::<NodeUdpSocketResource>(rid)?;
  let sock_ref = socket2::SockRef::from(&resource.socket);
  if is_ipv6 {
    let index = ipv6_interface_index(interface_address)?;
    sock_ref.set_multicast_if_v6(index)?;
  } else {
    let addr: Ipv4Addr = interface_address.parse().map_err(|_| {
      NodeUdpError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "invalid IPv4 address",
      ))
    })?;
    sock_ref.set_multicast_if_v4(&addr)?;
  }
  Ok(())
}

/// Parse an IPv6 interface address string to a network interface index.
/// Matches libuv's `uv__udp_set_multicast_interface6` behavior:
/// - Parses IPv6 address strings like `::%lo0`, `::%1`, `::`
/// - Extracts scope_id and resolves interface names via if_nametoindex
/// - Returns EINVAL for empty or unparseable addresses
fn ipv6_interface_index(interface_address: &str) -> Result<u32, NodeUdpError> {
  let einval =
    || NodeUdpError::Io(std::io::Error::from_raw_os_error(libc::EINVAL));

  if interface_address.is_empty() {
    return Err(einval());
  }

  // Check for scope ID separator
  if let Some(pos) = interface_address.rfind('%') {
    // Validate the address part before % is a valid IPv6 address
    let addr_part = &interface_address[..pos];
    if addr_part.parse::<Ipv6Addr>().is_err() {
      return Err(einval());
    }

    let scope_id = &interface_address[pos + 1..];
    if scope_id.is_empty() {
      return Err(einval());
    }

    // Try numeric scope ID first
    if let Ok(index) = scope_id.parse::<u32>() {
      return Ok(index);
    }

    // Resolve interface name to index
    #[cfg(unix)]
    {
      let name = std::ffi::CString::new(scope_id).map_err(|_| einval())?;
      // SAFETY: name is a valid CString
      let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
      // if_nametoindex returns 0 for unknown interfaces, which is
      // acceptable as "default selection" (matches libuv behavior)
      return Ok(index);
    }
    #[cfg(windows)]
    {
      let name = std::ffi::CString::new(scope_id).map_err(|_| einval())?;
      // SAFETY: name is a valid CString
      let index = unsafe {
        windows_sys::Win32::NetworkManagement::IpHelper::if_nametoindex(
          name.as_ptr() as *const u8,
        )
      };
      return Ok(index);
    }
    #[cfg(not(any(unix, windows)))]
    return Ok(0);
  }

  // No scope ID separator — try parsing as plain IPv6 address
  if interface_address.parse::<Ipv6Addr>().is_ok() {
    return Ok(0);
  }

  Err(einval())
}

fn source_specific_multicast(
  socket: &UdpSocket,
  source_addr: Ipv4Addr,
  group_addr: Ipv4Addr,
  interface_addr: Ipv4Addr,
  option: i32,
) -> Result<(), NodeUdpError> {
  #[cfg(unix)]
  {
    let mreq = libc::ip_mreq_source {
      imr_multiaddr: libc::in_addr {
        s_addr: u32::from(group_addr).to_be(),
      },
      imr_sourceaddr: libc::in_addr {
        s_addr: u32::from(source_addr).to_be(),
      },
      imr_interface: libc::in_addr {
        s_addr: u32::from(interface_addr).to_be(),
      },
    };

    // SAFETY: We pass a valid socket fd, level, option, and correctly-sized struct.
    let ret = unsafe {
      libc::setsockopt(
        std::os::fd::AsRawFd::as_raw_fd(socket),
        libc::IPPROTO_IP,
        option,
        &mreq as *const libc::ip_mreq_source as *const libc::c_void,
        std::mem::size_of::<libc::ip_mreq_source>() as libc::socklen_t,
      )
    };
    if ret != 0 {
      return Err(std::io::Error::last_os_error().into());
    }
  }

  #[cfg(windows)]
  {
    use std::os::windows::io::AsRawSocket;

    #[repr(C)]
    struct IpMreqSource {
      imr_multiaddr: u32,
      imr_sourceaddr: u32,
      imr_interface: u32,
    }

    let mreq = IpMreqSource {
      imr_multiaddr: u32::from(group_addr).to_be(),
      imr_sourceaddr: u32::from(source_addr).to_be(),
      imr_interface: u32::from(interface_addr).to_be(),
    };

    // SAFETY: We pass a valid socket, level, option, and correctly-sized struct.
    let ret = unsafe {
      windows_sys::Win32::Networking::WinSock::setsockopt(
        socket.as_raw_socket() as usize,
        windows_sys::Win32::Networking::WinSock::IPPROTO_IP,
        option,
        &mreq as *const IpMreqSource as *const u8,
        std::mem::size_of::<IpMreqSource>() as i32,
      )
    };
    if ret != 0 {
      return Err(std::io::Error::last_os_error().into());
    }
  }

  Ok(())
}

#[op2(fast)]
pub fn op_node_udp_join_source_specific(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] source_address: &str,
  #[string] group_address: &str,
  #[string] interface_address: &str,
) -> Result<(), NodeUdpError> {
  let resource = state.resource_table.get::<NodeUdpSocketResource>(rid)?;

  let source_addr = Ipv4Addr::from_str(source_address)?;
  let group_addr = Ipv4Addr::from_str(group_address)?;
  let interface_addr = Ipv4Addr::from_str(interface_address)?;

  #[cfg(unix)]
  let option = libc::IP_ADD_SOURCE_MEMBERSHIP;
  #[cfg(windows)]
  let option =
    windows_sys::Win32::Networking::WinSock::IP_ADD_SOURCE_MEMBERSHIP;

  source_specific_multicast(
    &resource.socket,
    source_addr,
    group_addr,
    interface_addr,
    option,
  )
}

#[op2(fast)]
pub fn op_node_udp_leave_source_specific(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] source_address: &str,
  #[string] group_address: &str,
  #[string] interface_address: &str,
) -> Result<(), NodeUdpError> {
  let resource = state.resource_table.get::<NodeUdpSocketResource>(rid)?;

  let source_addr = Ipv4Addr::from_str(source_address)?;
  let group_addr = Ipv4Addr::from_str(group_address)?;
  let interface_addr = Ipv4Addr::from_str(interface_address)?;

  #[cfg(unix)]
  let option = libc::IP_DROP_SOURCE_MEMBERSHIP;
  #[cfg(windows)]
  let option =
    windows_sys::Win32::Networking::WinSock::IP_DROP_SOURCE_MEMBERSHIP;

  source_specific_multicast(
    &resource.socket,
    source_addr,
    group_addr,
    interface_addr,
    option,
  )
}

// Not `async(deferred)`: Node.js sends with `uv_udp_try_send` first, and a
// datagram that goes out at once (the usual case) has its callback run from
// `process.nextTick`, ahead of the loop. A result that is ready on the first
// poll is the same case here.
#[op2]
#[smi]
pub async fn op_node_udp_send(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[buffer] buf: JsBuffer,
  #[string] hostname: String,
  #[smi] port: u16,
) -> Result<usize, NodeUdpError> {
  {
    state
      .borrow_mut()
      .borrow_mut::<PermissionsContainer>()
      .check_net_connect(&(&hostname, Some(port)), "socket.send()")?;
  }

  let resource = state
    .borrow()
    .resource_table
    .get::<NodeUdpSocketResource>(rid)?;

  let addr: SocketAddr = loopback_if_unspecified(
    deno_net::resolve_addr::resolve_addr_sync(&hostname, port)?
      .next()
      .ok_or(NodeUdpError::NoResolvedAddress)?,
  );
  {
    state
      .borrow_mut()
      .borrow_mut::<PermissionsContainer>()
      .check_net_connect_resolved(&addr.ip(), addr.port(), "socket.send()")?;
  }

  let cancel = RcRef::map(&resource, |r| &r.cancel);
  let nwritten = resource
    .socket
    .send_to(&buf, &addr)
    .or_cancel(cancel)
    .await??;

  Ok(nwritten)
}

/// The error libuv gives for an option the platform lacks (ENOTSUP), so
/// the polyfill reports it under that code. It carries the OS error number
/// because the error code is derived from it: an `ErrorKind::Unsupported`
/// without one is reported as ENOSYS. On Windows libuv's UV_ENOTSUP is
/// ERROR_NOT_SUPPORTED, which deno_error maps to ENOTSUP.
fn not_supported_error() -> std::io::Error {
  #[cfg(unix)]
  {
    std::io::Error::from_raw_os_error(libc::ENOTSUP)
  }
  #[cfg(windows)]
  {
    std::io::Error::from_raw_os_error(
      windows_sys::Win32::Foundation::ERROR_NOT_SUPPORTED as i32,
    )
  }
  #[cfg(not(any(unix, windows)))]
  {
    std::io::Error::new(
      std::io::ErrorKind::Unsupported,
      "operation not supported on socket",
    )
  }
}

/// denext: a datagram sent to the unspecified address (`0.0.0.0` / `::`)
/// goes to the loopback address of its family, as Linux does in the kernel
/// and libuv does on Windows. macOS sends it to the host's primary interface
/// address instead, where a host firewall that drops traffic to that address
/// (the application firewall's "block all incoming connections", pf) refuses
/// it with EHOSTUNREACH. A socket's own `address()` is a natural destination:
/// a cluster's shared socket bound to `0.0.0.0` is announced to the primary
/// with that address, and parallel/test-cluster-dgram-1.js sends to it.
fn loopback_if_unspecified(mut addr: SocketAddr) -> SocketAddr {
  if addr.ip().is_unspecified() {
    addr.set_ip(match addr {
      SocketAddr::V4(_) => std::net::Ipv4Addr::LOCALHOST.into(),
      SocketAddr::V6(_) => std::net::Ipv6Addr::LOCALHOST.into(),
    });
  }
  addr
}

#[derive(serde::Serialize)]
pub struct RecvResult {
  pub nread: usize,
  pub hostname: String,
  pub port: u16,
}

/// Receives the first datagram of a burst (see `UDP_RECV_BURST` in
/// `udp_wrap.ts`).
///
/// `async(deferred)`: libuv reads datagrams in the poll phase, so one that is
/// already queued when the call is made is delivered by the loop, like one
/// that arrives later: after `process.nextTick` callbacks and the timers that
/// are due. Delivered at once, every queued datagram was received from the
/// microtask of the one before, and a socket that was sent to faster than it
/// read kept the loop from everything else.
#[op2(async(deferred))]
#[serde]
pub async fn op_node_udp_recv(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[buffer] buf: JsBuffer,
) -> Result<RecvResult, NodeUdpError> {
  udp_recv(state, rid, buf).await
}

/// Receives a later datagram of a burst: one that is already queued is
/// returned at once, as libuv reads up to 32 datagrams for one readiness
/// event before it goes back to the loop.
#[op2]
#[serde]
pub async fn op_node_udp_recv_queued(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[buffer] buf: JsBuffer,
) -> Result<RecvResult, NodeUdpError> {
  udp_recv(state, rid, buf).await
}

async fn udp_recv(
  state: Rc<RefCell<OpState>>,
  rid: ResourceId,
  mut buf: JsBuffer,
) -> Result<RecvResult, NodeUdpError> {
  let resource = state
    .borrow()
    .resource_table
    .get::<NodeUdpSocketResource>(rid)?;

  let cancel = RcRef::map(&resource, |r| &r.cancel);
  let (nread, remote_addr) = resource
    .socket
    .recv_from(&mut buf)
    .or_cancel(cancel)
    .await??;

  Ok(RecvResult {
    nread,
    hostname: remote_addr.ip().to_string(),
    port: remote_addr.port(),
  })
}

/// Return an owned dup of the bound UDP socket's file descriptor, for use as
/// the payload of an SCM_RIGHTS cmsg on an IPC channel.  The caller owns the
/// returned fd and must close it after `sendmsg` has attached it to the IPC
/// message.  Returns -1 on platforms that don't support fd-passing (Windows).
#[op2(fast)]
#[smi]
pub fn op_node_udp_fd_for_ipc(
  state: &mut OpState,
  #[smi] rid: ResourceId,
) -> Result<i32, NodeUdpError> {
  let resource = state.resource_table.get::<NodeUdpSocketResource>(rid)?;
  #[cfg(unix)]
  {
    use std::os::unix::io::AsRawFd;
    let fd = resource.socket.as_raw_fd();
    if fd < 0 {
      return Ok(-1);
    }
    // SAFETY: fd is a valid open file descriptor. F_DUPFD_CLOEXEC
    // atomically dups and sets CLOEXEC, avoiding a race window.
    let dup = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    Ok(dup)
  }
  #[cfg(not(unix))]
  {
    let _ = resource;
    Ok(-1)
  }
}

/// The descriptor policy of `PipeWrap::open` / `TCPWrap::open` (see
/// `FdTable::begin_uv_adopt`) for a UDP socket, which takes ownership of the
/// fd (and closes it on drop): a descriptor received for this isolate may be
/// adopted once; an untracked one only with all permissions, since the OS
/// descriptor table is shared by every isolate in the process; stdio and fds
/// this isolate already tracks are refused.
#[cfg(unix)]
fn claim_udp_fd(state: &mut OpState, fd: i32) -> Result<(), NodeUdpError> {
  if fd < 0 {
    return Err(std::io::Error::from_raw_os_error(libc::EBADF).into());
  }
  let fd_table = state.borrow::<deno_io::FdTable>();
  if fd_table.is_uv_adoptable(fd) {
    state.borrow_mut::<deno_io::FdTable>().remove(fd);
    return Ok(());
  }
  if fd <= 2 || fd_table.contains(fd) {
    return Err(std::io::Error::from_raw_os_error(libc::EEXIST).into());
  }
  if state
    .borrow::<PermissionsContainer>()
    .check_has_all_permissions(std::path::Path::new(
      "node:dgram.Socket({ fd })",
    ))
    .is_err()
  {
    return Err(std::io::Error::from_raw_os_error(libc::EPERM).into());
  }
  Ok(())
}

/// Adopt an existing file descriptor as a UDP socket resource.  Used on the
/// receiving side of IPC handle passing.
#[op2]
#[serde]
pub fn op_node_udp_open(
  state: &mut OpState,
  #[smi] fd: i32,
) -> Result<(ResourceId, String, u16), NodeUdpError> {
  #[cfg(unix)]
  {
    use std::os::unix::io::FromRawFd;
    claim_udp_fd(state, fd)?;
    // SAFETY: claim_udp_fd accepted the fd: it was received for this isolate
    // (SCM_RIGHTS), or the isolate has all permissions and nothing else in it
    // tracks the fd.
    let std_socket = unsafe { std::net::UdpSocket::from_raw_fd(fd) };
    std_socket.set_nonblocking(true)?;
    let local_addr = std_socket.local_addr()?;
    let socket = UdpSocket::from_std(std_socket)?;
    let resource = NodeUdpSocketResource {
      socket,
      cancel: Default::default(),
    };
    let rid = state.resource_table.add(resource);
    Ok((rid, local_addr.ip().to_string(), local_addr.port()))
  }
  #[cfg(not(unix))]
  {
    // libuv's Windows IPC passes only TCP handles (ENOTSUP otherwise).
    let _ = (state, fd);
    Err(NodeUdpError::Io(not_supported_error()))
  }
}

#[cfg(test)]
mod tests {
  #[test]
  fn sends_to_the_unspecified_address_go_to_loopback() {
    use std::net::SocketAddr;
    let to = |s: &str| super::loopback_if_unspecified(s.parse().unwrap());
    assert_eq!(
      to("0.0.0.0:41234"),
      "127.0.0.1:41234".parse::<SocketAddr>().unwrap()
    );
    assert_eq!(
      to("[::]:41234"),
      "[::1]:41234".parse::<SocketAddr>().unwrap()
    );
    assert_eq!(
      to("10.1.2.3:7"),
      "10.1.2.3:7".parse::<SocketAddr>().unwrap()
    );
  }

  #[cfg(unix)]
  #[test]
  fn ipv6_interface_resolution_handles_if_nametoindex_failure() {
    use std::cell::Cell;

    let lookup_count = Cell::new(0);
    let result = super::resolve_ipv6_interface_by_address("::1", |_| {
      lookup_count.set(lookup_count.get() + 1);
      0
    })
    .expect("interface resolution should not fail");
    assert!(lookup_count.get() > 0);
    assert_eq!(result, 0);
  }
}
