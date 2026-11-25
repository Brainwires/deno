// Copyright 2018-2026 the Deno authors. MIT license.

//! A standalone `RTCIceTransport` (W3C webrtc-ice, "IceTransport Extensions
//! for WebRTC"): `new RTCIceTransport()`, then `gather()`, `start()`,
//! `addRemoteCandidate()` and `stop()`, without a peer connection.
//!
//! The engine's sans-I/O ICE agent (`rtci::ice::Agent`) does the checks,
//! nomination and role-conflict resolution (RFC 8445); this module owns the
//! I/O around it, as peer.rs does for a connection:
//!
//! - one UDP socket per host address, bound to that address (so the local
//!   address of every datagram is the candidate's, which the agent matches
//!   on), each one a host candidate. With `DENO_WEBRTC_LOOPBACK_ONLY=1`, or
//!   without interface enumeration, `127.0.0.1` alone;
//! - the same net permission gate as a connection: a datagram goes only to
//!   an address `--allow-net` allows (checked once per address);
//! - a driver task that moves datagrams between the sockets and the agent,
//!   fires its timers and queues its events for JavaScript.
//!
//! Gathering is host candidates only: server-reflexive and relay candidates
//! need the STUN / TURN clients peer.rs runs for a connection, which a
//! standalone transport doesn't have yet (`gather()` accepts and validates
//! `iceServers`, and with `gatherPolicy: "relay"` gathers nothing).
//!
//! Everything runs on the isolate's thread, like peer.rs: ops and the driver
//! share the [`IceCore`] through a `RefCell` and never hold it across an
//! `.await`.

use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use bytes::BytesMut;
use deno_core::CancelFuture;
use deno_core::CancelHandle;
use rtci::ice::Agent;
use rtci::ice::AgentConfig;
use rtci::ice::Event as AgentEvent;
use rtci::ice::mdns::MulticastDnsMode;
use rtci::peer_connection::transport::CandidateConfig;
use rtci::peer_connection::transport::CandidateHostConfig;
use rtci::peer_connection::transport::RTCIceCandidate;
use rtci::sansio::Protocol;
use rtci::shared::TaggedBytesMut;
use rtci::shared::TransportContext;
use rtci::shared::TransportProtocol;
use serde::Serialize;
use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tokio::sync::mpsc;

use crate::WebRtcError;
use crate::peer::NetGuard;
use crate::peer::PairCandidate;
use crate::peer::SelectedPair;
use crate::peer::rtc_err;

/// The largest UDP payload read from a socket (as peer.rs).
const RECV_BUFFER_SIZE: usize = 9216;
/// Datagrams queued between the socket readers and the driver.
const RECV_QUEUE: usize = 1024;
/// Most host candidates (sockets) one transport gathers.
const MAX_HOSTS: usize = 16;

/// An event for JavaScript, delivered in batches by
/// `op_webrtc_ice_next_events`.
#[derive(Serialize, Debug)]
#[serde(tag = "type")]
pub enum IceEvent {
  /// A gathered local candidate (its candidate-attribute value).
  #[serde(rename = "icecandidate")]
  Candidate { candidate: String },
  /// Gathering finished: the null candidate, then `complete`.
  #[serde(rename = "gatheringcomplete")]
  GatheringComplete,
  /// The agent's connection state, lower case (`checking`, `connected`,
  /// `completed`, `disconnected`, `failed`, `closed`).
  #[serde(rename = "statechange")]
  State { state: String },
  /// The agent selected a pair (`op_webrtc_ice_selected_pair` reads it).
  #[serde(rename = "selectedcandidatepairchange")]
  SelectedPair,
  /// A role conflict (RFC 8445 section 7.3.1.1) switched the role.
  #[serde(rename = "rolechange")]
  Role { controlling: bool },
}

/// One received datagram, for the driver.
struct Datagram {
  local: SocketAddr,
  from: SocketAddr,
  data: BytesMut,
}

struct Endpoint {
  socket: Rc<UdpSocket>,
  local: SocketAddr,
}

/// The agent and the I/O state around it, shared by ops and the driver.
struct IceCore {
  agent: Agent,
  endpoints: Vec<Endpoint>,
  events: VecDeque<IceEvent>,
  allowed: HashSet<SocketAddr>,
  refused: HashSet<SocketAddr>,
  started: bool,
  recv_tx: mpsc::Sender<Datagram>,
  recv_rx: Option<mpsc::Receiver<Datagram>>,
}

pub struct IceTransport {
  core: RefCell<IceCore>,
  guard: Rc<dyn NetGuard>,
  /// Ops -> driver: there is something to flush.
  wake: Notify,
  /// Driver -> `next_events`: `core.events` is not empty.
  events_ready: Notify,
  cancel: Rc<CancelHandle>,
  closed: Cell<bool>,
  driver_started: Cell<bool>,
}

impl IceTransport {
  /// A transport with fresh local ICE parameters (RFC 8445 section 5.3:
  /// at least 24 bits of ufrag and 128 of password, which the agent
  /// generates), gathering nothing until `gather()`.
  pub fn new(guard: Rc<dyn NetGuard>) -> Result<Rc<Self>, WebRtcError> {
    let provider = rtci::crypto::default_provider()
      .map_err(|e| WebRtcError::Operation(e.to_string()))?;
    let config = AgentConfig {
      // `.local` names need the mDNS socket peer.rs opens for a connection;
      // a standalone transport takes literal addresses.
      multicast_dns_mode: MulticastDnsMode::Disabled,
      ..Default::default()
    };
    let agent = Agent::new(Instant::now(), Arc::new(config), provider)
      .map_err(rtc_err)?;
    let (recv_tx, recv_rx) = mpsc::channel(RECV_QUEUE);
    Ok(Rc::new(Self {
      core: RefCell::new(IceCore {
        agent,
        endpoints: Vec::new(),
        events: VecDeque::new(),
        allowed: HashSet::new(),
        refused: HashSet::new(),
        started: false,
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

  /// `getLocalParameters()`: `(usernameFragment, password)`.
  pub fn local_parameters(&self) -> (String, String) {
    let core = self.core.borrow();
    let credentials = core.agent.get_local_credentials();
    (credentials.ufrag.clone(), credentials.pwd.clone())
  }

  /// `gather()`: binds a socket per host address and queues each host
  /// candidate, then the end of gathering. `relay_only` (`gatherPolicy:
  /// "relay"`) gathers no host candidate.
  pub fn gather(self: &Rc<Self>, relay_only: bool) -> Result<(), WebRtcError> {
    if self.closed.get() {
      return Err(WebRtcError::InvalidState(
        "The RTCIceTransport is closed".to_string(),
      ));
    }
    if !relay_only {
      for ip in host_addresses(&*self.guard) {
        let socket = match crate::peer::bind_udp(SocketAddr::new(ip, 0)) {
          Ok(s) => Rc::new(s),
          Err(e) => {
            log::debug!("webrtc: RTCIceTransport bind on {ip}: {e}");
            continue;
          }
        };
        let Ok(local) = socket.local_addr() else {
          continue;
        };
        let candidate = CandidateHostConfig {
          base_config: CandidateConfig {
            network: "udp".to_owned(),
            address: local.ip().to_string(),
            port: local.port(),
            component: 1,
            ..Default::default()
          },
          ..Default::default()
        }
        .new_candidate_host();
        let mut core = self.core.borrow_mut();
        let candidate = match candidate {
          Ok(c) => c,
          Err(e) => {
            log::debug!("webrtc: RTCIceTransport candidate on {local}: {e}");
            continue;
          }
        };
        let line = RTCIceCandidate::from(&candidate)
          .to_json()
          .map(|init| init.candidate);
        if core.agent.add_local_candidate(candidate).is_err() {
          continue;
        }
        if let Ok(line) = line {
          core
            .events
            .push_back(IceEvent::Candidate { candidate: line });
        }
        spawn_reader(
          socket.clone(),
          local,
          core.recv_tx.clone(),
          self.cancel.clone(),
        );
        core.endpoints.push(Endpoint { socket, local });
      }
    }
    self
      .core
      .borrow_mut()
      .events
      .push_back(IceEvent::GatheringComplete);
    self.ensure_driver();
    self.poke();
    Ok(())
  }

  /// `start()`: the remote ICE parameters and the role. A later call with
  /// other parameters is an ICE restart of the remote side: the remote
  /// candidates are flushed (W3C webrtc-ice `start()` step "flush").
  pub fn start(
    self: &Rc<Self>,
    ufrag: String,
    pwd: String,
    controlling: bool,
  ) -> Result<(), WebRtcError> {
    if self.closed.get() {
      return Err(WebRtcError::InvalidState(
        "The RTCIceTransport is closed".to_string(),
      ));
    }
    {
      let mut core = self.core.borrow_mut();
      let now = Instant::now();
      if core.started {
        let same = core
          .agent
          .get_remote_credentials()
          .is_some_and(|c| c.ufrag == ufrag && c.pwd == pwd);
        if same {
          return Ok(());
        }
        // Keeps the local candidates and credentials; drops the remote
        // credentials, candidates and pairs.
        core.agent.apply_restart(now, true).map_err(rtc_err)?;
      }
      core
        .agent
        .start_connectivity_checks(now, controlling, ufrag, pwd)
        .map_err(rtc_err)?;
      core.started = true;
    }
    self.ensure_driver();
    self.poke();
    Ok(())
  }

  /// `addRemoteCandidate()`: a candidate-attribute value (`candidate:...`).
  pub fn add_remote_candidate(
    self: &Rc<Self>,
    candidate: &str,
  ) -> Result<(), WebRtcError> {
    if self.closed.get() {
      return Err(WebRtcError::InvalidState(
        "The RTCIceTransport is closed".to_string(),
      ));
    }
    let value = candidate.trim();
    let value = value.strip_prefix("a=").unwrap_or(value);
    let value = value.strip_prefix("candidate:").unwrap_or(value);
    let parsed = rtci::ice::candidate::unmarshal_candidate(value)
      .map_err(|e| WebRtcError::Operation(format!("Invalid candidate: {e}")))?;
    self
      .core
      .borrow_mut()
      .agent
      .add_remote_candidate(parsed)
      .map_err(rtc_err)?;
    self.ensure_driver();
    self.poke();
    Ok(())
  }

  /// `getSelectedCandidatePair()`.
  pub fn selected_pair(&self) -> Option<SelectedPair> {
    if self.closed.get() {
      return None;
    }
    let core = self.core.borrow();
    let (local, remote) = core.agent.get_selected_candidate_pair()?;
    Some(SelectedPair {
      local: PairCandidate::of(&RTCIceCandidate::from(local)),
      remote: PairCandidate::of(&RTCIceCandidate::from(remote)),
    })
  }

  /// `stop()`: closes the agent and the sockets, for good.
  pub fn stop(&self) {
    if self.closed.replace(true) {
      return;
    }
    if let Ok(mut core) = self.core.try_borrow_mut() {
      let _ = core.agent.close();
      core.endpoints.clear();
      core.events.clear();
    }
    self.cancel.cancel();
    self.events_ready.notify_one();
  }

  pub async fn next_events(&self) -> Option<Vec<IceEvent>> {
    loop {
      {
        let mut core = self.core.borrow_mut();
        if self.closed.get() {
          return None;
        }
        if !core.events.is_empty() {
          return Some(core.events.drain(..).collect());
        }
      }
      self.events_ready.notified().await;
    }
  }

  fn poke(&self) {
    self.wake.notify_one();
  }

  fn ensure_driver(self: &Rc<Self>) {
    if self.driver_started.replace(true) || self.closed.get() {
      return;
    }
    let Some(rx) = self.core.borrow_mut().recv_rx.take() else {
      return;
    };
    let transport = self.clone();
    let cancel = self.cancel.clone();
    deno_core::unsync::spawn(async move {
      let _ = run_driver(transport, rx).or_cancel(cancel).await;
    });
  }
}

impl IceCore {
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
      Err(e) => {
        log::debug!("webrtc: RTCIceTransport may not send to {addr}: {e}");
        self.refused.insert(addr);
        false
      }
    }
  }

  /// Sends what the agent wants sent, and queues its events. A selected
  /// pair is reported before the `connected` it comes with (the agent
  /// queues them the other way round), so `getSelectedCandidatePair()`
  /// already answers when the state changes.
  fn pump(&mut self, guard: &dyn NetGuard) {
    while let Some(msg) = self.agent.poll_write() {
      let to = msg.transport.peer_addr;
      let local = msg.transport.local_addr;
      if !self.permit(guard, to) {
        continue;
      }
      let Some(endpoint) = self.endpoints.iter().find(|e| e.local == local)
      else {
        continue;
      };
      if let Err(e) = endpoint.socket.try_send_to(&msg.message, to) {
        log::debug!("webrtc: RTCIceTransport send to {to}: {e}");
      }
    }
    let mut states = Vec::new();
    while let Some(tagged) = self.agent.poll_event() {
      match tagged.event {
        AgentEvent::ConnectionStateChange(state) => {
          let state = state.to_string().to_ascii_lowercase();
          if state != "unspecified" && state != "new" {
            states.push(IceEvent::State { state });
          }
        }
        AgentEvent::SelectedCandidatePairChange(_, _) => {
          self.events.push_back(IceEvent::SelectedPair);
        }
        AgentEvent::RoleChange(controlling) => {
          self.events.push_back(IceEvent::Role { controlling });
        }
        _ => {}
      }
    }
    self.events.extend(states);
  }

  fn handle_datagram(&mut self, dg: Datagram) {
    if self.refused.contains(&dg.from) {
      return;
    }
    let _ = self.agent.handle_read(TaggedBytesMut {
      now: Instant::now(),
      transport: TransportContext {
        local_addr: dg.local,
        peer_addr: dg.from,
        ecn: None,
        transport_protocol: TransportProtocol::UDP,
      },
      message: dg.data,
    });
  }
}

/// The host addresses to gather on: `127.0.0.1` alone under
/// `DENO_WEBRTC_LOOPBACK_ONLY=1` or without interface enumeration
/// (`--allow-sys=networkInterfaces`), else the interface addresses the
/// listen permission allows (`127.0.0.1` when there is none).
fn host_addresses(guard: &dyn NetGuard) -> Vec<IpAddr> {
  let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
  let mut out = Vec::new();
  if !crate::peer::loopback_only() && guard.interfaces().is_ok() {
    for ip in crate::peer::interface_addresses(None) {
      if out.len() < MAX_HOSTS && guard.listen(ip).is_ok() {
        out.push(ip);
      }
    }
  }
  if out.is_empty() && guard.listen(loopback).is_ok() {
    out.push(loopback);
  }
  out
}

fn spawn_reader(
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
              data: BytesMut::from(&buf[..n]),
            };
            if let Err(mpsc::error::TrySendError::Closed(_)) = tx.try_send(dg) {
              break;
            }
          }
          Err(e) => {
            // ICMP port unreachable surfaces as a read error on some
            // platforms; keep reading.
            log::debug!("webrtc: RTCIceTransport recv on {local}: {e}");
            tokio::time::sleep(Duration::from_millis(1)).await;
          }
        }
      }
    }
    .or_cancel(cancel),
  );
}

async fn run_driver(
  transport: Rc<IceTransport>,
  mut rx: mpsc::Receiver<Datagram>,
) {
  loop {
    if transport.closed.get() {
      break;
    }
    let (has_events, deadline) = {
      let mut core = transport.core.borrow_mut();
      core.pump(&*transport.guard);
      (!core.events.is_empty(), core.agent.poll_timeout())
    };
    if has_events {
      transport.events_ready.notify_one();
    }
    let sleep = async {
      match deadline {
        Some(d) => {
          tokio::time::sleep_until(tokio::time::Instant::from_std(d)).await
        }
        None => std::future::pending::<()>().await,
      }
    };
    tokio::select! {
      _ = transport.wake.notified() => {}
      dg = rx.recv() => match dg {
        Some(dg) => {
          let mut core = transport.core.borrow_mut();
          core.handle_datagram(dg);
          while let Ok(dg) = rx.try_recv() {
            core.handle_datagram(dg);
          }
        }
        None => break,
      },
      _ = sleep => {
        let _ = transport.core.borrow_mut().agent.handle_timeout(Instant::now());
      }
    }
  }
}
