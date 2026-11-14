// Copyright 2018-2026 the Deno authors. MIT license.

//! `Deno.webrtc.Network`: the server-side network settings the standard
//! doesn't have, shared by the connections that name it in
//! `RTCConfiguration.denoNetwork`.
//!
//! - **One UDP port for every connection** (`port`): the network binds it
//!   once and its reader hands each datagram to the connection it belongs
//!   to. A remote address is learned from its first STUN binding request,
//!   whose USERNAME (`<local ufrag>:<remote ufrag>`) names the connection by
//!   the ICE username fragment of its local description; responses from STUN
//!   servers are routed by transaction id. Anything else from an unknown
//!   address is dropped. TURN over UDP isn't available on a shared port
//!   (TURN over TCP / TLS is: those clients have their own connections).
//! - **A port range** (`portRange`): each connection binds its own socket
//!   on a port from the range.
//! - **Bind addresses and interfaces** (`bindAddresses`, `interfaces`,
//!   `includeLoopback`).
//! - **1:1 NAT** (`publicAddresses`): the public address is advertised in
//!   place of the private one (`host`) or beside it (`srflx`).
//! - **ICE-lite**, the ICE timers and the data channel message size.
//!
//! Every bind needs the listen permission for its address, and listing
//! interfaces `--allow-sys=networkInterfaces`; datagrams still go only to
//! addresses the net permission allows.

use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::Ipv6Addr;
use std::net::SocketAddr;
use std::rc::Rc;
use std::rc::Weak;
use std::time::Duration;

use bytes::BytesMut;
use deno_core::CancelFuture;
use deno_core::CancelHandle;
use serde::Deserialize;
use tokio::net::UdpSocket;

use crate::WebRtcError;
use crate::peer::Datagram;
use crate::peer::NetGuard;
use crate::peer::Peer;

/// The most remote addresses (and STUN transactions) a shared port
/// remembers; a flood of spoofed sources can't grow the maps without limit.
const MAX_ROUTES: usize = 65536;

#[derive(Deserialize, Default, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub struct IceTimeoutsArg {
  pub disconnected: Option<u64>,
  pub failed: Option<u64>,
  pub keep_alive: Option<u64>,
}

#[derive(Deserialize, Clone, Copy, Debug)]
pub struct PortRangeArg {
  pub min: u16,
  pub max: u16,
}

#[derive(Deserialize, Default, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct NetworkArg {
  pub port: Option<u16>,
  pub port_range: Option<PortRangeArg>,
  pub bind_addresses: Option<Vec<String>>,
  pub interfaces: Option<Vec<String>>,
  #[serde(default = "yes")]
  pub include_loopback: bool,
  #[serde(default)]
  pub public_addresses: Vec<String>,
  #[serde(default)]
  pub public_address_candidate: Option<String>,
  #[serde(default)]
  pub ice_lite: bool,
  pub ice_timeouts: Option<IceTimeoutsArg>,
  pub max_message_size: Option<u32>,
}

fn yes() -> bool {
  true
}

/// A public address and the private one it maps to (`None`: every private
/// address).
#[derive(Clone, Copy, Debug)]
pub struct PublicAddress {
  pub public: IpAddr,
  pub private: Option<IpAddr>,
}

#[derive(Clone, Debug, Default)]
pub struct NetworkOptions {
  pub port: Option<u16>,
  pub port_range: Option<(u16, u16)>,
  pub bind_addresses: Option<Vec<IpAddr>>,
  pub interfaces: Option<Vec<String>>,
  pub include_loopback: bool,
  pub public_addresses: Vec<PublicAddress>,
  /// `publicAddressCandidate: "srflx"`: advertise the public address as a
  /// server-reflexive candidate beside the host one.
  pub public_srflx: bool,
  pub ice_lite: bool,
  pub ice_timeouts:
    Option<(Option<Duration>, Option<Duration>, Option<Duration>)>,
  pub max_message_size: Option<u32>,
}

fn parse_ip(s: &str, what: &str) -> Result<IpAddr, WebRtcError> {
  s.trim_matches(['[', ']']).parse().map_err(|_| {
    WebRtcError::Type(format!("'{s}' is not an IP address ({what})"))
  })
}

impl NetworkOptions {
  pub fn parse(arg: NetworkArg) -> Result<Self, WebRtcError> {
    let bind_addresses = match arg.bind_addresses {
      Some(list) => Some(
        list
          .iter()
          .map(|a| parse_ip(a, "bindAddresses"))
          .collect::<Result<Vec<_>, _>>()?,
      ),
      None => None,
    };
    let mut public_addresses = Vec::new();
    for entry in &arg.public_addresses {
      let (public, private) = match entry.split_once('/') {
        Some((p, q)) => (
          parse_ip(p, "publicAddresses")?,
          Some(parse_ip(q, "publicAddresses")?),
        ),
        None => (parse_ip(entry, "publicAddresses")?, None),
      };
      public_addresses.push(PublicAddress { public, private });
    }
    if public_addresses.len() > 1
      && public_addresses.iter().any(|p| p.private.is_none())
    {
      return Err(WebRtcError::Type(
        "With several publicAddresses, each must be 'public/private'"
          .to_string(),
      ));
    }
    let ms = |v: Option<u64>| v.map(Duration::from_millis);
    Ok(Self {
      port: arg.port,
      port_range: arg.port_range.map(|r| (r.min, r.max)),
      bind_addresses,
      interfaces: arg.interfaces,
      include_loopback: arg.include_loopback,
      public_addresses,
      public_srflx: arg.public_address_candidate.as_deref() == Some("srflx"),
      ice_lite: arg.ice_lite,
      ice_timeouts: arg
        .ice_timeouts
        .map(|t| (ms(t.disconnected), ms(t.failed), ms(t.keep_alive))),
      max_message_size: arg.max_message_size,
    })
  }

  /// The public address a private one is advertised as, if any.
  pub fn public_for(&self, private: IpAddr) -> Option<IpAddr> {
    self
      .public_addresses
      .iter()
      .find(|p| {
        p.public.is_ipv4() == private.is_ipv4()
          && p.private.is_none_or(|q| q == private)
      })
      .map(|p| p.public)
  }

  /// The private address a public one stands for (host replacement).
  pub fn private_for(
    &self,
    public: IpAddr,
    hosts: &[IpAddr],
  ) -> Option<IpAddr> {
    self
      .public_addresses
      .iter()
      .find(|p| p.public == public)
      .and_then(|p| {
        p.private.or_else(|| {
          hosts
            .iter()
            .copied()
            .find(|h| h.is_ipv4() == public.is_ipv4() && !h.is_loopback())
        })
      })
  }
}

/// One of a shared port's sockets.
#[derive(Clone)]
pub struct SharedEndpoint {
  pub socket: Rc<UdpSocket>,
  pub bound: SocketAddr,
  pub hosts: Vec<IpAddr>,
}

pub struct Network {
  pub options: NetworkOptions,
  /// `port` networks: the sockets every connection shares.
  pub shared: Vec<SharedEndpoint>,
  by_ufrag: RefCell<HashMap<String, Weak<Peer>>>,
  by_remote: RefCell<HashMap<SocketAddr, Weak<Peer>>>,
  by_transaction: RefCell<HashMap<[u8; 12], Weak<Peer>>>,
  cancel: Rc<CancelHandle>,
  closed: Cell<bool>,
}

/// The STUN message type, transaction id and (for a binding request) the
/// local ufrag its USERNAME names, if `data` is a STUN message.
fn stun_info(data: &[u8]) -> Option<(u16, [u8; 12], Option<String>)> {
  if data.len() < 20 || data[0] >= 4 || data[4..8] != [0x21, 0x12, 0xa4, 0x42] {
    return None;
  }
  let typ = u16::from_be_bytes([data[0], data[1]]);
  let mut tid = [0u8; 12];
  tid.copy_from_slice(&data[8..20]);
  let len = u16::from_be_bytes([data[2], data[3]]) as usize;
  let body = data.get(20..20 + len)?;
  let mut ufrag = None;
  if typ == 0x0001 {
    let mut i = 0;
    while i + 4 <= body.len() {
      let attr = u16::from_be_bytes([body[i], body[i + 1]]);
      let alen = u16::from_be_bytes([body[i + 2], body[i + 3]]) as usize;
      let value = body.get(i + 4..i + 4 + alen)?;
      if attr == 0x0006 {
        let username = std::str::from_utf8(value).ok()?;
        ufrag = username.split(':').next().map(str::to_string);
        break;
      }
      i += 4 + alen.div_ceil(4) * 4;
    }
  }
  Some((typ, tid, ufrag))
}

impl Network {
  /// Binds a `port` network's sockets now, so a taken port or a refused
  /// permission is `createNetwork`'s error.
  pub fn new(
    options: NetworkOptions,
    guard: &dyn NetGuard,
  ) -> Result<Rc<Self>, WebRtcError> {
    let cancel = CancelHandle::new_rc();
    let mut shared = Vec::new();
    if let Some(port) = options.port {
      for (bind, hosts) in bind_plan(&options, guard)?.0 {
        let socket = crate::peer::bind_udp(SocketAddr::new(bind, port))
          .map_err(|e| {
            WebRtcError::Operation(format!(
              "Could not bind UDP {}: {e}",
              SocketAddr::new(bind, port)
            ))
          })?;
        let bound = socket.local_addr().map_err(crate::peer::rtc_err)?;
        shared.push(SharedEndpoint {
          socket: Rc::new(socket),
          bound,
          hosts,
        });
      }
    }
    let network = Rc::new(Self {
      options,
      shared,
      by_ufrag: RefCell::new(HashMap::new()),
      by_remote: RefCell::new(HashMap::new()),
      by_transaction: RefCell::new(HashMap::new()),
      cancel,
      closed: Cell::new(false),
    });
    for endpoint in &network.shared {
      spawn_shared_reader(Rc::downgrade(&network), endpoint.clone());
    }
    Ok(network)
  }

  pub fn is_shared(&self) -> bool {
    self.options.port.is_some()
  }

  pub fn is_closed(&self) -> bool {
    self.closed.get()
  }

  pub fn addresses(&self) -> Vec<String> {
    self.shared.iter().map(|e| e.bound.to_string()).collect()
  }

  /// A connection on the shared port answers to this local ufrag.
  pub fn register_ufrag(&self, ufrag: String, peer: &Weak<Peer>) {
    let mut map = self.by_ufrag.borrow_mut();
    map.retain(|_, p| p.strong_count() > 0);
    map.insert(ufrag, peer.clone());
  }

  /// A STUN request a connection sends to a STUN server on the shared port:
  /// its response is routed back by transaction id.
  pub fn register_transaction(&self, message: &[u8], peer: &Weak<Peer>) {
    if let Some((_, tid, _)) = stun_info(message) {
      let mut map = self.by_transaction.borrow_mut();
      if map.len() >= MAX_ROUTES {
        map.retain(|_, p| p.strong_count() > 0);
      }
      if map.len() < MAX_ROUTES {
        map.insert(tid, peer.clone());
      }
    }
  }

  pub fn forget(&self, peer: &Peer) {
    let same = |p: &Weak<Peer>| std::ptr::eq(p.as_ptr(), peer);
    self.by_ufrag.borrow_mut().retain(|_, p| !same(p));
    self.by_remote.borrow_mut().retain(|_, p| !same(p));
    self.by_transaction.borrow_mut().retain(|_, p| !same(p));
  }

  pub fn close(&self) {
    if self.closed.replace(true) {
      return;
    }
    self.cancel.cancel();
    self.by_ufrag.borrow_mut().clear();
    self.by_remote.borrow_mut().clear();
    self.by_transaction.borrow_mut().clear();
  }

  /// The connection a datagram on the shared port belongs to. A binding
  /// request goes by the ufrag its USERNAME names and (re)maps its source
  /// address to that connection, so a request forged from someone's address
  /// can't keep their traffic away from the connection they are checking; a
  /// STUN server's response goes by transaction id; anything else by the
  /// source address.
  fn route(&self, from: SocketAddr, data: &[u8]) -> Option<Rc<Peer>> {
    if let Some((typ, tid, ufrag)) = stun_info(data) {
      if typ & 0x0110 != 0 {
        if let Some(peer) = self
          .by_transaction
          .borrow_mut()
          .remove(&tid)
          .and_then(|p| p.upgrade())
        {
          return Some(peer);
        }
      } else if let Some(ufrag) = ufrag {
        let peer =
          self.by_ufrag.borrow().get(&ufrag).and_then(Weak::upgrade)?;
        // ICE authenticates the request itself; a forged one only moves a
        // route, which the real peer's next check moves back.
        let mut routes = self.by_remote.borrow_mut();
        if routes.len() >= MAX_ROUTES {
          routes.retain(|_, p| p.strong_count() > 0);
        }
        if routes.len() < MAX_ROUTES {
          routes.insert(from, Rc::downgrade(&peer));
        }
        return Some(peer);
      }
    }
    self.by_remote.borrow().get(&from).and_then(Weak::upgrade)
  }
}

impl Drop for Network {
  fn drop(&mut self) {
    self.cancel.cancel();
  }
}

fn spawn_shared_reader(network: Weak<Network>, endpoint: SharedEndpoint) {
  let Some(cancel) = network.upgrade().map(|n| n.cancel.clone()) else {
    return;
  };
  deno_core::unsync::spawn(
    async move {
      let mut buf = vec![0u8; 9216];
      loop {
        match endpoint.socket.recv_from(&mut buf).await {
          Ok((n, from)) => {
            let Some(net) = network.upgrade() else {
              break;
            };
            let Some(peer) = net.route(from, &buf[..n]) else {
              continue;
            };
            peer.deliver(Datagram {
              local: endpoint.bound,
              from,
              stream: None,
              data: BytesMut::from(&buf[..n]),
              closed: None,
            });
          }
          Err(e) => {
            log::debug!("webrtc: shared recv on {}: {e}", endpoint.bound);
            tokio::time::sleep(Duration::from_millis(1)).await;
          }
        }
      }
    }
    .or_cancel(cancel),
  );
}

/// The addresses a connection (or a shared port) binds, each with the host
/// candidate addresses it stands for; and why some were left out (reported
/// as `icecandidateerror`s).
pub type BindPlan = (Vec<(IpAddr, Vec<IpAddr>)>, Vec<String>);

/// What to bind, from the network's options (or the defaults, without one):
/// the wildcard with every interface address as a host candidate when
/// interfaces may be listed and the wildcard listened on, else
/// `127.0.0.1`; `DENO_WEBRTC_LOOPBACK_ONLY=1` always `127.0.0.1`.
pub fn bind_plan(
  options: &NetworkOptions,
  guard: &dyn NetGuard,
) -> Result<BindPlan, WebRtcError> {
  let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
  let mut notes = Vec::new();
  let mut plan: Vec<(IpAddr, Vec<IpAddr>)> = Vec::new();
  if crate::peer::loopback_only() {
    plan.push((loopback, vec![loopback]));
    return Ok((plan, notes));
  }
  let interfaces = |notes: &mut Vec<String>| -> Option<Vec<IpAddr>> {
    match guard.interfaces() {
      Ok(()) => Some(crate::peer::interface_addresses(
        options.interfaces.as_deref(),
      )),
      Err(message) => {
        notes.push(format!(
          "Only loopback host candidates were gathered: {message}"
        ));
        None
      }
    }
  };
  let binds = options.bind_addresses.clone();
  match binds {
    Some(binds) => {
      let mut ifaces: Option<Option<Vec<IpAddr>>> = None;
      for bind in binds {
        if let Err(message) = guard.listen(bind) {
          if options.port.is_some() {
            return Err(WebRtcError::Permission(message));
          }
          notes.push(message);
          continue;
        }
        let hosts = if bind.is_unspecified() {
          let list =
            ifaces.get_or_insert_with(|| interfaces(&mut notes)).clone();
          let mut hosts: Vec<IpAddr> = list
            .unwrap_or_default()
            .into_iter()
            .filter(|ip| ip.is_ipv4() == bind.is_ipv4())
            .collect();
          if bind.is_ipv4() && options.include_loopback {
            hosts.insert(0, loopback);
          }
          hosts
        } else {
          vec![bind]
        };
        plan.push((bind, hosts));
      }
    }
    None => match interfaces(&mut notes) {
      Some(ifaces) => {
        let mut v4: Vec<IpAddr> =
          ifaces.iter().copied().filter(IpAddr::is_ipv4).collect();
        if options.include_loopback {
          v4.insert(0, loopback);
        }
        let v6: Vec<IpAddr> =
          ifaces.iter().copied().filter(IpAddr::is_ipv6).collect();
        let any4 = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
        match guard.listen(any4) {
          Ok(()) => plan.push((any4, v4)),
          Err(message) => {
            notes.push(format!(
              "Only loopback host candidates were gathered: {message}"
            ));
            plan.push((loopback, vec![loopback]));
          }
        }
        let any6 = IpAddr::V6(Ipv6Addr::UNSPECIFIED);
        if !v6.is_empty() && guard.listen(any6).is_ok() {
          plan.push((any6, v6));
        }
      }
      None => plan.push((loopback, vec![loopback])),
    },
  }
  // The loopback bind of the fallback needs its own listen grant.
  let mut checked = Vec::new();
  for (bind, hosts) in plan {
    if !bind.is_unspecified()
      && let Err(message) = guard.listen(bind)
    {
      if options.port.is_some() {
        return Err(WebRtcError::Permission(message));
      }
      notes.push(message);
      continue;
    }
    checked.push((bind, hosts));
  }
  Ok((checked, notes))
}

/// Binds a UDP socket on `ip` with a port from `range` (tried from a random
/// start, so concurrent connections spread over it).
pub fn bind_in_range(
  ip: IpAddr,
  range: (u16, u16),
) -> std::io::Result<UdpSocket> {
  let (min, max) = range;
  let span = (max - min) as u32 + 1;
  let mut start = [0u8; 4];
  let _ = aws_lc_rs::rand::fill(&mut start);
  let start = u32::from_le_bytes(start) % span;
  let mut last = None;
  for i in 0..span {
    let port = min + ((start + i) % span) as u16;
    match crate::peer::bind_udp(SocketAddr::new(ip, port)) {
      Ok(socket) => return Ok(socket),
      Err(e) => last = Some(e),
    }
  }
  Err(last.unwrap_or_else(|| {
    std::io::Error::new(std::io::ErrorKind::AddrInUse, "no free port in range")
  }))
}

#[cfg(test)]
pub(crate) fn stun_info_for_tests(
  data: &[u8],
) -> Option<(u16, [u8; 12], Option<String>)> {
  stun_info(data)
}
