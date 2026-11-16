// Copyright 2018-2026 the Deno authors. MIT license.

//! Name servers of the system DNS configuration with an IPv6 zone.
//!
//! A link-local name server is listed with the interface it is reached
//! through: `nameserver fe80::1%en0` in /etc/resolv.conf, the same string in
//! macOS's System Configuration (`State:/Network/Global/DNS`). hickory keeps
//! a name server as an `IpAddr`, which has no zone. On macOS its reader
//! failed the whole configuration on such an entry ("failed to parse
//! nameserver address: invalid IP address syntax"), so `Deno.resolveDns`
//! failed and `node:dns.getServers()` was empty; on Linux it kept the address
//! and dropped the zone, so a query to it could not leave the machine. Node
//! (c-ares) accepts the zone.
//!
//! So the zones are read here, turned into scope ids, and handed to the
//! resolver's sockets ([`ScopedRuntimeProvider`]), and an entry that does not
//! parse is skipped, not the whole configuration.

use std::future::Future;
use std::net::IpAddr;
use std::net::Ipv6Addr;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;

use hickory_resolver::net::runtime::DnsUdpSocket;
use hickory_resolver::net::runtime::RuntimeProvider;
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::net::runtime::TokioTime;

/// Name servers with a zone: each address and the scope id of its interface.
pub type Scopes = Vec<(Ipv6Addr, u32)>;

/// A name server address as the OS lists it: an IPv4 or IPv6 address, an
/// IPv6 one with a zone (`fe80::1%en0`, `fe80::1%2`). The zone is a scope id
/// when numeric, else an interface name (`if_nametoindex`); an interface the
/// machine doesn't have leaves the address unscoped. `None` when the address
/// itself doesn't parse.
pub fn parse_name_server(s: &str) -> Option<(IpAddr, Option<u32>)> {
  let s = s.trim();
  let Some((addr, zone)) = s.split_once('%') else {
    return s.parse().ok().map(|ip| (ip, None));
  };
  let ip: Ipv6Addr = addr.parse().ok()?;
  if zone.is_empty() {
    return None;
  }
  let scope = match zone.parse::<u32>() {
    Ok(id) => Some(id),
    Err(_) => interface_index(zone),
  };
  Some((IpAddr::V6(ip), scope.filter(|id| *id != 0)))
}

#[cfg(unix)]
fn interface_index(name: &str) -> Option<u32> {
  let name = std::ffi::CString::new(name).ok()?;
  // SAFETY: `name` is a valid NUL-terminated string.
  let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
  (index != 0).then_some(index)
}

#[cfg(not(unix))]
fn interface_index(_name: &str) -> Option<u32> {
  None
}

/// The scoped name servers of a resolv.conf: its `nameserver` lines whose
/// address has a zone that names a scope id.
pub fn scopes_from_resolv_conf(text: &str) -> Scopes {
  let mut scopes = Scopes::new();
  for line in text.lines() {
    let mut words = line.split_whitespace();
    if words.next() != Some("nameserver") {
      continue;
    }
    if let Some((IpAddr::V6(ip), Some(scope))) =
      words.next().and_then(parse_name_server)
      && !scopes.iter().any(|(i, _)| *i == ip)
    {
      scopes.push((ip, scope));
    }
  }
  scopes
}

/// The name servers in `entries` (System Configuration's `ServerAddresses`)
/// that parse, with the scoped ones; an entry that doesn't parse is skipped
/// with a log line.
pub fn name_servers_from_entries<'a>(
  entries: impl IntoIterator<Item = &'a str>,
) -> (Vec<IpAddr>, Scopes) {
  let mut servers = Vec::new();
  let mut scopes = Scopes::new();
  for entry in entries {
    match parse_name_server(entry) {
      Some((ip, scope)) => {
        if let (IpAddr::V6(v6), Some(scope)) = (ip, scope)
          && !scopes.iter().any(|(i, _)| *i == v6)
        {
          scopes.push((v6, scope));
        }
        servers.push(ip);
      }
      None => {
        log::warn!("skipping a name server that doesn't parse: {entry:?}");
      }
    }
  }
  (servers, scopes)
}

/// macOS: System Configuration's DNS state, as hickory's own reader
/// (`hickory_resolver::system_conf::apple`) reads it, but with zoned name
/// servers kept and an unparseable one skipped.
#[cfg(target_vendor = "apple")]
pub fn read_apple_system_conf() -> Result<
  (
    hickory_resolver::config::ResolverConfig,
    hickory_resolver::config::ResolverOpts,
    Scopes,
  ),
  hickory_proto::ProtoError,
> {
  use std::borrow::Cow;
  use std::str::FromStr;

  use hickory_proto::rr::Name;
  use hickory_resolver::config::NameServerConfig;
  use hickory_resolver::config::ResolverConfig;
  use hickory_resolver::config::ResolverOpts;
  use system_configuration::core_foundation::array::CFArray;
  use system_configuration::core_foundation::base::FromVoid;
  use system_configuration::core_foundation::base::ItemRef;
  use system_configuration::core_foundation::base::TCFType;
  use system_configuration::core_foundation::dictionary::CFDictionary;
  use system_configuration::core_foundation::string::CFString;
  use system_configuration::dynamic_store::SCDynamicStoreBuilder;

  let sc = SCDynamicStoreBuilder::new("deno").build();
  let dns_cfg = sc
    .ok_or("failed to access System Configuration dynamic store")?
    .get("State:/Network/Global/DNS")
    .ok_or("no DNS information in System Configuration")?
    .downcast_into::<CFDictionary>()
    .ok_or("DNS object in System Configuration is not a CFDictionary")?;

  let nameservers_cf = dns_cfg
    .find(CFString::from_static_string("ServerAddresses").as_CFTypeRef())
    .ok_or("no ServerAddresses key in DNS info")?;
  // SAFETY: ServerAddresses is a CFArray of CFStrings
  // (kSCPropNetDNSServerAddresses), as hickory's reader relies on too.
  let nameservers_cf: ItemRef<'_, CFArray<CFString>> =
    unsafe { CFArray::from_void(*nameservers_cf) };
  let mut entries: Vec<String> = Vec::new();
  for n in &*nameservers_cf {
    entries.push(Cow::from(&*n).into_owned());
  }
  let (servers, scopes) =
    name_servers_from_entries(entries.iter().map(String::as_str));
  if servers.is_empty() {
    return Err("no name server in System Configuration parses".into());
  }

  let mut search_domains = vec![];
  if let Some(search_domains_cf) =
    dns_cfg.find(CFString::from_static_string("SearchDomains").as_CFTypeRef())
  {
    // SAFETY: SearchDomains is a CFArray of CFStrings
    // (kSCPropNetDNSSearchDomains).
    let search_domains_cf: ItemRef<'_, CFArray<CFString>> =
      unsafe { CFArray::from_void(*search_domains_cf) };
    for s in &*search_domains_cf {
      let raw = Cow::from(&*s).into_owned();
      for domain in raw
        .split(|c: char| c.is_whitespace() || c == '\0')
        .filter(|d| !d.is_empty())
      {
        search_domains.push(Name::from_str(domain)?);
      }
    }
  }

  Ok((
    ResolverConfig::from_parts(
      None,
      search_domains,
      servers
        .into_iter()
        .map(NameServerConfig::udp_and_tcp)
        .collect(),
    ),
    ResolverOpts::default(),
    scopes,
  ))
}

/// `addr` with the scope id of its name server, when it is one of `scopes`
/// and has none yet.
fn scoped(addr: SocketAddr, scopes: &Scopes) -> SocketAddr {
  match addr {
    SocketAddr::V6(mut v6) if v6.scope_id() == 0 => {
      if let Some((_, scope)) = scopes.iter().find(|(ip, _)| ip == v6.ip()) {
        v6.set_scope_id(*scope);
      }
      SocketAddr::V6(v6)
    }
    other => other,
  }
}

/// hickory's Tokio runtime, but a packet or connection to a zoned name server
/// goes out through its interface (the scope id hickory's `IpAddr` can't
/// carry). Responses are matched by address and port only (hickory's UDP
/// client compares `ip()` and `port()`), so the scope id they come back with
/// changes nothing there.
#[derive(Clone)]
pub struct ScopedRuntimeProvider {
  inner: TokioRuntimeProvider,
  scopes: Arc<Scopes>,
}

impl ScopedRuntimeProvider {
  pub fn new(scopes: Scopes) -> Self {
    Self {
      inner: TokioRuntimeProvider::default(),
      scopes: Arc::new(scopes),
    }
  }
}

impl RuntimeProvider for ScopedRuntimeProvider {
  type Handle = <TokioRuntimeProvider as RuntimeProvider>::Handle;
  type Timer = <TokioRuntimeProvider as RuntimeProvider>::Timer;
  type Udp = ScopedUdpSocket;
  type Tcp = <TokioRuntimeProvider as RuntimeProvider>::Tcp;

  fn create_handle(&self) -> Self::Handle {
    self.inner.create_handle()
  }

  fn connect_tcp(
    &self,
    server_addr: SocketAddr,
    bind_addr: Option<SocketAddr>,
    timeout: Option<Duration>,
  ) -> Pin<Box<dyn Send + Future<Output = std::io::Result<Self::Tcp>>>> {
    self.inner.connect_tcp(
      scoped(server_addr, &self.scopes),
      bind_addr,
      timeout,
    )
  }

  fn bind_udp(
    &self,
    local_addr: SocketAddr,
    _server_addr: SocketAddr,
  ) -> Pin<Box<dyn Send + Future<Output = std::io::Result<Self::Udp>>>> {
    let scopes = self.scopes.clone();
    Box::pin(async move {
      let socket = tokio::net::UdpSocket::bind(local_addr).await?;
      Ok(ScopedUdpSocket { socket, scopes })
    })
  }
}

/// An unconnected UDP socket (as hickory's own) that sends to a zoned name
/// server with its scope id.
pub struct ScopedUdpSocket {
  socket: tokio::net::UdpSocket,
  scopes: Arc<Scopes>,
}

impl DnsUdpSocket for ScopedUdpSocket {
  type Time = TokioTime;

  fn poll_recv_from(
    &self,
    cx: &mut Context<'_>,
    buf: &mut [u8],
  ) -> Poll<std::io::Result<(usize, SocketAddr)>> {
    let mut read_buf = tokio::io::ReadBuf::new(buf);
    let addr =
      std::task::ready!(self.socket.poll_recv_from(cx, &mut read_buf))?;
    Poll::Ready(Ok((read_buf.filled().len(), addr)))
  }

  fn poll_send_to(
    &self,
    cx: &mut Context<'_>,
    buf: &[u8],
    target: SocketAddr,
  ) -> Poll<std::io::Result<usize>> {
    self
      .socket
      .poll_send_to(cx, buf, scoped(target, &self.scopes))
  }
}

#[cfg(test)]
mod tests {
  use std::net::Ipv4Addr;

  use super::*;

  // The /etc/resolv.conf of the report: a scoped link-local name server
  // first, an unscoped one after it.
  const RESOLV_CONF: &str = "\
# generated by the OS
nameserver fe80::bc74:eaff:fe3d:c964%7
nameserver 192.168.1.1
nameserver fe80::1%no-such-interface0
search lan
";

  #[test]
  fn parses_a_name_server_with_and_without_a_zone() {
    let ll: Ipv6Addr = "fe80::bc74:eaff:fe3d:c964".parse().unwrap();
    assert_eq!(
      parse_name_server("fe80::bc74:eaff:fe3d:c964%7"),
      Some((IpAddr::V6(ll), Some(7)))
    );
    assert_eq!(
      parse_name_server("192.168.1.1"),
      Some((IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), None))
    );
    assert_eq!(
      parse_name_server("2001:db8::1"),
      Some(("2001:db8::1".parse().unwrap(), None))
    );
    // An interface the machine doesn't have: the address, unscoped.
    assert_eq!(
      parse_name_server("fe80::bc74:eaff:fe3d:c964%no-such-interface0"),
      Some((IpAddr::V6(ll), None))
    );
    // Scope id 0 is no scope.
    assert_eq!(parse_name_server("fe80::1%0").unwrap().1, None);
    assert_eq!(parse_name_server("fe80::1%"), None);
    assert_eq!(parse_name_server("192.168.1.1%7"), None);
    assert_eq!(parse_name_server("not an address"), None);
  }

  #[cfg(unix)]
  #[test]
  fn an_interface_name_zone_is_its_index() {
    let name = if cfg!(target_vendor = "apple") {
      "lo0"
    } else {
      "lo"
    };
    let index = interface_index(name).expect("the loopback interface");
    assert_eq!(
      parse_name_server(&format!("fe80::1%{name}")),
      Some(("fe80::1".parse().unwrap(), Some(index)))
    );
  }

  #[test]
  fn resolv_conf_scopes() {
    assert_eq!(
      scopes_from_resolv_conf(RESOLV_CONF),
      vec![("fe80::bc74:eaff:fe3d:c964".parse().unwrap(), 7)]
    );
    assert_eq!(scopes_from_resolv_conf("nameserver 1.1.1.1\n"), vec![]);
  }

  // Linux: hickory's resolv.conf parser keeps the scoped name server (its
  // zone dropped, read back by `scopes_from_resolv_conf`) and the plain one,
  // and skips the entry whose zone it can't parse (a `-`), keeping the rest.
  #[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
  #[test]
  fn resolv_conf_with_a_scoped_name_server_keeps_every_server() {
    let (config, _) =
      hickory_resolver::system_conf::parse_resolv_conf(RESOLV_CONF).unwrap();
    let ips: Vec<IpAddr> = config.name_servers().iter().map(|n| n.ip).collect();
    assert_eq!(
      ips,
      vec![
        "fe80::bc74:eaff:fe3d:c964".parse::<IpAddr>().unwrap(),
        "192.168.1.1".parse().unwrap(),
      ]
    );
  }

  // macOS's ServerAddresses: a scoped entry first used to fail the whole
  // configuration; now it is kept with its scope and a bad entry is skipped.
  #[test]
  fn system_configuration_entries() {
    let (servers, scopes) = name_servers_from_entries([
      "fe80::bc74:eaff:fe3d:c964%7",
      "garbage",
      "192.168.1.1",
    ]);
    assert_eq!(
      servers,
      vec![
        "fe80::bc74:eaff:fe3d:c964".parse::<IpAddr>().unwrap(),
        "192.168.1.1".parse().unwrap(),
      ]
    );
    assert_eq!(
      scopes,
      vec![("fe80::bc74:eaff:fe3d:c964".parse().unwrap(), 7)]
    );
  }

  #[test]
  fn a_name_server_address_gets_its_scope() {
    let scopes: Scopes = vec![("fe80::1".parse().unwrap(), 7)];
    let to = |s: &str| s.parse::<SocketAddr>().unwrap();
    assert_eq!(scoped(to("[fe80::1]:53"), &scopes), to("[fe80::1%7]:53"));
    // Already scoped, another address, IPv4: unchanged.
    assert_eq!(scoped(to("[fe80::1%3]:53"), &scopes), to("[fe80::1%3]:53"));
    assert_eq!(scoped(to("[fe80::2]:53"), &scopes), to("[fe80::2]:53"));
    assert_eq!(scoped(to("1.1.1.1:53"), &scopes), to("1.1.1.1:53"));
  }
}
