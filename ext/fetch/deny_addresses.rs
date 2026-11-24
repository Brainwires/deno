// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: `Deno.createHttpClient({ denyAddresses })`, a runtime SSRF guard.
//!
//! A client made with a deny list refuses to connect to an address in it.
//! The check is on IP addresses, never on names: the connector resolves a
//! name itself, refuses when any address it resolved is denied, connects
//! only to the addresses it checked (so a second lookup can't rebind the
//! name), and checks the connected socket's peer address once more before a
//! byte is written. Every connection the client opens goes through it:
//! the first request, each redirect hop (a new request on the same client),
//! every address happy eyeballs tries, and pooled connections (checked when
//! they were opened).
//!
//! An embedder (a runtime for web pages, which has no `Deno` namespace to
//! make such a client with) sets the same guard for a whole runtime by
//! putting an [`EmbedderDenyList`] in its `OpState`.

use std::fmt;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::Ipv6Addr;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use deno_core::OpState;
use ipnet::IpNet;
use ipnet::Ipv4Net;
use ipnet::Ipv6Net;

/// The `code` of the `cause` of the `TypeError` a denied connection fails
/// a `fetch()` with.
pub const ADDRESS_DENIED_CODE: &str = "ERR_ADDRESS_DENIED";

/// The ranges a client refuses to connect to.
#[derive(Debug, Default)]
pub struct AddressDenyList {
  nets: Vec<IpNet>,
}

/// denext: an embedder's deny list for a whole runtime, put in its `OpState`
/// (`state.put(EmbedderDenyList(Arc::new(list)))`) before any script runs.
/// With one there,
///
/// - the runtime's default `fetch()` client and every client
///   `Deno.createHttpClient()` makes deny its ranges (added to the client's
///   own `denyAddresses`),
/// - `WebSocket` and `WebSocketStream` connect through a connector that
///   denies them, whichever client they were given (`deno_websocket`),
/// - `RTCPeerConnection` and `RTCIceTransport` drop remote candidates in
///   them, refuse STUN/TURN servers that resolve to them and send no
///   datagram to them (`deno_webrtc`).
///
/// Without one nothing changes. Scripts can neither read nor replace it.
#[derive(Clone, Debug)]
pub struct EmbedderDenyList(pub Arc<AddressDenyList>);

impl EmbedderDenyList {
  /// The embedder's list in `state`, when it has one that denies anything.
  pub fn from_state(state: &OpState) -> Option<Arc<AddressDenyList>> {
    state
      .try_borrow::<EmbedderDenyList>()
      .map(|list| list.0.clone())
      .filter(|list| !list.is_empty())
  }

  /// `own` (a client's `denyAddresses`) with the embedder's list in `state`
  /// added.
  pub fn added_to(
    state: &OpState,
    own: Option<Arc<AddressDenyList>>,
  ) -> Option<Arc<AddressDenyList>> {
    match (Self::from_state(state), own) {
      (Some(embedder), Some(own)) => Some(own.with(&embedder)),
      (embedder, own) => embedder.or(own),
    }
  }
}

fn v4(a: u8, b: u8, c: u8, d: u8, prefix: u8) -> IpNet {
  IpNet::V4(Ipv4Net::new(Ipv4Addr::new(a, b, c, d), prefix).unwrap())
}

fn v6(segments: [u16; 8], prefix: u8) -> IpNet {
  let [a, b, c, d, e, f, g, h] = segments;
  IpNet::V6(
    Ipv6Net::new(Ipv6Addr::new(a, b, c, d, e, f, g, h), prefix).unwrap(),
  )
}

/// The ranges a preset names.
fn preset(name: &str) -> Option<Vec<IpNet>> {
  Some(match name {
    // the host itself; 0.0.0.0 and :: reach it too on Linux and macOS
    "loopback" => vec![
      v4(127, 0, 0, 0, 8),
      v4(0, 0, 0, 0, 8),
      v6([0, 0, 0, 0, 0, 0, 0, 1], 128),
      v6([0, 0, 0, 0, 0, 0, 0, 0], 128),
    ],
    // RFC 1918, carrier-grade NAT (RFC 6598) and IPv6 unique local
    "private" => vec![
      v4(10, 0, 0, 0, 8),
      v4(172, 16, 0, 0, 12),
      v4(192, 168, 0, 0, 16),
      v4(100, 64, 0, 0, 10),
      v6([0xfc00, 0, 0, 0, 0, 0, 0, 0], 7),
    ],
    "link-local" => vec![
      v4(169, 254, 0, 0, 16),
      v6([0xfe80, 0, 0, 0, 0, 0, 0, 0], 10),
    ],
    // the cloud instance metadata services: AWS, GCP, Azure, OpenStack and
    // DigitalOcean on 169.254.169.254, AWS's IPv6 endpoint, Alibaba Cloud,
    // Azure's WireServer (a public-range address: no other preset holds it)
    // and Oracle Cloud's
    "metadata" => vec![
      v4(169, 254, 169, 254, 32),
      v6([0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254], 128),
      v4(100, 100, 100, 200, 32),
      v4(168, 63, 129, 16, 32),
      v4(192, 0, 0, 192, 32),
    ],
    _ => return None,
  })
}

/// An address in the form it is compared in: an IPv4-mapped IPv6 address
/// (`::ffff:127.0.0.1`) is the IPv4 address it carries, which is where a
/// connection to it goes.
fn canonical(ip: IpAddr) -> IpAddr {
  match ip {
    IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
      Some(v4) => IpAddr::V4(v4),
      None => IpAddr::V6(v6),
    },
    ip => ip,
  }
}

/// The IPv4 addresses an IPv6 address carries, where a connection to it
/// can end up at one: the forms a translator or a tunnel turns back into
/// IPv4 on the way.
///
/// - IPv4-compatible `::a.b.c.d` (deprecated, still parsed and routed by
///   some stacks) and IPv4-translated `::ffff:0:a.b.c.d` (SIIT, RFC 7915);
/// - NAT64: the well-known prefix `64:ff9b::/96` (RFC 6052), and in the
///   local-use prefix `64:ff9b:1::/48` (RFC 8215) the address in the last
///   32 bits (a `/96` inside it) and the one RFC 6052 puts after a `/48`
///   (bits 48-63 and 72-87). Which of the two a network uses can't be told
///   from the address, so both are taken;
/// - 6to4 `2002:a.b.c.d::/48` (RFC 3056);
/// - Teredo `2001::/32` (RFC 4380): the client's address, stored inverted
///   in the last 32 bits.
///
/// `::` and `::1` are not here (they are IPv6's own unspecified and
/// loopback addresses), and the IPv4-mapped form is [`canonical`]'s.
fn embedded_v4(v6: Ipv6Addr) -> Vec<Ipv4Addr> {
  let s = v6.segments();
  let v4 = |hi: u16, lo: u16| Ipv4Addr::from(((hi as u32) << 16) | lo as u32);
  if s[..6] == [0; 6] {
    return if s[6] == 0 && s[7] <= 1 {
      Vec::new()
    } else {
      vec![v4(s[6], s[7])]
    };
  }
  if s[..6] == [0, 0, 0, 0, 0xffff, 0] || s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
    return vec![v4(s[6], s[7])];
  }
  if s[..3] == [0x64, 0xff9b, 1] {
    return vec![v4(s[6], s[7]), v4(s[3], (s[4] << 8) | (s[5] >> 8))];
  }
  if s[0] == 0x2002 {
    return vec![v4(s[1], s[2])];
  }
  if s[0] == 0x2001 && s[1] == 0 {
    return vec![v4(!s[6], !s[7])];
  }
  Vec::new()
}

impl AddressDenyList {
  /// Parses `denyAddresses`: CIDR ranges (`10.0.0.0/8`, `fc00::/7`), single
  /// addresses (`192.0.2.1`, `::1`, `[::1]`) and the presets `"loopback"`,
  /// `"private"`, `"link-local"` and `"metadata"`. The error names the entry
  /// that is none of these.
  pub fn parse(entries: &[String]) -> Result<Self, String> {
    let mut nets = Vec::new();
    for entry in entries {
      let trimmed = entry.trim();
      if let Some(preset) = preset(trimmed) {
        nets.extend(preset);
        continue;
      }
      let bare = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
      if let Ok(net) = IpNet::from_str(bare) {
        // a mapped range (`::ffff:127.0.0.0/104`) holds IPv4 addresses
        nets.push(match net {
          IpNet::V6(net) if net.prefix_len() >= 96 => {
            match net.network().to_ipv4_mapped() {
              Some(addr) => IpNet::V4(
                Ipv4Net::new(addr, net.prefix_len() - 96)
                  .map_err(|_| entry.clone())?,
              ),
              None => IpNet::V6(net),
            }
          }
          net => net,
        });
        continue;
      }
      match IpAddr::from_str(bare) {
        Ok(ip) => nets.push(IpNet::from(canonical(ip))),
        Err(_) => return Err(entry.clone()),
      }
    }
    Ok(Self { nets })
  }

  pub fn is_empty(&self) -> bool {
    self.nets.is_empty()
  }

  /// A list that denies what `self` or `other` denies (`self` itself when
  /// they are the same list or `other` is empty).
  pub fn with(self: &Arc<Self>, other: &Arc<Self>) -> Arc<Self> {
    if Arc::ptr_eq(self, other) || other.is_empty() {
      return self.clone();
    }
    let mut nets = self.nets.clone();
    for net in &other.nets {
      if !nets.contains(net) {
        nets.push(*net);
      }
    }
    Arc::new(Self { nets })
  }

  /// The range that denies `ip`, if one does: a range that holds the
  /// address itself, or one that holds an IPv4 address it carries
  /// ([`embedded_v4`]: NAT64, 6to4, Teredo, the IPv4-compatible and
  /// IPv4-translated forms), which is where a connection to it can go.
  pub fn denied(&self, ip: IpAddr) -> Option<IpNet> {
    let ip = canonical(ip);
    let find =
      |ip: IpAddr| self.nets.iter().find(|net| net.contains(&ip)).copied();
    find(ip).or_else(|| match ip {
      IpAddr::V6(v6) => embedded_v4(v6)
        .into_iter()
        .find_map(|v4| find(IpAddr::V4(v4))),
      IpAddr::V4(_) => None,
    })
  }

  /// `Ok` when `addr` may be connected to.
  pub fn check(&self, addr: SocketAddr) -> Result<(), AddressDeniedError> {
    match self.denied(addr.ip()) {
      Some(net) => Err(AddressDeniedError::Denied { addr, net }),
      None => Ok(()),
    }
  }
}

/// Why a client with `denyAddresses` refused a connection.
#[derive(Debug)]
pub enum AddressDeniedError {
  /// The address (resolved, given as a literal, or the connected peer) is
  /// in a denied range.
  Denied { addr: SocketAddr, net: IpNet },
  /// A destination behind a proxy whose name doesn't resolve here: the
  /// proxy resolves it on its own, so where it leads can't be checked.
  Unresolved { host: String, port: u16 },
}

impl fmt::Display for AddressDeniedError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Denied { addr, net } => write!(
        f,
        "{ADDRESS_DENIED_CODE}: connection to {addr} refused: the address is in {net}, which this client's denyAddresses deny"
      ),
      Self::Unresolved { host, port } => write!(
        f,
        "{ADDRESS_DENIED_CODE}: connection to {host}:{port} through a proxy refused: the name doesn't resolve locally, so its address can't be checked against this client's denyAddresses"
      ),
    }
  }
}

impl std::error::Error for AddressDeniedError {}

/// Finds an [`AddressDeniedError`] anywhere in an error's source chain.
pub fn find_address_denied_error<'a>(
  err: &'a (dyn std::error::Error + 'static),
) -> Option<&'a AddressDeniedError> {
  let mut current = Some(err);
  while let Some(err) = current {
    if let Some(found) = err.downcast_ref::<AddressDeniedError>() {
      return Some(found);
    }
    // `io::Error::source()` skips a custom payload, so look inside it
    if let Some(found) = err
      .downcast_ref::<std::io::Error>()
      .and_then(|io_err| io_err.get_ref())
      .and_then(|inner| inner.downcast_ref::<AddressDeniedError>())
    {
      return Some(found);
    }
    current = err.source();
  }
  None
}

#[cfg(test)]
mod tests {
  use super::*;

  fn list(entries: &[&str]) -> AddressDenyList {
    AddressDenyList::parse(
      &entries.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    )
    .unwrap()
  }

  fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
  }

  #[test]
  fn ranges_single_addresses_and_presets() {
    let deny = list(&["10.0.0.0/8", "192.0.2.7", "[2001:db8::1]", "metadata"]);
    assert!(deny.denied(ip("10.1.2.3")).is_some());
    assert!(deny.denied(ip("11.0.0.1")).is_none());
    assert!(deny.denied(ip("192.0.2.7")).is_some());
    assert!(deny.denied(ip("192.0.2.8")).is_none());
    assert!(deny.denied(ip("2001:db8::1")).is_some());
    assert!(deny.denied(ip("169.254.169.254")).is_some());
    assert!(deny.denied(ip("169.254.169.253")).is_none());
    assert!(deny.denied(ip("fd00:ec2::254")).is_some());
  }

  #[test]
  fn a_mapped_address_is_the_ipv4_address_it_carries() {
    let deny = list(&["loopback"]);
    assert!(deny.denied(ip("::ffff:127.0.0.1")).is_some());
    assert!(deny.denied(ip("0.0.0.0")).is_some());
    assert!(deny.denied(ip("::")).is_some());
    assert!(deny.denied(ip("::1")).is_some());
    assert!(deny.denied(ip("::ffff:8.8.8.8")).is_none());
    let deny = list(&["::ffff:10.0.0.0/104"]);
    assert!(deny.denied(ip("10.9.9.9")).is_some());
    let deny = list(&["::ffff:10.0.0.1"]);
    assert!(deny.denied(ip("10.0.0.1")).is_some());
  }

  #[test]
  fn an_address_that_carries_an_ipv4_address_is_that_address_too() {
    let deny = list(&["private", "127.0.0.0/8", "metadata"]);
    for denied in [
      // NAT64, the well-known prefix: 10.0.0.5, 127.0.0.1, 169.254.169.254
      "64:ff9b::a00:5",
      "64:ff9b::10.0.0.5",
      "64:ff9b::7f00:1",
      "64:ff9b::a9fe:a9fe",
      // NAT64, the local-use prefix: in the last 32 bits, and after a /48
      "64:ff9b:1::a00:5",
      "64:ff9b:1:ffff:ffff:ffff:c0a8:101",
      "64:ff9b:1:a00:0:500::",
      // IPv4-translated (SIIT) and IPv4-compatible
      "::ffff:0:10.0.0.5",
      "::ffff:0:127.0.0.1",
      "::10.0.0.5",
      "::127.0.0.1",
      "::169.254.169.254",
      // 6to4
      "2002:a00:5::1",
      "2002:7f00:1::",
      "2002:c0a8:101:1234::1",
      // Teredo: the client's address inverted (10.0.0.5, 192.168.1.1)
      "2001:0:4136:e378:8000:63bf:f5ff:fffa",
      "2001::3f57:fefe",
    ] {
      assert!(deny.denied(ip(denied)).is_some(), "{denied}");
    }
    for allowed in [
      // the same forms around a public address (8.8.8.8, 1.1.1.1)
      "64:ff9b::808:808",
      "64:ff9b:1::808:808",
      "::ffff:0:8.8.8.8",
      "::8.8.8.8",
      "2002:808:808::1",
      "2001::f7f7:f7f7",
      // not a carrier: documentation and a public address
      "2001:db8::a00:5",
      "2606:4700::a00:5",
    ] {
      assert!(deny.denied(ip(allowed)).is_none(), "{allowed}");
    }
    // the error names the IPv4 range that the carried address is in
    assert_eq!(
      deny.denied(ip("64:ff9b::a00:5")).unwrap().to_string(),
      "10.0.0.0/8"
    );

    // IPv6's own `::` and `::1` are not IPv4-compatible `0.0.0.0` / `0.0.0.1`
    let deny = list(&["0.0.0.0/8"]);
    assert!(deny.denied(ip("::")).is_none());
    assert!(deny.denied(ip("::1")).is_none());
    assert!(deny.denied(ip("::0.0.0.2")).is_some());
    // a range given in a carrier's own IPv6 form still denies as written
    let deny = list(&["64:ff9b::/96", "2002::/16"]);
    assert!(deny.denied(ip("64:ff9b::808:808")).is_some());
    assert!(deny.denied(ip("2002:808:808::1")).is_some());
    assert!(deny.denied(ip("8.8.8.8")).is_none());
  }

  #[test]
  fn private_and_link_local() {
    let deny = list(&["private", "link-local"]);
    for denied in [
      "10.0.0.1",
      "172.16.0.1",
      "172.31.255.255",
      "192.168.1.1",
      "100.64.0.1",
      "fd12::1",
      "169.254.1.1",
      "fe80::1",
    ] {
      assert!(deny.denied(ip(denied)).is_some(), "{denied}");
    }
    for allowed in ["172.32.0.1", "8.8.8.8", "2606:4700::1111", "127.0.0.1"] {
      assert!(deny.denied(ip(allowed)).is_none(), "{allowed}");
    }
  }

  #[test]
  fn two_lists_deny_what_either_denies() {
    let own = Arc::new(list(&["10.0.0.0/8"]));
    let embedder = Arc::new(list(&["loopback", "10.0.0.0/8"]));
    let both = own.with(&embedder);
    assert!(both.denied(ip("10.1.2.3")).is_some());
    assert!(both.denied(ip("127.0.0.1")).is_some());
    assert!(both.denied(ip("8.8.8.8")).is_none());
    assert!(own.denied(ip("127.0.0.1")).is_none());
    assert!(Arc::ptr_eq(&own.with(&own), &own));
    assert!(Arc::ptr_eq(
      &own.with(&Arc::new(AddressDenyList::default())),
      &own
    ));
  }

  #[test]
  fn the_embedder_list_is_read_from_the_op_state() {
    let mut state = OpState::new(None);
    assert!(EmbedderDenyList::from_state(&state).is_none());
    let own = Arc::new(list(&["10.0.0.0/8"]));
    let kept = EmbedderDenyList::added_to(&state, Some(own.clone())).unwrap();
    assert!(Arc::ptr_eq(&kept, &own));
    // an empty list is no list
    state.put(EmbedderDenyList(Arc::new(AddressDenyList::default())));
    assert!(EmbedderDenyList::from_state(&state).is_none());
    state.put(EmbedderDenyList(Arc::new(list(&["loopback"]))));
    let embedder = EmbedderDenyList::from_state(&state).unwrap();
    assert!(embedder.denied(ip("127.0.0.1")).is_some());
    let both = EmbedderDenyList::added_to(&state, Some(own)).unwrap();
    assert!(both.denied(ip("10.1.2.3")).is_some());
    assert!(both.denied(ip("::1")).is_some());
    let alone = EmbedderDenyList::added_to(&state, None).unwrap();
    assert!(alone.denied(ip("10.1.2.3")).is_none());
    assert!(alone.denied(ip("127.0.0.1")).is_some());
  }

  #[test]
  fn an_unknown_entry_is_named() {
    let err = AddressDenyList::parse(&["10.0.0.0/8".into(), "intranet".into()])
      .unwrap_err();
    assert_eq!(err, "intranet");
    assert!(AddressDenyList::parse(&["10.0.0.0/33".into()]).is_err());
  }

  #[test]
  fn the_error_is_found_through_an_io_error() {
    let err = std::io::Error::new(
      std::io::ErrorKind::PermissionDenied,
      AddressDeniedError::Denied {
        addr: "127.0.0.1:80".parse().unwrap(),
        net: "127.0.0.0/8".parse().unwrap(),
      },
    );
    let found = find_address_denied_error(&err).unwrap();
    assert!(found.to_string().starts_with(ADDRESS_DENIED_CODE));
  }

  // denext: the metadata preset names every provider's endpoint, the ones
  // outside the link-local range too, and nothing beside them.
  #[test]
  fn the_metadata_preset_names_each_endpoint() {
    let deny = list(&["metadata"]);
    for endpoint in [
      "169.254.169.254",
      "fd00:ec2::254",
      "100.100.100.200",
      "168.63.129.16",
      "192.0.0.192",
      "::ffff:168.63.129.16",
    ] {
      assert!(deny.denied(ip(endpoint)).is_some(), "{endpoint}");
    }
    for other in ["168.63.129.17", "192.0.0.191", "192.0.0.1", "168.63.0.1"] {
      assert!(deny.denied(ip(other)).is_none(), "{other}");
    }
  }
}
