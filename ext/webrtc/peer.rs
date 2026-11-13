// Copyright 2018-2026 the Deno authors. MIT license.

//! One `RTCPeerConnection`: webrtc-rs's sans-I/O `rtc` engine plus the I/O
//! that engine leaves to its host, owned here.
//!
//! `rtc` runs ICE, DTLS, SCTP and the data channels as state machines: it
//! never opens a socket, arms a timer or reads the clock. This module gives
//! each peer connection
//!
//! - one UDP socket per address family. With interface enumeration and a
//!   listen grant for the wildcard address it is bound to `0.0.0.0` (and
//!   `[::]` when the machine has a usable IPv6 address), and every interface
//!   address becomes a host candidate on that port; otherwise it is bound to
//!   `127.0.0.1` alone. The engine matches a datagram to a local candidate by
//!   its exact local address, which a wildcard socket doesn't report, so the
//!   local address of a datagram from a peer is the address the OS routes to
//!   that peer from (a connected probe socket, cached per peer);
//! - one sans-I/O STUN/TURN client per ICE server URL: over the UDP socket,
//!   or over its own TCP or TLS connection for `?transport=tcp` and `turns:`
//!   (RFC 6544 framing: STUN messages and padded ChannelData on the stream);
//! - a driver task that moves datagrams between the sockets, those clients
//!   and the engine, fires the timers and queues the engine's events for
//!   JavaScript.
//!
//! Guards the engine doesn't provide:
//!
//! - **Net permission gate** ([`NetGuard`]): every datagram is sent only to
//!   an allowed address (checked once per address); a refused one is never
//!   sent to and is reported as an `icecandidateerror`.
//! - **Selected-pair filter**: the engine doesn't check where a non-STUN
//!   packet (DTLS, SCTP inside it, RTP) comes from, so only those from the
//!   selected candidate pair reach it (see [`Core::is_from_selected_pair`]); STUN is
//!   authenticated by ICE itself.
//! - **DTLS watchdog**: the engine logs and drops handshake errors, so a
//!   connection whose ICE is up but whose DTLS hasn't connected within
//!   [`DTLS_TIMEOUT`] is reported as `failed`.
//! - **Panic boundary**: a panic in the engine poisons the connection (it
//!   reports `failed` and closes) instead of unwinding into the runtime. In a
//!   `panic = "abort"` build (Deno's release profile) a panic still aborts.
//!
//! Everything runs on the isolate's thread: ops and the driver share the
//! [`Core`] through a `RefCell` and never hold it across an `.await`.

use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::Ipv6Addr;
use std::net::SocketAddr;
use std::panic::AssertUnwindSafe;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use bytes::BytesMut;
use deno_core::CancelFuture;
use deno_core::CancelHandle;
use deno_core::ToJsBuffer;
use rtc::data_channel::RTCDataChannelId;
use rtc::data_channel::RTCDataChannelInit;
use rtc::data_channel::RTCDataChannelState;
use rtc::ice::url::ProtoType;
use rtc::ice::url::SchemeType;
use rtc::ice::url::Url as IceUrl;
use rtc::peer_connection::RTCPeerConnection;
use rtc::peer_connection::RTCPeerConnectionBuilder;
use rtc::peer_connection::configuration::RTCConfigurationBuilder;
use rtc::peer_connection::configuration::setting_engine::SctpMaxMessageSize;
use rtc::peer_connection::configuration::setting_engine::SettingEngineBuilder;
use rtc::peer_connection::event::RTCDataChannelEvent;
use rtc::peer_connection::event::RTCPeerConnectionEvent;
use rtc::peer_connection::message::RTCMessage;
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::peer_connection::transport::CandidateConfig;
use rtc::peer_connection::transport::CandidateHostConfig;
use rtc::peer_connection::transport::CandidateRelayConfig;
use rtc::peer_connection::transport::CandidateServerReflexiveConfig;
use rtc::peer_connection::transport::RTCIceCandidate;
use rtc::peer_connection::transport::RTCIceCandidateInit;
use rtc::peer_connection::transport::RTCIceCandidateType;
use rtc::sansio::Protocol;
use rtc::shared::TaggedBytesMut;
use rtc::shared::TransportContext;
use rtc::shared::TransportProtocol;
use rtc::shared::error::Error as RtcError;
use rtc::statistics::StatsSelector;
use rtc::statistics::report::RTCStatsReportEntry;
use rtc::stun::message::TransactionId;
use rtc::turn::client::Client as TurnClient;
use rtc::turn::client::ClientConfig as TurnClientConfig;
use rtc::turn::client::Event as TurnEvent;
use serde::Serialize;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tokio::sync::mpsc;

use crate::WebRtcError;

/// The largest UDP payload read from a socket. ICE, DTLS and SCTP packets
/// stay under the path MTU; this leaves room for jumbo frames.
const RECV_BUFFER_SIZE: usize = 9216;
/// Datagrams queued between the socket readers and the driver. A full queue
/// drops new datagrams, as a full socket buffer would.
const RECV_QUEUE: usize = 4096;
/// How many times one driver pass loops over writes, server clients and
/// events before it waits again (each loop can produce more work, e.g. a
/// relayed datagram the engine answers).
const MAX_PUMP_ROUNDS: usize = 32;
/// `icecandidateerror` code for "no server response / not reachable", the
/// code browsers use for gathering failures without a STUN error code.
const ERROR_CODE_UNREACHABLE: u16 = 701;
/// How long DTLS may take once ICE is connected before the connection is
/// reported as failed (the handshake retransmits for up to ~60 s in the
/// engine, but a peer that connects ICE and never finishes DTLS is broken:
/// a wrong fingerprint, a certificate the engine refused).
pub const DTLS_TIMEOUT: Duration = Duration::from_secs(15);
/// Remote addresses remembered as having sent STUN.
const MAX_STUN_PEERS: usize = 256;
/// The largest frame read from a TCP/TLS TURN connection (a STUN message or
/// a ChannelData frame: 4 + 65535 bytes, padded).
const MAX_STREAM_FRAME: usize = 4 + 65536;

/// The net permission gate. Production code wraps the runtime's
/// `PermissionsContainer` (lib.rs); tests use their own guards.
pub trait NetGuard {
  /// May the connection send to `addr`? (`--allow-net` /
  /// `--allow-net-connect` for `ip:port`.)
  fn connect(&self, addr: SocketAddr) -> Result<(), String>;
  /// May the connection bind a UDP socket on `ip`? (`--allow-net` /
  /// `--allow-net-listen` for `ip`.)
  fn listen(&self, ip: IpAddr) -> Result<(), String>;
  /// May the connection list the machine's interface addresses (its host
  /// candidates)? (`--allow-sys=networkInterfaces`, as
  /// `Deno.networkInterfaces()`.)
  fn interfaces(&self) -> Result<(), String>;
}

/// How a STUN/TURN client reaches its server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerTransport {
  Udp,
  Tcp,
  Tls,
}

/// A STUN or TURN server URL from `RTCConfiguration.iceServers`, parsed and
/// permission-checked by the constructor.
#[derive(Clone, Debug)]
pub struct IceServerSpec {
  pub url: String,
  pub turn: bool,
  pub host: String,
  pub port: u16,
  pub username: String,
  pub credential: String,
  pub transport: ServerTransport,
}

impl IceServerSpec {
  /// Parses one URL of an `RTCIceServer`. `Err` is the W3C `SyntaxError`
  /// (bad URL) or `InvalidAccessError` (TURN without credentials) message.
  pub fn parse(
    url: &str,
    username: Option<&str>,
    credential: Option<&str>,
  ) -> Result<Self, WebRtcError> {
    let parsed = IceUrl::parse_url(url).map_err(|e| {
      WebRtcError::Syntax(format!("Invalid ICE server URL '{url}': {e}"))
    })?;
    let turn = matches!(parsed.scheme, SchemeType::Turn | SchemeType::Turns);
    if turn && (username.is_none() || credential.is_none()) {
      return Err(WebRtcError::InvalidAccess(format!(
        "TURN server '{url}' needs a username and a credential"
      )));
    }
    let transport = match (parsed.scheme, parsed.proto) {
      (SchemeType::Turns | SchemeType::Stuns, _) => ServerTransport::Tls,
      (_, ProtoType::Tcp) => ServerTransport::Tcp,
      _ => ServerTransport::Udp,
    };
    Ok(Self {
      url: url.to_string(),
      turn,
      host: parsed.host.trim_matches(['[', ']']).to_string(),
      port: parsed.port,
      username: username.unwrap_or_default().to_string(),
      credential: credential.unwrap_or_default().to_string(),
      transport,
    })
  }
}

/// Options for [`Peer::new`], from `RTCConfiguration`.
#[derive(Default)]
pub struct PeerOptions {
  pub ice_servers: Vec<IceServerSpec>,
  /// `iceTransportPolicy: "relay"`: only relay candidates are signalled.
  pub relay_only: bool,
  /// For `turns:` servers: the runtime's TLS client configuration (its root
  /// store, `--unsafely-ignore-certificate-errors`).
  pub tls: Option<Arc<rustls::ClientConfig>>,
}

/// An event for JavaScript, delivered in batches by
/// `op_webrtc_pc_next_events`. Data channels are named by their engine
/// handle (`channel`), which exists before the SCTP stream id does.
#[derive(Serialize, Debug)]
#[serde(tag = "type")]
pub enum JsEvent {
  #[serde(rename = "icecandidate", rename_all = "camelCase")]
  IceCandidate {
    candidate: String,
    sdp_mid: Option<String>,
    #[serde(rename = "sdpMLineIndex")]
    sdp_m_line_index: Option<u16>,
    username_fragment: Option<String>,
    url: Option<String>,
  },
  #[serde(rename = "icecandidateerror", rename_all = "camelCase")]
  IceCandidateError {
    address: Option<String>,
    port: Option<u16>,
    url: String,
    error_code: u16,
    error_text: String,
  },
  #[serde(rename = "signalingstatechange")]
  SignalingState { state: String },
  #[serde(rename = "icegatheringstatechange")]
  IceGatheringState { state: String },
  #[serde(rename = "iceconnectionstatechange")]
  IceConnectionState { state: String },
  #[serde(rename = "connectionstatechange")]
  ConnectionState { state: String },
  #[serde(rename = "negotiationneeded")]
  NegotiationNeeded,
  #[serde(rename = "dcopen", rename_all = "camelCase")]
  DataChannelOpen {
    channel: u32,
    id: Option<u16>,
    label: String,
    protocol: String,
    ordered: bool,
    max_packet_life_time: Option<u16>,
    max_retransmits: Option<u16>,
    negotiated: bool,
  },
  #[serde(rename = "dcclosing")]
  DataChannelClosing { channel: u32 },
  #[serde(rename = "dcclose")]
  DataChannelClose { channel: u32 },
  #[serde(rename = "dcerror")]
  DataChannelError { channel: u32 },
  /// The channel's buffered amount fell to its low threshold or below.
  #[serde(rename = "dclow")]
  DataChannelLow { channel: u32 },
  #[serde(rename = "dcmessage")]
  DataChannelMessage {
    channel: u32,
    text: bool,
    data: Payload,
  },
}

/// A received message's bytes, handed to V8 without a copy (as a
/// `ToJsBuffer`) when the event is serialized.
#[derive(Debug)]
pub struct Payload(RefCell<Option<Vec<u8>>>);

impl Payload {
  fn new(bytes: Vec<u8>) -> Self {
    Self(RefCell::new(Some(bytes)))
  }

  #[cfg(test)]
  pub fn bytes(&self) -> Vec<u8> {
    self.0.borrow().clone().unwrap_or_default()
  }
}

impl Serialize for Payload {
  fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
    let bytes = self.0.borrow_mut().take().unwrap_or_default();
    ToJsBuffer::from(bytes).serialize(s)
  }
}

/// A datagram one of the readers received. `stream` is the index of the
/// server client whose TCP/TLS connection carried it.
struct Datagram {
  local: SocketAddr,
  from: SocketAddr,
  stream: Option<usize>,
  data: BytesMut,
  /// A TCP/TLS link to a TURN server ended (`stream` names it): why.
  closed: Option<String>,
}

/// The connection's UDP socket for one address family.
struct UdpEndpoint {
  socket: Rc<UdpSocket>,
  /// What the socket is bound to; a wildcard address for a wildcard bind.
  bound: SocketAddr,
  /// The host candidate addresses on this socket's port.
  hosts: Vec<IpAddr>,
}

enum ServerLink {
  Udp,
  /// A TCP or TLS connection: frames for the writer task.
  Stream(mpsc::UnboundedSender<Vec<u8>>),
}

/// One STUN/TURN client, for one ICE server URL.
struct ServerClient {
  url: String,
  client: TurnClient,
  /// The client's local address: a host candidate address on the UDP
  /// socket, or the stream's local address.
  base: SocketAddr,
  server: SocketAddr,
  link: ServerLink,
  binding: Option<TransactionId>,
  allocate: Option<TransactionId>,
  relay: Option<SocketAddr>,
}

impl ServerClient {
  fn pending(&self) -> bool {
    self.binding.is_some() || self.allocate.is_some()
  }

  fn protocol(&self) -> TransportProtocol {
    match self.link {
      ServerLink::Udp => TransportProtocol::UDP,
      ServerLink::Stream(_) => TransportProtocol::TCP,
    }
  }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GatherPhase {
  NotStarted,
  Running,
  Complete,
}

/// The engine and the I/O state around it, shared by ops and the driver.
pub struct Core {
  pub pc: RTCPeerConnection,
  options: PeerOptions,
  endpoints: Vec<UdpEndpoint>,
  servers: Vec<ServerClient>,
  gathering: GatherPhase,
  /// ICE server URLs still being resolved / connected before their client
  /// exists.
  resolving: usize,
  srflx_seen: HashSet<SocketAddr>,
  pub(crate) events: VecDeque<JsEvent>,
  allowed: HashSet<SocketAddr>,
  refused: HashSet<SocketAddr>,
  /// Peer IP -> the local IP the OS routes to it from (wildcard sockets).
  routes: HashMap<IpAddr, IpAddr>,
  /// The selected candidate pair as (local, remote): where the engine's
  /// last non-STUN datagram went.
  selected: Option<(SocketAddr, SocketAddr)>,
  /// Remote addresses that sent STUN (see [`Core::is_from_selected_pair`]).
  stun_peers: HashSet<SocketAddr>,
  /// ICE is connected or completed.
  ice_up: bool,
  /// When ICE last reached connected/completed while DTLS hadn't.
  ice_up_at: Option<Instant>,
  dtls_connected: bool,
  /// A synthetic `failed` was reported (watchdog or panic): later engine
  /// connection states other than `closed` are not reported.
  failed: bool,
  recv_tx: mpsc::Sender<Datagram>,
  recv_rx: Option<mpsc::Receiver<Datagram>>,
}

pub struct Peer {
  pub core: RefCell<Core>,
  guard: Rc<dyn NetGuard>,
  /// Ops -> driver: there is something to flush.
  wake: Notify,
  /// Driver -> `op_webrtc_pc_next_events`: `core.events` is not empty.
  events_ready: Notify,
  cancel: Rc<CancelHandle>,
  closed: Cell<bool>,
  driver_started: Cell<bool>,
}

fn rtc_err(e: impl std::fmt::Display) -> WebRtcError {
  WebRtcError::Operation(e.to_string())
}

/// The candidate's transport, address and port, from a candidate attribute
/// value (`candidate:<foundation> <component> <transport> <priority>
/// <address> <port> typ ...`).
pub fn candidate_address(candidate: &str) -> Option<(String, String, u16)> {
  let value = candidate
    .trim()
    .strip_prefix("a=")
    .unwrap_or(candidate.trim());
  let value = value.strip_prefix("candidate:").unwrap_or(value);
  let mut fields = value.split_ascii_whitespace();
  let _foundation = fields.next()?;
  let _component = fields.next()?;
  let transport = fields.next()?.to_ascii_lowercase();
  let _priority = fields.next()?;
  let address = fields.next()?.to_string();
  let port = fields.next()?.parse().ok()?;
  Some((transport, address, port))
}

/// What to do with a remote candidate before the engine sees it.
pub enum RemoteCandidate {
  /// A UDP candidate with a literal address: check it, then add it.
  Udp(SocketAddr),
  /// Skipped and reported as accepted, as a browser does with a candidate it
  /// cannot use: TCP candidates (this agent does UDP only) and mDNS `.local`
  /// names (no mDNS resolution; such a peer is still reached as a
  /// peer-reflexive candidate once its checks arrive).
  Ignore,
}

pub fn classify_remote_candidate(
  candidate: &str,
) -> Result<RemoteCandidate, WebRtcError> {
  let Some((transport, address, port)) = candidate_address(candidate) else {
    return Err(WebRtcError::Operation(format!(
      "Invalid ICE candidate '{candidate}'"
    )));
  };
  if transport != "udp" {
    return Ok(RemoteCandidate::Ignore);
  }
  match address.trim_matches(['[', ']']).parse::<IpAddr>() {
    Ok(ip) => Ok(RemoteCandidate::Udp(SocketAddr::new(ip, port))),
    Err(_) => Ok(RemoteCandidate::Ignore),
  }
}

/// `DENO_WEBRTC_LOOPBACK_ONLY=1`: gather on `127.0.0.1` alone, never binding
/// the wildcard address (test machines whose firewall asks about every
/// non-loopback listener; two peers on one machine still connect). Read by
/// the runtime like its other `DENO_*` switches, without `--allow-env`.
fn loopback_only() -> bool {
  std::env::var_os("DENO_WEBRTC_LOOPBACK_ONLY").is_some_and(|v| v == "1")
}

/// The machine's usable interface addresses (no loopback, no link-local).
fn interface_addresses() -> Vec<IpAddr> {
  let Ok(ifaces) = netif::up() else {
    return Vec::new();
  };
  let mut out = Vec::new();
  for iface in ifaces {
    let ip = *iface.address();
    let usable = match ip {
      IpAddr::V4(v4) => {
        !v4.is_loopback() && !v4.is_link_local() && !v4.is_unspecified()
      }
      IpAddr::V6(v6) => {
        // Link-local addresses need a scope id the candidate grammar can't
        // carry; IPv4-mapped ones duplicate the IPv4 candidate.
        !v6.is_loopback()
          && !v6.is_unspecified()
          && (v6.segments()[0] & 0xffc0) != 0xfe80
          && v6.to_ipv4_mapped().is_none()
      }
    };
    if usable && !out.contains(&ip) {
      out.push(ip);
    }
  }
  out
}

/// Binds a UDP socket (an IPv6 one IPv6-only, so the two families stay
/// apart).
fn bind_udp(addr: SocketAddr) -> std::io::Result<UdpSocket> {
  let domain = if addr.is_ipv4() {
    socket2::Domain::IPV4
  } else {
    socket2::Domain::IPV6
  };
  let socket = socket2::Socket::new(
    domain,
    socket2::Type::DGRAM,
    Some(socket2::Protocol::UDP),
  )?;
  if addr.is_ipv6() {
    socket.set_only_v6(true)?;
  }
  socket.set_nonblocking(true)?;
  socket.bind(&addr.into())?;
  UdpSocket::from_std(socket.into())
}

/// The local IP the OS sends to `peer` from: a connected UDP probe (no
/// datagram leaves the machine).
fn route_to(peer: IpAddr) -> Option<IpAddr> {
  let unspecified: IpAddr = if peer.is_ipv4() {
    Ipv4Addr::UNSPECIFIED.into()
  } else {
    Ipv6Addr::UNSPECIFIED.into()
  };
  let probe = std::net::UdpSocket::bind((unspecified, 0)).ok()?;
  probe.connect((peer, 9)).ok()?;
  Some(probe.local_addr().ok()?.ip())
}

/// RFC 7983 demultiplexing: a STUN message starts with 0-3.
fn is_stun(data: &[u8]) -> bool {
  data.first().is_some_and(|b| *b < 4)
}

fn candidate_line(
  candidate: rtc::shared::error::Result<rtc::ice::candidate::Candidate>,
) -> Result<String, WebRtcError> {
  let candidate = candidate.map_err(rtc_err)?;
  Ok(
    RTCIceCandidate::from(&candidate)
      .to_json()
      .map_err(rtc_err)?
      .candidate,
  )
}

fn base_config(addr: SocketAddr) -> CandidateConfig {
  CandidateConfig {
    network: "udp".to_owned(),
    address: addr.ip().to_string(),
    port: addr.port(),
    component: 1,
    ..Default::default()
  }
}

/// The length of the first complete RFC 6544 frame in `buf` (a STUN message
/// or a padded ChannelData message), `Some(0)` for an incomplete one, `None`
/// for garbage.
fn stream_frame_len(buf: &[u8]) -> Option<usize> {
  if buf.len() < 4 {
    return Some(0);
  }
  let len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
  let total = match buf[0] >> 6 {
    0 => 20 + len,
    1 => 4 + len.div_ceil(4) * 4,
    _ => return None,
  };
  if total > MAX_STREAM_FRAME {
    return None;
  }
  Some(if buf.len() >= total { total } else { 0 })
}

impl Core {
  /// The local description's first media section id, for candidates the
  /// engine reports without one (W3C: `sdpMid` of the bundled section).
  fn first_mid(&self) -> Option<String> {
    let desc = self.pc.local_description()?;
    desc.sdp.lines().find_map(|l| {
      l.trim_end()
        .strip_prefix("a=mid:")
        .map(|mid| mid.trim().to_string())
    })
  }

  fn local_ufrag(&self) -> Option<String> {
    let desc = self.pc.local_description()?;
    desc.sdp.lines().find_map(|l| {
      l.trim_end()
        .strip_prefix("a=ice-ufrag:")
        .map(|u| u.trim().to_string())
    })
  }

  fn add_local(
    &mut self,
    candidate: String,
    url: Option<String>,
  ) -> Result<(), WebRtcError> {
    self
      .pc
      .add_local_candidate(RTCIceCandidateInit {
        candidate,
        url,
        ..Default::default()
      })
      .map_err(rtc_err)
  }

  fn report_error(
    &mut self,
    url: &str,
    base: Option<SocketAddr>,
    error_code: u16,
    error_text: String,
  ) {
    self.events.push_back(JsEvent::IceCandidateError {
      address: base.map(|b| b.ip().to_string()),
      port: base.map(|b| b.port()),
      url: url.to_string(),
      error_code,
      error_text,
    });
  }

  /// Reports `failed` on the engine's behalf (watchdog, panic).
  fn fail(&mut self, why: &str) {
    if self.failed {
      return;
    }
    log::debug!("webrtc: connection failed: {why}");
    self.failed = true;
    self.events.push_back(JsEvent::IceConnectionState {
      state: "failed".to_string(),
    });
    self.events.push_back(JsEvent::ConnectionState {
      state: "failed".to_string(),
    });
  }

  /// The send-side permission gate: once per destination address.
  fn permit(&mut self, guard: &dyn NetGuard, addr: SocketAddr) -> bool {
    if self.allowed.contains(&addr) {
      return true;
    }
    if self.refused.contains(&addr) {
      return false;
    }
    match guard.connect(addr) {
      Ok(()) => {
        self.allowed.insert(addr);
        true
      }
      Err(message) => {
        self.refused.insert(addr);
        self.events.push_back(JsEvent::IceCandidateError {
          address: Some(addr.ip().to_string()),
          port: Some(addr.port()),
          url: String::new(),
          error_code: ERROR_CODE_UNREACHABLE,
          error_text: message,
        });
        false
      }
    }
  }

  /// Marks a remote address as already checked (by `addIceCandidate` /
  /// `setRemoteDescription`, which reject when it is refused).
  pub(crate) fn mark_allowed(&mut self, addr: SocketAddr) {
    self.allowed.insert(addr);
  }

  /// The UDP socket a datagram from `local` (a host candidate address or a
  /// base) goes out on.
  fn endpoint_for(&self, local: SocketAddr) -> Option<Rc<UdpSocket>> {
    self
      .endpoints
      .iter()
      .find(|e| {
        e.bound.port() == local.port() && e.bound.is_ipv4() == local.is_ipv4()
      })
      .map(|e| e.socket.clone())
  }

  /// The local address a datagram on `endpoint` from `from` arrived on.
  fn local_for(&mut self, endpoint: usize, from: SocketAddr) -> SocketAddr {
    let e = &self.endpoints[endpoint];
    if !e.bound.ip().is_unspecified() {
      return e.bound;
    }
    let port = e.bound.port();
    let hosts = e.hosts.clone();
    let ip = *self.routes.entry(from.ip()).or_insert_with(|| {
      route_to(from.ip())
        .filter(|ip| hosts.contains(ip))
        .or_else(|| hosts.first().copied())
        .unwrap_or(from.ip())
    });
    SocketAddr::new(ip, port)
  }

  /// Whether a non-STUN packet on (`local`, `from`) belongs to the selected
  /// pair. The engine sends DTLS (and everything inside it) only on the
  /// selected pair, so the pair is the one its last non-STUN datagram went
  /// out on. Until it has sent one (the DTLS server waiting for the client's
  /// first flight), a packet is accepted only while ICE is connected and only
  /// from an address ICE has exchanged STUN with. The local side is compared
  /// by family and port (exactly for a relay): a wildcard socket's local
  /// address is the routed one.
  fn is_from_selected_pair(
    &self,
    local: SocketAddr,
    from: SocketAddr,
    relayed: bool,
  ) -> bool {
    match self.selected {
      Some((l, r)) => {
        r == from
          && if relayed {
            l == local
          } else {
            l.port() == local.port() && l.is_ipv4() == local.is_ipv4()
          }
      }
      None => self.ice_up && self.stun_peers.contains(&from),
    }
  }

  /// Remembers a remote address that sent STUN (bounded: a flood of
  /// spoofed sources can't grow it without limit).
  fn saw_stun(&mut self, from: SocketAddr) {
    if self.stun_peers.len() < MAX_STUN_PEERS {
      self.stun_peers.insert(from);
    }
  }

  /// Sends what the engine wants sent, through the socket or a TURN relay.
  fn flush_engine_writes(&mut self, guard: &dyn NetGuard) -> bool {
    let mut progressed = false;
    while let Some(msg) = self.pc.poll_write() {
      progressed = true;
      let local = msg.transport.local_addr;
      let to = msg.transport.peer_addr;
      if !self.permit(guard, to) {
        continue;
      }
      if !is_stun(&msg.message) {
        self.selected = Some((local, to));
      }
      if let Some(server) =
        self.servers.iter_mut().find(|s| s.relay == Some(local))
      {
        let now = Instant::now();
        let sent = server
          .client
          .relay(local)
          .and_then(|mut relay| relay.send_to(now, &msg.message, to));
        if sent.is_err() {
          // No permission for this peer on the relay yet: ask for it and
          // drop the datagram (ICE retransmits its checks).
          if let Ok(mut relay) = server.client.relay(local) {
            let _ = relay.create_permission(now, to);
          }
        }
        continue;
      }
      if let Some(socket) = self.endpoint_for(local)
        && let Err(e) = socket.try_send_to(&msg.message, to)
      {
        log::debug!("webrtc: send {local} -> {to}: {e}");
      }
    }
    progressed
  }

  /// Runs the STUN/TURN clients: their datagrams out, their results in.
  fn service_servers(&mut self, now: Instant) -> bool {
    let mut progressed = false;
    let mut found: Vec<(String, SocketAddr, SocketAddr)> = Vec::new();
    let mut relays: Vec<(String, SocketAddr, SocketAddr)> = Vec::new();
    let mut errors: Vec<(String, SocketAddr, String)> = Vec::new();
    let mut relayed_in: Vec<(SocketAddr, SocketAddr, BytesMut)> = Vec::new();
    for server in &mut self.servers {
      while let Some(tx) = server.client.poll_write() {
        progressed = true;
        match &server.link {
          ServerLink::Stream(writer) => {
            let _ = writer.send(tx.message.to_vec());
          }
          ServerLink::Udp => {
            if let Some(e) = self.endpoints.iter().find(|e| {
              e.bound.port() == tx.transport.local_addr.port()
                && e.bound.is_ipv4() == tx.transport.local_addr.is_ipv4()
            }) && let Err(err) =
              e.socket.try_send_to(&tx.message, tx.transport.peer_addr)
            {
              log::debug!("webrtc: STUN/TURN send: {err}");
            }
          }
        }
      }
      while let Some(event) = server.client.poll_event() {
        progressed = true;
        let url = server.url.clone();
        match event {
          TurnEvent::BindingResponse(tid, addr) => {
            if server.binding == Some(tid) {
              server.binding = None;
              if matches!(server.link, ServerLink::Udp) {
                found.push((url, server.base, addr));
              }
            }
          }
          TurnEvent::BindingError(tid, e) => {
            if server.binding == Some(tid) {
              server.binding = None;
              errors.push((url, server.base, e.to_string()));
            }
          }
          TurnEvent::AllocateResponse(tid, relay) => {
            if server.allocate == Some(tid) {
              server.allocate = None;
              server.relay = Some(relay);
              relays.push((url, server.base, relay));
            }
          }
          TurnEvent::AllocateError(tid, e) => {
            if server.allocate == Some(tid) {
              server.allocate = None;
              errors.push((url, server.base, e.to_string()));
            }
          }
          TurnEvent::TransactionTimeout(tid) => {
            if server.binding == Some(tid) {
              server.binding = None;
              errors.push((
                url,
                server.base,
                "STUN binding request timed out".to_string(),
              ));
            } else if server.allocate == Some(tid) {
              server.allocate = None;
              errors.push((
                url,
                server.base,
                "TURN allocate request timed out".to_string(),
              ));
            }
          }
          TurnEvent::DataIndicationOrChannelData(_, from, data) => {
            if let Some(relay) = server.relay {
              relayed_in.push((relay, from, data));
            }
          }
          _ => {}
        }
      }
    }
    for (url, base, addr) in found {
      let host = self
        .endpoints
        .iter()
        .any(|e| e.bound.port() == addr.port() && e.hosts.contains(&addr.ip()));
      if host || self.options.relay_only || !self.srflx_seen.insert(addr) {
        continue;
      }
      let line = candidate_line(
        CandidateServerReflexiveConfig {
          base_config: base_config(addr),
          rel_addr: base.ip().to_string(),
          rel_port: base.port(),
          url: Some(url.clone()),
        }
        .new_candidate_server_reflexive(),
      );
      if let Err(e) =
        line.and_then(|line| self.add_local(line, Some(url.clone())))
      {
        self.report_error(
          &url,
          Some(base),
          ERROR_CODE_UNREACHABLE,
          e.to_string(),
        );
      }
    }
    for (url, base, relay) in relays {
      let line = candidate_line(
        CandidateRelayConfig {
          base_config: base_config(relay),
          rel_addr: base.ip().to_string(),
          rel_port: base.port(),
          url: Some(url.clone()),
        }
        .new_candidate_relay(),
      );
      if let Err(e) =
        line.and_then(|line| self.add_local(line, Some(url.clone())))
      {
        self.report_error(
          &url,
          Some(base),
          ERROR_CODE_UNREACHABLE,
          e.to_string(),
        );
      }
    }
    for (url, base, text) in errors {
      self.report_error(&url, Some(base), ERROR_CODE_UNREACHABLE, text);
    }
    for (relay, from, data) in relayed_in {
      if self.refused.contains(&from) {
        continue;
      }
      if is_stun(&data) {
        self.saw_stun(from);
      } else if !self.is_from_selected_pair(relay, from, true) {
        continue;
      }
      let _ = self.pc.handle_read(TaggedBytesMut {
        now,
        transport: TransportContext {
          local_addr: relay,
          peer_addr: from,
          ecn: None,
          transport_protocol: TransportProtocol::UDP,
        },
        message: data,
      });
    }
    progressed
  }

  /// Moves the engine's events and received messages to the JS queue.
  pub(crate) fn collect_events(&mut self, now: Instant) {
    let mut mid = None::<String>;
    while let Some(event) = self.pc.poll_event() {
      match event {
        RTCPeerConnectionEvent::OnNegotiationNeededEvent => {
          self.events.push_back(JsEvent::NegotiationNeeded)
        }
        RTCPeerConnectionEvent::OnIceCandidateEvent(e) => {
          if self.options.relay_only
            && e.candidate.typ != RTCIceCandidateType::Relay
          {
            continue;
          }
          let Ok(init) = e.candidate.to_json() else {
            continue;
          };
          if mid.is_none() {
            mid = Some(self.first_mid().unwrap_or_else(|| "0".to_string()));
          }
          let ufrag = self.local_ufrag();
          self.events.push_back(JsEvent::IceCandidate {
            candidate: init.candidate,
            sdp_mid: mid.clone(),
            sdp_m_line_index: Some(0),
            username_fragment: ufrag,
            url: if e.url.is_empty() { None } else { Some(e.url) },
          });
        }
        RTCPeerConnectionEvent::OnIceCandidateErrorEvent(e) => {
          self.events.push_back(JsEvent::IceCandidateError {
            address: Some(e.address.clone()).filter(|a| !a.is_empty()),
            port: Some(e.port).filter(|p| *p != 0),
            url: e.url.clone(),
            error_code: e.error_code,
            error_text: e.error_text.clone(),
          })
        }
        RTCPeerConnectionEvent::OnSignalingStateChangeEvent(s) => {
          self.events.push_back(JsEvent::SignalingState {
            state: s.to_string(),
          })
        }
        RTCPeerConnectionEvent::OnIceConnectionStateChangeEvent(s) => {
          let state = s.to_string();
          if state == "connected" || state == "completed" {
            self.ice_up = true;
            if !self.dtls_connected && self.ice_up_at.is_none() {
              self.ice_up_at = Some(now);
            }
          } else {
            self.ice_up = false;
            self.ice_up_at = None;
          }
          if !self.failed || state == "closed" {
            self.events.push_back(JsEvent::IceConnectionState { state });
          }
        }
        RTCPeerConnectionEvent::OnIceGatheringStateChangeEvent(s) => {
          self.events.push_back(JsEvent::IceGatheringState {
            state: s.to_string(),
          })
        }
        RTCPeerConnectionEvent::OnConnectionStateChangeEvent(s) => {
          let state = s.to_string();
          if state == "connected" {
            self.dtls_connected = true;
            self.ice_up_at = None;
          }
          if !self.failed || state == "closed" {
            self.events.push_back(JsEvent::ConnectionState { state });
          }
        }
        RTCPeerConnectionEvent::OnDataChannel(dc) => {
          let event = match dc {
            RTCDataChannelEvent::OnOpen(handle) => {
              let Some(channel) = self.pc.data_channel(handle) else {
                continue;
              };
              JsEvent::DataChannelOpen {
                channel: handle as u32,
                id: channel.stream_id(),
                label: channel.label().to_string(),
                protocol: channel.protocol().to_string(),
                ordered: channel.ordered(),
                max_packet_life_time: channel.max_packet_life_time(),
                max_retransmits: channel.max_retransmits(),
                negotiated: channel.negotiated(),
              }
            }
            RTCDataChannelEvent::OnClosing(handle) => {
              JsEvent::DataChannelClosing {
                channel: handle as u32,
              }
            }
            RTCDataChannelEvent::OnClose(handle) => JsEvent::DataChannelClose {
              channel: handle as u32,
            },
            RTCDataChannelEvent::OnError(handle) => JsEvent::DataChannelError {
              channel: handle as u32,
            },
            RTCDataChannelEvent::OnBufferedAmountLow(handle) => {
              JsEvent::DataChannelLow {
                channel: handle as u32,
              }
            }
            _ => continue,
          };
          self.events.push_back(event);
        }
        _ => {}
      }
    }
    while let Some(message) = self.pc.poll_read() {
      if let RTCMessage::DataChannelMessage(handle, message) = message.message {
        self.events.push_back(JsEvent::DataChannelMessage {
          channel: handle as u32,
          text: message.is_string,
          data: Payload::new(message.data.to_vec()),
        });
      }
    }
  }

  /// Ends gathering once every server answered or gave up.
  fn maybe_complete_gathering(&mut self) {
    if self.gathering != GatherPhase::Running
      || self.resolving > 0
      || self.servers.iter().any(ServerClient::pending)
    {
      return;
    }
    self.gathering = GatherPhase::Complete;
    // An empty candidate is the engine's end-of-candidates: it sets the
    // gathering state to "complete" and emits the state change.
    let _ = self.pc.add_local_candidate(RTCIceCandidateInit::default());
  }

  /// One driver pass: loop until the engine and the clients are idle.
  fn pump(&mut self, guard: &dyn NetGuard) {
    let now = Instant::now();
    if self
      .ice_up_at
      .is_some_and(|t| now.duration_since(t) >= DTLS_TIMEOUT)
    {
      self.ice_up_at = None;
      self.fail("DTLS did not connect after ICE did");
    }
    for _ in 0..MAX_PUMP_ROUNDS {
      let a = self.flush_engine_writes(guard);
      let b = self.service_servers(now);
      self.maybe_complete_gathering();
      self.collect_events(now);
      if !a && !b {
        break;
      }
    }
  }

  fn next_deadline(&mut self) -> Option<Instant> {
    let mut deadline = self.pc.poll_timeout();
    for server in &mut self.servers {
      if let Some(t) = server.client.poll_timeout() {
        deadline = Some(deadline.map_or(t, |d| d.min(t)));
      }
    }
    if let Some(up) = self.ice_up_at {
      let t = up + DTLS_TIMEOUT;
      deadline = Some(deadline.map_or(t, |d| d.min(t)));
    }
    deadline
  }

  fn handle_timeout(&mut self, now: Instant) {
    let _ = self.pc.handle_timeout(now);
    for server in &mut self.servers {
      let _ = server.client.handle_timeout(now);
    }
  }

  fn handle_datagram(&mut self, dg: Datagram) {
    if self.refused.contains(&dg.from) {
      return;
    }
    let now = Instant::now();
    if let (Some(index), Some(reason)) = (dg.stream, dg.closed.as_ref()) {
      if let Some(server) = self.servers.get_mut(index) {
        let (url, base) = (server.url.clone(), server.base);
        server.binding = None;
        server.allocate = None;
        server.relay = None;
        self.report_error(
          &url,
          Some(base),
          ERROR_CODE_UNREACHABLE,
          format!("The TURN connection ended: {reason}"),
        );
      }
      return;
    }
    if let Some(index) = dg.stream {
      if let Some(server) = self.servers.get_mut(index) {
        let protocol = server.protocol();
        let _ = server.client.handle_read(TaggedBytesMut {
          now,
          transport: TransportContext {
            local_addr: server.base,
            peer_addr: dg.from,
            ecn: None,
            transport_protocol: protocol,
          },
          message: dg.data,
        });
      }
      return;
    }
    // A reply from a STUN/TURN server on the UDP socket.
    if let Some(server) = self.servers.iter_mut().find(|s| {
      matches!(s.link, ServerLink::Udp)
        && s.server == dg.from
        && s.base.port() == dg.local.port()
    }) {
      let base = server.base;
      let _ = server.client.handle_read(TaggedBytesMut {
        now,
        transport: TransportContext {
          local_addr: base,
          peer_addr: dg.from,
          ecn: None,
          transport_protocol: TransportProtocol::UDP,
        },
        message: dg.data,
      });
      return;
    }
    let Some(endpoint) = self
      .endpoints
      .iter()
      .position(|e| e.bound.port() == dg.local.port())
    else {
      return;
    };
    let local = self.local_for(endpoint, dg.from);
    if is_stun(&dg.data) {
      self.saw_stun(dg.from);
    } else if !self.is_from_selected_pair(local, dg.from, false) {
      return;
    }
    let _ = self.pc.handle_read(TaggedBytesMut {
      now,
      transport: TransportContext {
        local_addr: local,
        peer_addr: dg.from,
        ecn: None,
        transport_protocol: TransportProtocol::UDP,
      },
      message: dg.data,
    });
  }
}

impl Peer {
  pub fn new(
    options: PeerOptions,
    guard: Rc<dyn NetGuard>,
  ) -> Result<Rc<Self>, WebRtcError> {
    let config = RTCConfigurationBuilder::new().build();
    // Messages up to 256 KiB, as browsers take (the engine's own default is
    // RFC 8841's 64 KiB); the connection's maxMessageSize is the smaller of
    // this and the peer's.
    let settings = SettingEngineBuilder::new()
      .with_sctp_max_message_size(SctpMaxMessageSize::Bounded(
        SctpMaxMessageSize::MAX_MESSAGE_SIZE,
      ))
      .build();
    let pc = RTCPeerConnectionBuilder::new()
      .with_configuration(config)
      .with_setting_engine(settings)
      .build(Instant::now())
      .map_err(rtc_err)?;
    let (recv_tx, recv_rx) = mpsc::channel(RECV_QUEUE);
    Ok(Rc::new(Self {
      core: RefCell::new(Core {
        pc,
        options,
        endpoints: Vec::new(),
        servers: Vec::new(),
        gathering: GatherPhase::NotStarted,
        resolving: 0,
        srflx_seen: HashSet::new(),
        events: VecDeque::new(),
        allowed: HashSet::new(),
        refused: HashSet::new(),
        routes: HashMap::new(),
        selected: None,
        stun_peers: HashSet::new(),
        ice_up: false,
        ice_up_at: None,
        dtls_connected: false,
        failed: false,
        recv_tx,
        recv_rx: Some(recv_rx),
      }),
      guard,
      wake: Notify::new(),
      events_ready: Notify::new(),
      cancel: CancelHandle::new_rc(),
      closed: Cell::new(false),
      driver_started: Cell::new(false),
    }))
  }

  pub fn is_closed(&self) -> bool {
    self.closed.get()
  }

  /// The panic boundary: runs `f` on the engine; a panic poisons the
  /// connection (`failed`, then closed) and becomes an `OperationError`.
  fn guarded<T>(
    &self,
    f: impl FnOnce(&mut Core) -> Result<T, WebRtcError>,
  ) -> Result<T, WebRtcError> {
    if self.closed.get() {
      return Err(WebRtcError::InvalidState(
        "RTCPeerConnection is closed".to_string(),
      ));
    }
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
      f(&mut self.core.borrow_mut())
    }));
    match result {
      Ok(r) => r,
      Err(_) => {
        self.poison();
        Err(WebRtcError::Operation(
          "the WebRTC engine failed; the connection is closed".to_string(),
        ))
      }
    }
  }

  fn poison(&self) {
    if let Ok(mut core) = self.core.try_borrow_mut() {
      core.fail("engine panic");
    }
    self.shutdown(false);
  }

  /// Ops call this after changing the engine: the driver flushes what the
  /// change produced and JavaScript gets the resulting events.
  pub fn poke(&self) {
    self.wake.notify_one();
  }

  /// The next batch of events, or `None` once the connection is closed and
  /// the queue is drained.
  pub async fn next_events(&self) -> Option<Vec<JsEvent>> {
    loop {
      {
        let mut core = self.core.borrow_mut();
        if !core.events.is_empty() {
          return Some(core.events.drain(..).collect());
        }
        if self.closed.get() {
          return None;
        }
      }
      self.events_ready.notified().await;
    }
  }

  pub fn create_offer(&self, ice_restart: bool) -> Result<String, WebRtcError> {
    self.guarded(|core| {
      if ice_restart {
        core.pc.restart_ice();
      }
      Ok(core.pc.create_offer(None).map_err(rtc_err)?.sdp)
    })
  }

  pub fn create_answer(&self) -> Result<String, WebRtcError> {
    self.guarded(|core| Ok(core.pc.create_answer(None).map_err(rtc_err)?.sdp))
  }

  fn description(
    kind: &str,
    sdp: String,
  ) -> Result<RTCSessionDescription, WebRtcError> {
    match kind {
      "offer" => RTCSessionDescription::offer(sdp),
      "answer" => RTCSessionDescription::answer(sdp),
      "pranswer" => RTCSessionDescription::pranswer(sdp),
      "rollback" => RTCSessionDescription::rollback(None),
      other => {
        return Err(WebRtcError::Type(format!(
          "'{other}' is not a valid RTCSdpType"
        )));
      }
    }
    .map_err(rtc_err)
  }

  pub fn set_local_description(
    self: &Rc<Self>,
    kind: &str,
    sdp: String,
  ) -> Result<(), WebRtcError> {
    let desc = Self::description(kind, sdp)?;
    self.guarded(|core| {
      core
        .pc
        .set_local_description(Instant::now(), desc)
        .map_err(rtc_err)
    })?;
    if kind != "rollback" {
      self.start_gathering();
    }
    self.ensure_driver();
    self.poke();
    Ok(())
  }

  /// Checks every candidate the remote description carries and hands the
  /// engine a copy without the ones it can't use (see
  /// [`RemoteCandidate::Ignore`]). A refused address fails the whole call.
  pub fn set_remote_description(
    self: &Rc<Self>,
    kind: &str,
    sdp: String,
  ) -> Result<(), WebRtcError> {
    let mut kept = String::with_capacity(sdp.len());
    let mut checked = Vec::new();
    for line in sdp.split_inclusive('\n') {
      if let Some(value) = line.trim_end().strip_prefix("a=candidate:") {
        match classify_remote_candidate(value)? {
          RemoteCandidate::Ignore => continue,
          RemoteCandidate::Udp(addr) => {
            self.guard.connect(addr).map_err(WebRtcError::Permission)?;
            checked.push(addr);
          }
        }
      }
      kept.push_str(line);
    }
    let desc = Self::description(kind, kept)?;
    self.guarded(|core| {
      core
        .pc
        .set_remote_description(Instant::now(), desc)
        .map_err(rtc_err)?;
      for addr in checked {
        core.mark_allowed(addr);
      }
      Ok(())
    })?;
    self.ensure_driver();
    self.poke();
    Ok(())
  }

  pub fn add_ice_candidate(
    &self,
    candidate: String,
    sdp_mid: Option<String>,
    sdp_m_line_index: Option<u16>,
    username_fragment: Option<String>,
  ) -> Result<(), WebRtcError> {
    if candidate.trim().is_empty() {
      // End-of-candidates: nothing to do for a UDP-only agent.
      return Ok(());
    }
    let addr = match classify_remote_candidate(&candidate)? {
      RemoteCandidate::Ignore => return Ok(()),
      RemoteCandidate::Udp(addr) => addr,
    };
    self.guard.connect(addr).map_err(WebRtcError::Permission)?;
    self.guarded(|core| {
      core
        .pc
        .add_remote_candidate(RTCIceCandidateInit {
          candidate,
          sdp_mid,
          sdp_mline_index: sdp_m_line_index,
          username_fragment,
          url: None,
        })
        .map_err(rtc_err)?;
      core.mark_allowed(addr);
      Ok(())
    })?;
    self.poke();
    Ok(())
  }

  /// Returns the channel's engine handle and its SCTP stream id when it
  /// already has one (a negotiated channel).
  pub fn create_data_channel(
    self: &Rc<Self>,
    label: &str,
    init: RTCDataChannelInit,
  ) -> Result<(u32, Option<u16>), WebRtcError> {
    let created = self.guarded(|core| {
      let channel = core
        .pc
        .create_data_channel(label, Some(init))
        .map_err(rtc_err)?;
      let handle = channel.id() as u32;
      let id = channel.stream_id();
      // A negotiated channel created on a live association is open at once
      // and the engine reports no open for it; JavaScript still gets its
      // `open` event (asynchronously, as the spec has it).
      if channel.ready_state() == RTCDataChannelState::Open {
        let open = JsEvent::DataChannelOpen {
          channel: handle,
          id,
          label: channel.label().to_string(),
          protocol: channel.protocol().to_string(),
          ordered: channel.ordered(),
          max_packet_life_time: channel.max_packet_life_time(),
          max_retransmits: channel.max_retransmits(),
          negotiated: channel.negotiated(),
        };
        core.events.push_back(open);
      }
      Ok((handle, id))
    })?;
    self.events_ready.notify_one();
    self.poke();
    Ok(created)
  }

  /// Sends one message and returns the channel's buffered amount after it.
  pub fn send(
    &self,
    handle: u32,
    text: bool,
    data: &[u8],
  ) -> Result<u32, WebRtcError> {
    let buffered = self.guarded(|core| {
      let Some(mut channel) = core.pc.data_channel(handle as RTCDataChannelId)
      else {
        return Err(WebRtcError::InvalidState(
          "RTCDataChannel is closed".to_string(),
        ));
      };
      let now = Instant::now();
      let sent = if text {
        let s = std::str::from_utf8(data)
          .map_err(|e| WebRtcError::Type(e.to_string()))?;
        channel.send_text(now, s)
      } else {
        channel.send(now, BytesMut::from(data))
      };
      match sent {
        Ok(()) => {}
        // W3C: a message over the SCTP transport's maxMessageSize is a
        // TypeError; a channel that isn't open an InvalidStateError.
        Err(RtcError::ErrOutboundPacketTooLarge) => {
          return Err(WebRtcError::Type(format!(
            "The message ({} bytes) is larger than the connection's maxMessageSize",
            data.len()
          )));
        }
        Err(
          e @ (RtcError::ErrDataChannelNotOpen | RtcError::ErrDataChannelClosed),
        ) => {
          return Err(WebRtcError::InvalidState(e.to_string()));
        }
        Err(e) => return Err(rtc_err(e)),
      }
      Ok(u32::try_from(channel.outstanding_bytes()).unwrap_or(u32::MAX))
    })?;
    self.poke();
    Ok(buffered)
  }

  /// `bufferedAmount`: bytes sent that SCTP hasn't released (acknowledged
  /// or abandoned).
  pub fn buffered_amount(&self, handle: u32) -> u32 {
    let mut core = self.core.borrow_mut();
    core
      .pc
      .data_channel(handle as RTCDataChannelId)
      .map(|c| u32::try_from(c.outstanding_bytes()).unwrap_or(u32::MAX))
      .unwrap_or(0)
  }

  pub fn set_buffered_amount_low_threshold(&self, handle: u32, threshold: u32) {
    let _ = self.guarded(|core| {
      if let Some(mut channel) =
        core.pc.data_channel(handle as RTCDataChannelId)
      {
        channel.set_buffered_amount_low_threshold(threshold);
      }
      Ok(())
    });
  }

  pub fn close_data_channel(&self, handle: u32) {
    let _ = self.guarded(|core| {
      if let Some(mut channel) =
        core.pc.data_channel(handle as RTCDataChannelId)
      {
        let _ = channel.close();
      }
      Ok(())
    });
    self.poke();
  }

  /// `RTCSctpTransport.maxMessageSize`, once SCTP is up.
  pub fn max_message_size(&self) -> Option<u32> {
    self
      .core
      .borrow()
      .pc
      .sctp()
      .and_then(|s| s.max_message_size())
  }

  pub fn local_description(&self) -> Option<(String, String)> {
    self
      .core
      .borrow()
      .pc
      .local_description()
      .map(|d| (d.sdp_type.to_string(), d.sdp))
  }

  pub fn remote_description(&self) -> Option<(String, String)> {
    self
      .core
      .borrow()
      .pc
      .remote_description()
      .map(|d| (d.sdp_type.to_string(), d.sdp.clone()))
  }

  pub fn stats(&self) -> Result<Vec<serde_json::Value>, WebRtcError> {
    self.guarded(|core| {
      let report = core.pc.get_stats(Instant::now(), StatsSelector::None);
      use RTCStatsReportEntry as E;
      Ok(
        report
          .iter()
          .filter_map(|entry| {
            match entry {
              E::PeerConnection(s) => serde_json::to_value(s),
              E::Transport(s) => serde_json::to_value(s),
              E::IceCandidatePair(s) => serde_json::to_value(s),
              E::LocalCandidate(s) => serde_json::to_value(s),
              E::RemoteCandidate(s) => serde_json::to_value(s),
              E::Certificate(s) => serde_json::to_value(s),
              E::Codec(s) => serde_json::to_value(s),
              E::DataChannel(s) => serde_json::to_value(s),
              E::InboundRtp(s) => serde_json::to_value(s),
              E::OutboundRtp(s) => serde_json::to_value(s),
              E::RemoteInboundRtp(s) => serde_json::to_value(s),
              E::RemoteOutboundRtp(s) => serde_json::to_value(s),
              E::AudioSource(s) => serde_json::to_value(s),
              E::VideoSource(s) => serde_json::to_value(s),
              E::AudioPlayout(s) => serde_json::to_value(s),
              _ => return None,
            }
            .ok()
          })
          .collect(),
      )
    })
  }

  /// `close()`: stops the engine, the sockets and the driver. Idempotent.
  pub fn close(&self) {
    self.shutdown(true);
  }

  fn shutdown(&self, graceful: bool) {
    if self.closed.replace(true) {
      return;
    }
    if graceful && let Ok(mut core) = self.core.try_borrow_mut() {
      let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let now = Instant::now();
        let _ = core.pc.close();
        core.collect_events(now);
        for server in &mut core.servers {
          if let Some(relay) = server.relay
            && let Ok(mut relay) = server.client.relay(relay)
          {
            let _ = relay.close(now);
          }
        }
        // A last flush so the peer hears the SCTP abort / DTLS close and a
        // TURN allocation is released, then the sockets go.
        let writes: Vec<_> =
          std::iter::from_fn(|| core.pc.poll_write()).collect();
        for msg in writes {
          if core.allowed.contains(&msg.transport.peer_addr)
            && core
              .servers
              .iter()
              .all(|s| s.relay != Some(msg.transport.local_addr))
            && let Some(socket) = core.endpoint_for(msg.transport.local_addr)
          {
            let _ = socket.try_send_to(&msg.message, msg.transport.peer_addr);
          }
        }
        let mut udp_writes = Vec::new();
        for server in &mut core.servers {
          while let Some(tx) = server.client.poll_write() {
            match &server.link {
              ServerLink::Stream(writer) => {
                let _ = writer.send(tx.message.to_vec());
              }
              ServerLink::Udp => udp_writes.push(tx),
            }
          }
        }
        for tx in udp_writes {
          if let Some(socket) = core.endpoint_for(tx.transport.local_addr) {
            let _ = socket.try_send_to(&tx.message, tx.transport.peer_addr);
          }
        }
      }));
    }
    if let Ok(mut core) = self.core.try_borrow_mut() {
      core.servers.clear();
      core.endpoints.clear();
    }
    self.cancel.cancel();
    self.wake.notify_one();
    self.events_ready.notify_one();
  }

  fn ensure_driver(self: &Rc<Self>) {
    if self.driver_started.replace(true) || self.closed.get() {
      return;
    }
    let Some(rx) = self.core.borrow_mut().recv_rx.take() else {
      return;
    };
    let peer = self.clone();
    let cancel = self.cancel.clone();
    deno_core::unsync::spawn(async move {
      let _ = run_driver(peer, rx).or_cancel(cancel).await;
    });
  }

  /// Binds one UDP socket per family and adds the host candidates, then
  /// starts one client per ICE server, once (the first
  /// `setLocalDescription`).
  fn start_gathering(self: &Rc<Self>) {
    let specs = {
      let mut core = self.core.borrow_mut();
      if core.gathering != GatherPhase::NotStarted {
        return;
      }
      core.gathering = GatherPhase::Running;
      core.events.push_back(JsEvent::IceGatheringState {
        state: "gathering".to_string(),
      });

      let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
      // (bind address, host candidate addresses)
      let mut plan: Vec<(IpAddr, Vec<IpAddr>)> = Vec::new();
      let interfaces = if loopback_only() {
        None
      } else {
        match self.guard.interfaces() {
          Ok(()) => Some(interface_addresses()),
          Err(message) => {
            core.report_error(
              "",
              None,
              ERROR_CODE_UNREACHABLE,
              format!("Only loopback host candidates were gathered: {message}"),
            );
            None
          }
        }
      };
      match interfaces {
        Some(ifaces) => {
          let v4: Vec<IpAddr> = std::iter::once(loopback)
            .chain(ifaces.iter().copied().filter(IpAddr::is_ipv4))
            .collect();
          let v6: Vec<IpAddr> =
            ifaces.iter().copied().filter(IpAddr::is_ipv6).collect();
          let any4 = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
          match self.guard.listen(any4) {
            Ok(()) => plan.push((any4, v4)),
            Err(message) => {
              core.report_error(
                "",
                None,
                ERROR_CODE_UNREACHABLE,
                format!(
                  "Only loopback host candidates were gathered: {message}"
                ),
              );
              plan.push((loopback, vec![loopback]));
            }
          }
          let any6 = IpAddr::V6(Ipv6Addr::UNSPECIFIED);
          if !v6.is_empty() && self.guard.listen(any6).is_ok() {
            plan.push((any6, v6));
          }
        }
        None => plan.push((loopback, vec![loopback])),
      }

      for (bind, hosts) in plan {
        if !bind.is_unspecified()
          && let Err(message) = self.guard.listen(bind)
        {
          core.report_error(
            "",
            Some(SocketAddr::new(bind, 0)),
            ERROR_CODE_UNREACHABLE,
            message,
          );
          continue;
        }
        let socket = match bind_udp(SocketAddr::new(bind, 0)) {
          Ok(s) => Rc::new(s),
          Err(e) => {
            core.report_error(
              "",
              Some(SocketAddr::new(bind, 0)),
              ERROR_CODE_UNREACHABLE,
              e.to_string(),
            );
            continue;
          }
        };
        let Ok(bound) = socket.local_addr() else {
          continue;
        };
        spawn_udp_reader(
          socket.clone(),
          bound,
          core.recv_tx.clone(),
          self.cancel.clone(),
        );
        if !core.options.relay_only {
          for ip in &hosts {
            let addr = SocketAddr::new(*ip, bound.port());
            let line = candidate_line(
              CandidateHostConfig {
                base_config: base_config(addr),
                ..Default::default()
              }
              .new_candidate_host(),
            );
            if let Err(e) = line.and_then(|line| core.add_local(line, None)) {
              core.report_error(
                "",
                Some(addr),
                ERROR_CODE_UNREACHABLE,
                e.to_string(),
              );
            }
          }
        }
        core.endpoints.push(UdpEndpoint {
          socket,
          bound,
          hosts,
        });
      }
      core.resolving = core.options.ice_servers.len();
      core.options.ice_servers.clone()
    };
    for spec in specs {
      let peer = self.clone();
      let cancel = self.cancel.clone();
      deno_core::unsync::spawn(async move {
        let _ = add_server(peer, spec).or_cancel(cancel).await;
      });
    }
    self.poke();
  }
}

fn spawn_udp_reader(
  socket: Rc<UdpSocket>,
  local: SocketAddr,
  tx: mpsc::Sender<Datagram>,
  cancel: Rc<CancelHandle>,
) {
  deno_core::unsync::spawn(
    async move {
      let mut buf = vec![0u8; RECV_BUFFER_SIZE];
      loop {
        match socket.recv_from(&mut buf).await {
          Ok((n, from)) => {
            let dg = Datagram {
              local,
              from,
              stream: None,
              data: BytesMut::from(&buf[..n]),
              closed: None,
            };
            if let Err(mpsc::error::TrySendError::Closed(_)) = tx.try_send(dg) {
              break;
            }
          }
          Err(e) => {
            // ICMP port unreachable surfaces as a read error on some
            // platforms (Windows' WSAECONNRESET); keep reading.
            log::debug!("webrtc: recv on {local}: {e}");
            tokio::time::sleep(Duration::from_millis(1)).await;
          }
        }
      }
    }
    .or_cancel(cancel),
  );
}

/// Reads RFC 6544 frames off a TURN server stream into the driver's queue;
/// when the stream ends, tells the driver why.
async fn read_stream<R: tokio::io::AsyncRead + Unpin>(
  mut reader: R,
  local: SocketAddr,
  server: SocketAddr,
  index: usize,
  tx: mpsc::Sender<Datagram>,
) {
  let mut buf: Vec<u8> = Vec::with_capacity(RECV_BUFFER_SIZE);
  let mut chunk = vec![0u8; RECV_BUFFER_SIZE];
  let reason = 'read: loop {
    let n = match reader.read(&mut chunk).await {
      Ok(0) => break 'read "the server closed the connection".to_string(),
      Err(e) => break 'read e.to_string(),
      Ok(n) => n,
    };
    buf.extend_from_slice(&chunk[..n]);
    loop {
      match stream_frame_len(&buf) {
        None => break 'read "the server sent a malformed frame".to_string(),
        Some(0) => break,
        Some(len) => {
          let frame: Vec<u8> = buf.drain(..len).collect();
          let dg = Datagram {
            local,
            from: server,
            stream: Some(index),
            data: BytesMut::from(&frame[..]),
            closed: None,
          };
          if tx.send(dg).await.is_err() {
            return;
          }
        }
      }
    }
  };
  let _ = tx
    .send(Datagram {
      local,
      from: server,
      stream: Some(index),
      data: BytesMut::new(),
      closed: Some(reason),
    })
    .await;
}

async fn write_stream<W: tokio::io::AsyncWrite + Unpin>(
  mut writer: W,
  mut rx: mpsc::UnboundedReceiver<Vec<u8>>,
) {
  // A failed write also ends the read side, which reports it.
  while let Some(frame) = rx.recv().await {
    if writer.write_all(&frame).await.is_err() {
      let _ = writer.shutdown().await;
      return;
    }
  }
}

/// Opens the TCP (or TLS) connection to a stream TURN server and starts its
/// reader and writer; returns the writer's queue and the local address.
async fn connect_stream(
  peer: &Rc<Peer>,
  spec: &IceServerSpec,
  server: SocketAddr,
  index: usize,
) -> Result<(mpsc::UnboundedSender<Vec<u8>>, SocketAddr), String> {
  let tcp = tokio::net::TcpStream::connect(server)
    .await
    .map_err(|e| format!("Could not connect to {}: {e}", spec.url))?;
  let _ = tcp.set_nodelay(true);
  let local = tcp.local_addr().map_err(|e| e.to_string())?;
  let (writer_tx, writer_rx) = mpsc::unbounded_channel();
  let recv_tx = peer.core.borrow().recv_tx.clone();
  let cancel = peer.cancel.clone();
  match spec.transport {
    ServerTransport::Tls => {
      let config = peer
        .core
        .borrow()
        .options
        .tls
        .clone()
        .ok_or_else(|| "TLS is not available here".to_string())?;
      let name = rustls::pki_types::ServerName::try_from(spec.host.clone())
        .map_err(|e| e.to_string())?;
      let conn = rustls::ClientConnection::new(config, name)
        .map_err(|e| e.to_string())?;
      let mut stream =
        rustls_tokio_stream::TlsStream::new_client_side(tcp, conn, None);
      // Handshake first, so a refused certificate is reported now rather
      // than as a request timeout.
      stream
        .handshake()
        .await
        .map_err(|e| format!("TLS handshake with {} failed: {e}", spec.url))?;
      let (r, w) = stream.into_split();
      deno_core::unsync::spawn(
        read_stream(r, local, server, index, recv_tx).or_cancel(cancel.clone()),
      );
      deno_core::unsync::spawn(write_stream(w, writer_rx).or_cancel(cancel));
    }
    _ => {
      let (r, w) = tcp.into_split();
      deno_core::unsync::spawn(
        read_stream(r, local, server, index, recv_tx).or_cancel(cancel.clone()),
      );
      deno_core::unsync::spawn(write_stream(w, writer_rx).or_cancel(cancel));
    }
  }
  Ok((writer_tx, local))
}

/// Resolves one ICE server and starts its client.
async fn add_server(peer: Rc<Peer>, spec: IceServerSpec) {
  let finish = |peer: &Rc<Peer>, error: Option<String>| {
    let mut core = peer.core.borrow_mut();
    core.resolving = core.resolving.saturating_sub(1);
    if let Some(text) = error {
      core.report_error(&spec.url, None, ERROR_CODE_UNREACHABLE, text);
    }
    drop(core);
    peer.poke();
  };
  let resolved: Vec<SocketAddr> =
    match tokio::net::lookup_host((spec.host.as_str(), spec.port)).await {
      Ok(addrs) => addrs.collect(),
      Err(e) => {
        finish(&peer, Some(format!("Could not resolve {}: {e}", spec.host)));
        return;
      }
    };
  // A family the connection has a socket for (UDP), or any (TCP/TLS).
  let server = {
    let core = peer.core.borrow();
    resolved.iter().copied().find(|server| {
      spec.transport != ServerTransport::Udp
        || core.endpoints.iter().any(|e| {
          e.bound.is_ipv4() == server.is_ipv4()
            && (!e.bound.ip().is_loopback() || server.ip().is_loopback())
        })
    })
  };
  let Some(server) = server else {
    finish(
      &peer,
      Some(format!(
        "No local address can reach {} (interface enumeration or the \
         wildcard listen permission may be denied)",
        spec.host
      )),
    );
    return;
  };
  // The URL's host was checked by the constructor; the address it resolved
  // to must not be denied either.
  if let Err(message) = peer.guard.connect(server) {
    finish(&peer, Some(message));
    return;
  }
  let index = peer.core.borrow().servers.len();
  let (link, base) = match spec.transport {
    ServerTransport::Udp => {
      let base = {
        let mut core = peer.core.borrow_mut();
        let Some(endpoint) = core
          .endpoints
          .iter()
          .position(|e| e.bound.is_ipv4() == server.is_ipv4())
        else {
          drop(core);
          finish(&peer, Some("No UDP socket for this family".to_string()));
          return;
        };
        core.local_for(endpoint, server)
      };
      (ServerLink::Udp, base)
    }
    ServerTransport::Tcp | ServerTransport::Tls => {
      if !spec.turn {
        // A server-reflexive address over TCP describes the TCP mapping,
        // useless to a UDP agent.
        finish(&peer, None);
        return;
      }
      match connect_stream(&peer, &spec, server, index).await {
        Ok((tx, local)) => (ServerLink::Stream(tx), local),
        Err(text) => {
          finish(&peer, Some(text));
          return;
        }
      }
    }
  };
  if peer.closed.get() {
    return;
  }
  let protocol = match link {
    ServerLink::Udp => TransportProtocol::UDP,
    ServerLink::Stream(_) => TransportProtocol::TCP,
  };
  let config = TurnClientConfig {
    stun_serv_addr: server.to_string(),
    turn_serv_addr: if spec.turn {
      server.to_string()
    } else {
      String::new()
    },
    local_addr: base,
    transport_protocol: protocol,
    username: spec.username.clone(),
    password: spec.credential.clone(),
    realm: String::new(),
    software: String::new(),
    rto_in_ms: 0,
    allocation_refresh_interval_cap: None,
  };
  let provider = match rtc::crypto::default_provider() {
    Ok(p) => p,
    Err(e) => {
      finish(&peer, Some(e.to_string()));
      return;
    }
  };
  let client = match TurnClient::new(config, provider) {
    Ok(c) => c,
    Err(e) => {
      finish(&peer, Some(e.to_string()));
      return;
    }
  };
  {
    let mut core = peer.core.borrow_mut();
    core.mark_allowed(server);
    let now = Instant::now();
    let mut entry = ServerClient {
      url: spec.url.clone(),
      client,
      base,
      server,
      link,
      binding: None,
      allocate: None,
      relay: None,
    };
    if matches!(entry.link, ServerLink::Udp) {
      entry.binding = entry.client.send_binding_request(now).ok();
    }
    if spec.turn {
      entry.allocate = entry.client.allocate(now).ok();
    }
    core.servers.push(entry);
  }
  finish(&peer, None);
}

async fn run_driver(peer: Rc<Peer>, mut rx: mpsc::Receiver<Datagram>) {
  loop {
    if peer.closed.get() {
      break;
    }
    let pumped = std::panic::catch_unwind(AssertUnwindSafe(|| {
      let mut core = peer.core.borrow_mut();
      core.pump(&*peer.guard);
      (!core.events.is_empty(), core.next_deadline())
    }));
    let Ok((has_events, deadline)) = pumped else {
      peer.poison();
      break;
    };
    if has_events {
      peer.events_ready.notify_one();
    }
    let sleep = async {
      match deadline {
        Some(d) => {
          tokio::time::sleep_until(tokio::time::Instant::from_std(d)).await
        }
        None => std::future::pending::<()>().await,
      }
    };
    let step = tokio::select! {
      _ = peer.wake.notified() => None,
      dg = rx.recv() => match dg {
        Some(dg) => Some(Some(dg)),
        None => break,
      },
      _ = sleep => Some(None),
    };
    let handled = std::panic::catch_unwind(AssertUnwindSafe(|| {
      let mut core = peer.core.borrow_mut();
      match step {
        Some(Some(dg)) => {
          core.handle_datagram(dg);
          // Whatever else already arrived goes in before the next pump.
          while let Ok(dg) = rx.try_recv() {
            core.handle_datagram(dg);
          }
        }
        Some(None) => core.handle_timeout(Instant::now()),
        None => {}
      }
    }));
    if handled.is_err() {
      peer.poison();
      break;
    }
  }
}

#[cfg(test)]
pub(crate) fn frame_len_for_tests(buf: &[u8]) -> Option<usize> {
  stream_frame_len(buf)
}

#[cfg(test)]
impl Peer {
  pub(crate) fn selected_for_tests(&self) -> Option<(SocketAddr, SocketAddr)> {
    self.core.borrow().selected
  }
}
