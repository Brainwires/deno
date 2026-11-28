// Copyright 2018-2026 the Deno authors. MIT license.

//! WebRTC data channels: `RTCPeerConnection`, `RTCDataChannel`,
//! `RTCSessionDescription`, `RTCIceCandidate` and their events, as globals
//! (DENEXT.md item 370).
//!
//! The protocol stack (ICE, DTLS, SCTP, the data channel protocol) is
//! webrtc-rs's sans-I/O `rtci` engine; `peer.rs` owns the sockets, the
//! timers and the STUN/TURN clients around it. The JavaScript side is
//! `01_webrtc.js`.
//!
//! Permissions (least-privilege guardrails, like every other net API): the
//! constructor checks each ICE server's host and port (`--allow-net`, or
//! `--allow-net-connect`); every datagram is sent only to an address the net
//! permission allows (checked once per address); a remote candidate whose
//! address is refused makes `addIceCandidate` / `setRemoteDescription`
//! reject; a socket is bound on an address only with the listen-direction
//! permission for it; and the machine's interface addresses (host
//! candidates beyond `127.0.0.1`) are gathered only with
//! `--allow-sys=networkInterfaces`, the permission `Deno.networkInterfaces()`
//! needs.

use std::cell::RefCell;
use std::collections::HashMap;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use deno_core::OpState;
use deno_core::Resource;
use deno_core::ResourceId;
use deno_core::op2;
use deno_core::v8;
use deno_net::DefaultTlsOptions;
use deno_net::UnsafelyIgnoreCertificateErrors;
use deno_permissions::PermissionsContainer;
use rtci::data_channel::RTCDataChannelInit;
use rtci::peer_connection::certificate::CertificateParams;
use rtci::peer_connection::certificate::RTCCertificate;
use rtci::peer_connection::configuration::RTCBundlePolicy;
use serde::Deserialize;
use serde::Serialize;

mod ecn;
mod encoded_frames;
mod fake_media;
// wpt100:ice
mod ice_transport;
// wpt100:hdrext
mod header_extensions;
mod media;
mod media_sink;
mod net;
mod peer;

pub use peer::IceServerSpec;
pub use peer::JsEvent;
pub use peer::NetGuard;
pub use peer::Peer;
pub use peer::PeerOptions;
pub use peer::ServerTransport;

const API_NAME: &str = "RTCPeerConnection";

/// How verbose the engine's log is (`Deno.webrtc.setLogLevel`), as a
/// `log::LevelFilter` (0 = off, the default). Process-wide, like the log.
static ENGINE_LOG_LEVEL: AtomicUsize = AtomicUsize::new(0);

const LOG_LEVELS: [log::LevelFilter; 6] = [
  log::LevelFilter::Off,
  log::LevelFilter::Error,
  log::LevelFilter::Warn,
  log::LevelFilter::Info,
  log::LevelFilter::Debug,
  log::LevelFilter::Trace,
];

/// Whether a log record comes from the WebRTC engine (the `rtci` and `rtci_*`
/// crates), whose records `Deno.webrtc.setLogLevel` governs.
pub fn is_engine_log_target(target: &str) -> bool {
  target == "rtci"
    || target.starts_with("rtci::")
    || target.starts_with("rtci_")
}

/// The engine's log level (`Deno.webrtc.setLogLevel`; off by default: its
/// outcomes are `RTCPeerConnection` events). The runtime's logger shows
/// engine records at or above it, and no others.
pub fn engine_log_level() -> log::LevelFilter {
  LOG_LEVELS
    .get(ENGINE_LOG_LEVEL.load(Ordering::Relaxed))
    .copied()
    .unwrap_or(log::LevelFilter::Off)
}

/// Sets the engine's log level by name: `off`, `error`, `warn`, `info`,
/// `debug` or `trace`.
pub fn set_engine_log_level(name: &str) -> Result<(), WebRtcError> {
  let Some(index) = LOG_LEVELS
    .iter()
    .position(|l| l.as_str().eq_ignore_ascii_case(name))
  else {
    return Err(WebRtcError::Type(format!(
      "'{name}' is not a log level: use 'off', 'error', 'warn', 'info', \
       'debug' or 'trace'"
    )));
  };
  ENGINE_LOG_LEVEL.store(index, Ordering::Relaxed);
  // The `log` macros skip records above the global maximum before asking
  // the logger, so it has to admit the engine's level (it is never lowered
  // here: other targets may need it).
  if LOG_LEVELS[index] > log::max_level() {
    log::set_max_level(LOG_LEVELS[index]);
  }
  Ok(())
}

deno_core::extension!(
  deno_webrtc,
  deps = [deno_web, deno_net],
  ops = [
    op_webrtc_pc_create,
    op_webrtc_pc_create_offer,
    op_webrtc_pc_create_answer,
    op_webrtc_pc_set_local_description,
    op_webrtc_pc_set_remote_description,
    op_webrtc_pc_add_ice_candidate,
    op_webrtc_pc_descriptions,
    op_webrtc_pc_create_data_channel,
    op_webrtc_pc_next_events,
    op_webrtc_pc_get_stats,
    op_webrtc_pc_close,
    op_webrtc_pc_max_message_size,
    op_webrtc_dc_send,
    op_webrtc_dc_id,
    op_webrtc_dc_set_low_threshold,
    op_webrtc_dc_close,
    op_webrtc_generate_certificate,
    op_webrtc_certificate_to_pem,
    op_webrtc_certificate_from_pem,
    op_webrtc_certificate_drop,
    op_webrtc_capabilities,
    op_webrtc_pc_add_transceiver,
    op_webrtc_pc_attach_track,
    op_webrtc_pc_transceivers,
    op_webrtc_transceiver_set_direction,
    op_webrtc_transceiver_stop,
    op_webrtc_transceiver_set_codec_preferences,
    op_webrtc_sender_set_paused,
    op_webrtc_sender_set_streams,
    op_webrtc_sender_set_track_codec,
    op_webrtc_sender_parameters,
    op_webrtc_receiver_parameters,
    op_webrtc_receiver_sources,
    op_webrtc_sender_write_rtp,
    op_webrtc_sender_write_frame,
    op_webrtc_sender_write_fake,
    op_webrtc_sender_fake_frame,
    op_webrtc_sender_reserve_frame,
    op_webrtc_sender_write_frame_at,
    op_webrtc_receiver_transform,
    op_webrtc_fake_media_devices,
    op_webrtc_receiver_read_rtp,
    op_webrtc_receiver_request_key_frame,
    op_webrtc_sender_send_dtmf,
    op_webrtc_pc_selected_pair,
    op_webrtc_pc_set_ice_servers,
    op_webrtc_network_create,
    op_webrtc_network_addresses,
    op_webrtc_network_tcp_addresses,
    op_webrtc_network_close,
    op_webrtc_set_log_level,
    op_webrtc_data_globals,
    // wpt100:ice
    op_webrtc_pc_ice_selected_pair,
    op_webrtc_ice_create,
    op_webrtc_ice_local_parameters,
    op_webrtc_ice_gather,
    op_webrtc_ice_start,
    op_webrtc_ice_add_remote_candidate,
    op_webrtc_ice_selected_pair,
    op_webrtc_ice_stop,
    op_webrtc_ice_next_events,
    // wpt100:hdrext
    op_webrtc_transceiver_set_header_extensions,
    op_webrtc_pc_set_negotiation_options,
    // wpt100:mediasink
    op_webrtc_receiver_read_frames,
    op_webrtc_receiver_muted,
  ],
  lazy_loaded_js = [
    "01_webrtc.js",
    "02_media_devices.js",
    "03_encoded_transform.js",
    "04_sframe.js",
    // wpt100:S2 transferable RTCDataChannel
    "05_transfer.js",
    // wpt100:identity
    "06_identity.js",
  ],
);

/// The getter of a lazy data property (`op_webrtc_data_globals`): calls the
/// JavaScript getter the property had, which loads the module that defines
/// the interface. V8 then replaces the property with the value.
fn data_global_getter<'s>(
  scope: &mut v8::PinScope<'s, '_>,
  _key: v8::Local<'s, v8::Name>,
  args: v8::PropertyCallbackArguments<'s>,
  mut rv: v8::ReturnValue<v8::Value>,
) {
  let Ok(getter) = v8::Local::<v8::Function>::try_from(args.data()) else {
    return;
  };
  let receiver: v8::Local<v8::Value> = args.holder().into();
  if let Some(value) = getter.call(scope, receiver, &[]) {
    rv.set(value);
  }
}

/// Turns the lazily loaded WebRTC interface objects from accessors on the
/// global object into (lazy) data properties, as Web IDL has an interface
/// object: `Object.getOwnPropertyDescriptor(globalThis, "RTCPeerConnection")`
/// is `{ value, writable: true, enumerable: false, configurable: true }`
/// with no getter. The module still loads on first use: V8 calls the old
/// getter the first time the property is read, then keeps the value. Called
/// once per realm at bootstrap (a snapshot can't hold these properties
/// without an external reference for their getter); `names` that are not
/// own accessors of `target` are skipped.
#[op2(nofast)]
fn op_webrtc_data_globals<'s>(
  scope: &mut v8::PinScope<'s, '_>,
  target: v8::Local<'s, v8::Object>,
  names: v8::Local<'s, v8::Array>,
) {
  let get_key = v8::String::new_external_onebyte_static(scope, b"get")
    .unwrap()
    .into();
  for i in 0..names.length() {
    let Some(name) = names.get_index(scope, i) else {
      continue;
    };
    let Ok(name) = v8::Local::<v8::Name>::try_from(name) else {
      continue;
    };
    let Some(descriptor) = target.get_own_property_descriptor(scope, name)
    else {
      continue;
    };
    let Ok(descriptor) = v8::Local::<v8::Object>::try_from(descriptor) else {
      continue;
    };
    let Some(getter) = descriptor.get(scope, get_key) else {
      continue;
    };
    if !getter.is_function() {
      continue;
    }
    if target.delete(scope, name.into()) != Some(true) {
      continue;
    }
    target.set_lazy_data_property_with_data(
      scope,
      name,
      data_global_getter,
      getter,
      v8::PropertyAttribute::DONT_ENUM,
      v8::SideEffectType::HasSideEffect,
      v8::SideEffectType::HasSideEffect,
    );
  }
}

#[derive(Debug, thiserror::Error, deno_error::JsError)]
pub enum WebRtcError {
  #[class(type)]
  #[error("{0}")]
  Type(String),
  #[class("DOMExceptionSyntaxError")]
  #[error("{0}")]
  Syntax(String),
  #[class("DOMExceptionInvalidAccessError")]
  #[error("{0}")]
  InvalidAccess(String),
  #[class("DOMExceptionInvalidStateError")]
  #[error("{0}")]
  InvalidState(String),
  #[class("DOMExceptionOperationError")]
  #[error("{0}")]
  Operation(String),
  #[class("DOMExceptionInvalidModificationError")]
  #[error("{0}")]
  InvalidModification(String),
  #[class("DOMExceptionNotSupportedError")]
  #[error("{0}")]
  NotSupported(String),
  #[class("NotCapable")]
  #[error("{0}")]
  Permission(String),
  #[class(inherit)]
  #[error(transparent)]
  PermissionCheck(#[from] deno_permissions::PermissionCheckError),
  #[class(inherit)]
  #[error(transparent)]
  Resource(#[from] deno_core::error::ResourceError),
}

/// The runtime's permissions as a [`NetGuard`].
struct PermissionsGuard(RefCell<PermissionsContainer>);

impl NetGuard for PermissionsGuard {
  fn connect(&self, addr: SocketAddr) -> Result<(), String> {
    let mut perms = self.0.borrow_mut();
    let ip = addr.ip().to_string();
    perms
      .check_net_connect(&(ip.as_str(), Some(addr.port())), API_NAME)
      .and_then(|()| {
        perms.check_net_connect_resolved(&addr.ip(), addr.port(), API_NAME)
      })
      .map_err(|e| e.to_string())
  }

  fn listen(&self, ip: IpAddr) -> Result<(), String> {
    let mut perms = self.0.borrow_mut();
    let host = ip.to_string();
    perms
      .check_net_listen(&(host.as_str(), None), API_NAME)
      .map_err(|e| e.to_string())
  }

  fn interfaces(&self) -> Result<(), String> {
    self
      .0
      .borrow()
      .check_sys("networkInterfaces", API_NAME)
      .map_err(|e| e.to_string())
  }
}

struct PeerResource(Rc<Peer>);

impl Resource for PeerResource {
  fn name(&self) -> std::borrow::Cow<'_, str> {
    "rtcPeerConnection".into()
  }

  fn close(self: Rc<Self>) {
    self.0.close();
  }
}

fn peer(state: &OpState, rid: ResourceId) -> Result<Rc<Peer>, WebRtcError> {
  Ok(state.resource_table.get::<PeerResource>(rid)?.0.clone())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IceServerArg {
  urls: Vec<String>,
  username: Option<String>,
  credential: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfigArg {
  ice_servers: Vec<IceServerArg>,
  ice_transport_policy: String,
  #[serde(default)]
  bundle_policy: Option<String>,
  /// `RTCCertificate` ids (the certificate store's).
  #[serde(default)]
  certificates: Vec<u32>,
  /// `RTCConfiguration.denoNetwork`: a `Deno.webrtc.Network` resource.
  #[serde(default)]
  network: Option<ResourceId>,
}

/// Parses and permission-checks `RTCConfiguration.iceServers`.
fn ice_server_specs(
  permissions: &mut PermissionsContainer,
  servers: &[IceServerArg],
) -> Result<Vec<IceServerSpec>, WebRtcError> {
  let mut specs = Vec::new();
  for server in servers {
    for url in &server.urls {
      specs.push(IceServerSpec::parse(
        url,
        server.username.as_deref(),
        server.credential.as_deref(),
      )?);
    }
  }
  for spec in &specs {
    permissions
      .check_net_connect(&(spec.host.as_str(), Some(spec.port)), API_NAME)?;
  }
  Ok(specs)
}

#[op2(stack_trace)]
#[smi]
fn op_webrtc_pc_create(
  state: &mut OpState,
  #[serde] config: ConfigArg,
) -> Result<ResourceId, WebRtcError> {
  let mut permissions = state.borrow::<PermissionsContainer>().clone();
  let specs = ice_server_specs(&mut permissions, &config.ice_servers)?;
  let tls = if specs.iter().any(|s| s.transport == ServerTransport::Tls) {
    Some(tls_config(state)?)
  } else {
    None
  };
  let mut certificates = Vec::new();
  let now = std::time::SystemTime::now();
  for id in &config.certificates {
    let cert = certificate(state, *id)?;
    // The constructor checks `expires` against `Date.now()` first; a
    // certificate that expired since, by this clock, is the same
    // InvalidAccessError, never the engine's OperationError.
    if cert.expires <= now {
      return Err(WebRtcError::InvalidAccess(
        "an RTCCertificate has expired".to_string(),
      ));
    }
    certificates.push(cert);
  }
  let bundle_policy = match config.bundle_policy.as_deref() {
    Some("max-bundle") => RTCBundlePolicy::MaxBundle,
    Some("max-compat") => RTCBundlePolicy::MaxCompat,
    _ => RTCBundlePolicy::Balanced,
  };
  let network = match config.network {
    Some(rid) => {
      Some(state.resource_table.get::<NetworkResource>(rid)?.0.clone())
    }
    None => None,
  };
  if network.as_ref().is_some_and(|n| n.is_closed()) {
    return Err(WebRtcError::InvalidState(
      "The Deno.webrtc.Network is closed".to_string(),
    ));
  }
  let guard = Rc::new(PermissionsGuard(RefCell::new(permissions)));
  let peer = Peer::new(
    PeerOptions {
      ice_servers: specs,
      relay_only: config.ice_transport_policy == "relay",
      tls,
      certificates,
      bundle_policy,
      network,
    },
    guard,
  )?;
  Ok(state.resource_table.add(PeerResource(peer)))
}

/// The TLS client configuration for `turns:` servers: the runtime's root
/// store and `--unsafely-ignore-certificate-errors`, as `Deno.connectTls`.
fn tls_config(
  state: &OpState,
) -> Result<Arc<rustls::ClientConfig>, WebRtcError> {
  let root_cert_store = state
    .borrow::<DefaultTlsOptions>()
    .root_cert_store()
    .map_err(|e| WebRtcError::Operation(e.to_string()))?;
  let unsafely_ignore_certificate_errors = state
    .try_borrow::<UnsafelyIgnoreCertificateErrors>()
    .and_then(|it| it.0.clone());
  let config =
    deno_tls::create_client_config(deno_tls::TlsClientConfigOptions {
      root_cert_store,
      ca_certs: vec![],
      unsafely_ignore_certificate_errors,
      unsafely_disable_hostname_verification: false,
      cert_chain_and_key: deno_tls::TlsKeys::Null,
      socket_use: deno_tls::SocketUse::GeneralSsl,
    })
    .map_err(|e| WebRtcError::Operation(e.to_string()))?;
  Ok(Arc::new(config))
}

#[op2]
#[string]
fn op_webrtc_pc_create_offer(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  ice_restart: bool,
) -> Result<String, WebRtcError> {
  peer(state, rid)?.create_offer(ice_restart)
}

#[op2]
#[string]
fn op_webrtc_pc_create_answer(
  state: &mut OpState,
  #[smi] rid: ResourceId,
) -> Result<String, WebRtcError> {
  peer(state, rid)?.create_answer()
}

#[op2(fast)]
fn op_webrtc_pc_set_local_description(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] kind: String,
  #[string] sdp: String,
) -> Result<(), WebRtcError> {
  peer(state, rid)?.set_local_description(&kind, sdp)
}

#[op2(fast)]
fn op_webrtc_pc_set_remote_description(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] kind: String,
  #[string] sdp: String,
) -> Result<(), WebRtcError> {
  peer(state, rid)?.set_remote_description(&kind, sdp)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CandidateArg {
  candidate: String,
  sdp_mid: Option<String>,
  #[serde(rename = "sdpMLineIndex")]
  sdp_m_line_index: Option<u16>,
  username_fragment: Option<String>,
}

#[op2(stack_trace)]
fn op_webrtc_pc_add_ice_candidate(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[serde] candidate: CandidateArg,
) -> Result<(), WebRtcError> {
  peer(state, rid)?.add_ice_candidate(
    candidate.candidate,
    candidate.sdp_mid,
    candidate.sdp_m_line_index,
    candidate.username_fragment,
  )
}

#[op2]
#[serde]
fn op_webrtc_pc_descriptions(
  state: &mut OpState,
  #[smi] rid: ResourceId,
) -> Result<peer::PeerDescriptions, WebRtcError> {
  Ok(peer(state, rid)?.descriptions())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DataChannelArg {
  label: String,
  ordered: bool,
  max_packet_life_time: Option<u16>,
  max_retransmits: Option<u16>,
  protocol: String,
  negotiated: bool,
  id: Option<u16>,
  // wpt100:S2 the DCEP priority (webrtc-priority 4.3), 256 when absent.
  #[serde(default = "default_channel_priority")]
  priority: u16,
}

fn default_channel_priority() -> u16 {
  256
}

#[derive(Serialize)]
struct CreatedChannel {
  channel: u32,
  id: Option<u16>,
}

#[op2]
#[serde]
fn op_webrtc_pc_create_data_channel(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[serde] arg: DataChannelArg,
) -> Result<CreatedChannel, WebRtcError> {
  let peer = peer(state, rid)?;
  if peer.is_closed() {
    return Err(WebRtcError::InvalidState(
      "RTCPeerConnection is closed".to_string(),
    ));
  }
  let negotiated = if arg.negotiated {
    Some(arg.id.ok_or_else(|| {
      WebRtcError::Type("A negotiated RTCDataChannel needs an id".to_string())
    })?)
  } else {
    None
  };
  let (channel, id) = peer.create_data_channel(
    &arg.label,
    RTCDataChannelInit {
      ordered: arg.ordered,
      max_packet_life_time: arg.max_packet_life_time,
      max_retransmits: arg.max_retransmits,
      protocol: arg.protocol,
      negotiated,
      priority: arg.priority,
    },
  )?;
  Ok(CreatedChannel { channel, id })
}

#[op2]
#[serde]
async fn op_webrtc_pc_next_events(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
) -> Result<Option<Vec<JsEvent>>, WebRtcError> {
  let peer = {
    let state = state.borrow();
    match state.resource_table.get::<PeerResource>(rid) {
      Ok(r) => r.0.clone(),
      Err(_) => return Ok(None),
    }
  };
  Ok(peer.next_events().await)
}

/// `getStats(selector)`: with `selected`, `key` names a transceiver whose
/// sender (`sender`) or receiver is the selector; else the whole connection.
#[op2]
#[serde]
fn op_webrtc_pc_get_stats(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
  selected: bool,
  sender: bool,
) -> Result<Vec<serde_json::Value>, WebRtcError> {
  peer(state, rid)?.stats(selected.then_some((key, sender)))
}

/// `RTCSctpTransport.maxMessageSize`, or 0 before SCTP is up.
#[op2(fast)]
fn op_webrtc_pc_max_message_size(
  state: &mut OpState,
  #[smi] rid: ResourceId,
) -> Result<u32, WebRtcError> {
  Ok(peer(state, rid)?.max_message_size().unwrap_or(0))
}

#[op2(fast)]
fn op_webrtc_pc_close(
  state: &mut OpState,
  #[smi] rid: ResourceId,
) -> Result<(), WebRtcError> {
  if let Ok(resource) = state.resource_table.take::<PeerResource>(rid) {
    resource.0.close();
  }
  Ok(())
}

/// Sends a message; returns the channel's buffered amount after it.
#[op2(fast)]
fn op_webrtc_dc_send(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[smi] channel: u32,
  text: bool,
  #[buffer] data: &[u8],
) -> Result<u32, WebRtcError> {
  peer(state, rid)?.send(channel, text, data)
}

/// `RTCDataChannel.id` while it is still null on the JavaScript side: the
/// stream id the engine has assigned since, or -1 for none yet.
#[op2(fast)]
fn op_webrtc_dc_id(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[smi] channel: u32,
) -> i32 {
  match state.resource_table.get::<PeerResource>(rid) {
    Ok(r) => r.0.data_channel_id(channel).map_or(-1, i32::from),
    Err(_) => -1,
  }
}

#[op2(fast)]
fn op_webrtc_dc_set_low_threshold(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[smi] channel: u32,
  threshold: u32,
) -> Result<(), WebRtcError> {
  peer(state, rid)?.set_buffered_amount_low_threshold(channel, threshold);
  Ok(())
}

#[op2(fast)]
fn op_webrtc_dc_close(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[smi] channel: u32,
) -> Result<(), WebRtcError> {
  peer(state, rid)?.close_data_channel(channel);
  Ok(())
}

// --- RTCCertificate --------------------------------------------------------

/// The `RTCCertificate`s of an isolate, by id: each holds a key and an X.509
/// certificate for the DTLS handshakes of the connections given it. An
/// entry goes when its `RTCCertificate` is garbage collected (a
/// `FinalizationRegistry` in `01_webrtc.js`); a connection keeps its own
/// copy.
#[derive(Default)]
struct CertificateStore {
  next: u32,
  certs: HashMap<u32, RTCCertificate>,
}

fn certificate_store(state: &mut OpState) -> &mut CertificateStore {
  if state.try_borrow::<CertificateStore>().is_none() {
    state.put(CertificateStore::default());
  }
  state.borrow_mut::<CertificateStore>()
}

fn certificate(
  state: &mut OpState,
  id: u32,
) -> Result<RTCCertificate, WebRtcError> {
  certificate_store(state)
    .certs
    .get(&id)
    .cloned()
    .ok_or_else(|| {
      WebRtcError::InvalidAccess("Unknown RTCCertificate".to_string())
    })
}

#[op2(fast)]
fn op_webrtc_certificate_drop(state: &mut OpState, id: u32) {
  certificate_store(state).certs.remove(&id);
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeygenArg {
  name: String,
  named_curve: Option<String>,
  modulus_length: Option<u32>,
  public_exponent: Option<Vec<u8>>,
  hash: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CertificateInfo {
  id: u32,
  /// Milliseconds since the epoch.
  expires: f64,
  fingerprints: Vec<FingerprintInfo>,
}

#[derive(Serialize)]
struct FingerprintInfo {
  algorithm: String,
  value: String,
}

fn certificate_info(
  state: &mut OpState,
  cert: RTCCertificate,
) -> Result<CertificateInfo, WebRtcError> {
  let provider = rtci::crypto::default_provider()
    .map_err(|e| WebRtcError::Operation(e.to_string()))?;
  let fingerprints = cert
    .get_fingerprints(provider.crypto())
    .map_err(|e| WebRtcError::Operation(e.to_string()))?
    .into_iter()
    .map(|f| FingerprintInfo {
      algorithm: f.algorithm,
      value: f.value,
    })
    .collect();
  // A DOMTimeStamp: whole milliseconds.
  let expires = cert
    .expires
    .duration_since(std::time::SystemTime::UNIX_EPOCH)
    .map(|d| d.as_millis() as f64)
    .unwrap_or(0.0);
  let store = certificate_store(state);
  store.next += 1;
  let id = store.next;
  store.certs.insert(id, cert);
  Ok(CertificateInfo {
    id,
    expires,
    fingerprints,
  })
}

/// The DTLS key: ECDSA P-256 (what browsers default to), or RSASSA-PKCS1-v1_5
/// with SHA-256 and the public exponent 65537 (WebCrypto's algorithm
/// dictionaries, as `generateCertificate` takes them).
fn generate_certificate(
  alg: &KeygenArg,
  expires_at_ms: f64,
) -> Result<RTCCertificate, WebRtcError> {
  use rtci::crypto::SignatureScheme;
  let provider = rtci::crypto::default_provider()
    .map_err(|e| WebRtcError::Operation(e.to_string()))?;
  let name = alg.name.to_ascii_uppercase();
  let (scheme, signing_key) = match name.as_str() {
    "ECDSA" => {
      if alg.named_curve.as_deref() != Some("P-256") {
        return Err(WebRtcError::NotSupported(format!(
          "ECDSA curve {:?} is not supported (P-256 is)",
          alg.named_curve.as_deref().unwrap_or("")
        )));
      }
      let key = provider
        .crypto()
        .generate_signing_key(SignatureScheme::EcdsaP256Sha256)
        .map_err(|e| WebRtcError::Operation(e.to_string()))?;
      (SignatureScheme::EcdsaP256Sha256, key)
    }
    "RSASSA-PKCS1-V1_5" => {
      let hash = alg
        .hash
        .as_deref()
        .unwrap_or("SHA-256")
        .to_ascii_uppercase();
      if hash != "SHA-256" {
        return Err(WebRtcError::NotSupported(format!(
          "RSASSA-PKCS1-v1_5 with {hash} is not supported (SHA-256 is)"
        )));
      }
      let exponent = alg.public_exponent.as_deref().unwrap_or(&[1, 0, 1]);
      let trimmed: Vec<u8> =
        exponent.iter().copied().skip_while(|b| *b == 0).collect();
      if trimmed != [1, 0, 1] {
        return Err(WebRtcError::NotSupported(
          "Only the RSA public exponent 65537 is supported".to_string(),
        ));
      }
      let size = match alg.modulus_length.unwrap_or(2048) {
        2048 => aws_lc_rs::rsa::KeySize::Rsa2048,
        3072 => aws_lc_rs::rsa::KeySize::Rsa3072,
        4096 => aws_lc_rs::rsa::KeySize::Rsa4096,
        other => {
          return Err(WebRtcError::NotSupported(format!(
            "An RSA modulus of {other} bits is not supported (2048, 3072 or 4096 are)"
          )));
        }
      };
      let pair = aws_lc_rs::rsa::KeyPair::generate(size)
        .map_err(|e| WebRtcError::Operation(e.to_string()))?;
      let der = aws_lc_rs::encoding::AsDer::<
        aws_lc_rs::encoding::Pkcs8V1Der<'static>,
      >::as_der(&pair)
      .map_err(|e| WebRtcError::Operation(e.to_string()))?;
      let key = provider
        .crypto()
        .import_signing_key(SignatureScheme::RsaPkcs1Sha256, der.as_ref())
        .map_err(|e| WebRtcError::Operation(e.to_string()))?;
      (SignatureScheme::RsaPkcs1Sha256, key)
    }
    _ => {
      return Err(WebRtcError::NotSupported(format!(
        "'{}' is not a supported certificate algorithm",
        alg.name
      )));
    }
  };
  let params = CertificateParams::new(vec!["WebRTC".to_owned()])
    .map_err(|e| WebRtcError::Operation(e.to_string()))?;
  let mut cert =
    RTCCertificate::generate_from_signing_key(params, scheme, signing_key)
      .map_err(|e| WebRtcError::Operation(e.to_string()))?;
  // W3C `expires`: the certificate stops being usable for new connections
  // then (the X.509 validity is the engine's; DTLS peers check fingerprints,
  // not validity). `expires_at_ms` is a time on the JS clock (`Date.now()`
  // plus the `expires` the page asked for): the attribute, the constructor's
  // expiry check and the page all read that clock, which on Windows can lag
  // the system clock by a few milliseconds, so the instant is kept as given
  // (at most a year ahead) instead of being re-based on `SystemTime::now()`.
  let max_ms = std::time::SystemTime::now()
    .duration_since(std::time::SystemTime::UNIX_EPOCH)
    .map(|d| d.as_millis() as f64)
    .unwrap_or(0.0)
    + 365.0 * 24.0 * 3600.0 * 1000.0;
  let expires_at_ms = expires_at_ms.clamp(0.0, max_ms);
  cert.expires = std::time::SystemTime::UNIX_EPOCH
    + std::time::Duration::from_millis(expires_at_ms as u64);
  Ok(cert)
}

#[op2]
#[serde]
async fn op_webrtc_generate_certificate(
  state: Rc<RefCell<OpState>>,
  #[serde] alg: KeygenArg,
  expires_at_ms: f64,
) -> Result<CertificateInfo, WebRtcError> {
  let cert = tokio::task::spawn_blocking(move || {
    generate_certificate(&alg, expires_at_ms)
  })
  .await
  .map_err(|e| WebRtcError::Operation(e.to_string()))??;
  certificate_info(&mut state.borrow_mut(), cert)
}

/// `Deno.webrtc.exportCertificate`: the key and certificate as PEM, with
/// the `expires` time, to keep a server's DTLS fingerprint across restarts.
#[op2]
#[string]
fn op_webrtc_certificate_to_pem(
  state: &mut OpState,
  id: u32,
) -> Result<String, WebRtcError> {
  certificate(state, id)?
    .serialize_pem()
    .map_err(|e| WebRtcError::Operation(e.to_string()))
}

#[op2]
#[serde]
fn op_webrtc_certificate_from_pem(
  state: &mut OpState,
  #[string] pem: String,
) -> Result<CertificateInfo, WebRtcError> {
  let provider = rtci::crypto::default_provider()
    .map_err(|e| WebRtcError::Operation(e.to_string()))?;
  let cert =
    RTCCertificate::from_pem(&pem, provider.crypto()).map_err(|e| {
      WebRtcError::Syntax(format!("Not an exported RTCCertificate: {e}"))
    })?;
  certificate_info(state, cert)
}

// --- Media -----------------------------------------------------------------

#[op2]
#[serde]
fn op_webrtc_capabilities(
  #[string] kind: String,
  sender: bool,
) -> Result<Option<media::Capabilities>, WebRtcError> {
  media::capabilities(&kind, sender)
}

#[op2]
fn op_webrtc_pc_add_transceiver(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[serde] arg: media::TransceiverArg,
) -> Result<u32, WebRtcError> {
  peer(state, rid)?.media(|core| core.add_transceiver(arg))
}

#[op2]
fn op_webrtc_pc_attach_track(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
  #[serde] track: media::TrackArg,
  #[serde] streams: Vec<String>,
) -> Result<(), WebRtcError> {
  peer(state, rid)?.media(|core| core.attach_track(key, track, streams))
}

#[op2]
#[serde]
fn op_webrtc_pc_transceivers(
  state: &mut OpState,
  #[smi] rid: ResourceId,
) -> Result<Vec<media::TransceiverInfo>, WebRtcError> {
  let peer = peer(state, rid)?;
  if peer.is_closed() {
    return Ok(Vec::new());
  }
  peer.media(|core| Ok(core.transceivers()))
}

#[op2(fast)]
fn op_webrtc_transceiver_set_direction(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
  #[string] direction: String,
) -> Result<(), WebRtcError> {
  peer(state, rid)?.media(|core| core.set_direction(key, &direction))
}

#[op2(fast)]
fn op_webrtc_transceiver_stop(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
) -> Result<(), WebRtcError> {
  peer(state, rid)?.media(|core| core.stop_transceiver(key))
}

#[op2]
fn op_webrtc_transceiver_set_codec_preferences(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
  #[serde] codecs: Vec<media::CodecArg>,
) -> Result<(), WebRtcError> {
  peer(state, rid)?.media(|core| core.set_codec_preferences(key, codecs))
}

// wpt100:hdrext
#[op2]
fn op_webrtc_transceiver_set_header_extensions(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
  #[serde] uris: Vec<String>,
) -> Result<(), WebRtcError> {
  peer(state, rid)?
    .media(|core| core.set_header_extensions_to_negotiate(key, uris))
}

// wpt100:hdrext
#[op2(fast)]
fn op_webrtc_pc_set_negotiation_options(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] rtp_header_encryption_policy: String,
  always_negotiate_data_channels: bool,
) -> Result<(), WebRtcError> {
  peer(state, rid)?.media(|core| {
    core.set_negotiation_options(
      &rtp_header_encryption_policy,
      always_negotiate_data_channels,
    )
  })
}

/// The sender's new track is an `EncodedMediaTrack` of `codec` (what its
/// frames are in), or `null`: another track or none.
#[op2]
fn op_webrtc_sender_set_track_codec(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
  #[serde] codec: Option<media::CodecArg>,
) -> Result<(), WebRtcError> {
  peer(state, rid)?.media(|core| {
    core.set_sender_track_codec(key, codec);
    Ok(())
  })
}

#[op2(fast)]
fn op_webrtc_sender_set_paused(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
  paused: bool,
) -> Result<(), WebRtcError> {
  peer(state, rid)?.media(|core| {
    core.set_sender_paused(key, paused);
    Ok(())
  })
}

#[op2]
fn op_webrtc_sender_set_streams(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
  #[serde] streams: Vec<String>,
) -> Result<(), WebRtcError> {
  peer(state, rid)?.media(|core| core.set_streams(key, streams))
}

#[op2]
#[serde]
fn op_webrtc_sender_parameters(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
) -> Result<serde_json::Value, WebRtcError> {
  peer(state, rid)?.media(|core| core.sender_parameters(key))
}

#[op2]
#[serde]
fn op_webrtc_receiver_parameters(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
) -> Result<serde_json::Value, WebRtcError> {
  peer(state, rid)?.media(|core| core.receiver_parameters(key))
}

#[op2]
#[serde]
fn op_webrtc_receiver_sources(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
  contributing: bool,
) -> Result<Vec<serde_json::Value>, WebRtcError> {
  peer(state, rid)?.media(|core| core.receiver_sources(key, contributing))
}

#[op2(fast)]
fn op_webrtc_sender_write_rtp(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
  #[buffer] data: &[u8],
) -> Result<bool, WebRtcError> {
  peer(state, rid)?.media(|core| core.write_rtp(key, data))
}

#[op2(fast)]
fn op_webrtc_sender_write_frame(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
  #[buffer] data: &[u8],
  duration_ms: f64,
) -> Result<bool, WebRtcError> {
  peer(state, rid)?.media(|core| core.write_frame(key, data, duration_ms))
}

#[op2(fast)]
fn op_webrtc_sender_write_fake(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
  stereo: bool,
) -> Result<bool, WebRtcError> {
  peer(state, rid)?.media(|core| core.write_fake(key, stereo))
}

/// A fake device's next frame for a sender with an encoded transform (it
/// is written later with `op_webrtc_sender_write_frame_at`).
#[op2]
#[serde]
fn op_webrtc_sender_fake_frame(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
  stereo: bool,
) -> Result<Option<media::FakeFrame>, WebRtcError> {
  peer(state, rid)?.media(|core| core.fake_frame(key, stereo))
}

/// Takes the RTP timestamp of a sender's next frame, for an encoded
/// transform's frame; null before negotiation.
#[op2]
#[serde]
fn op_webrtc_sender_reserve_frame(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
  duration_ms: f64,
) -> Result<Option<media::SenderFrameInfo>, WebRtcError> {
  peer(state, rid)?.media(|core| core.reserve_frame(key, duration_ms))
}

/// Writes an encoded transform's frame at the RTP timestamp it reserved.
#[op2(fast)]
fn op_webrtc_sender_write_frame_at(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
  #[buffer] data: &[u8],
  duration_ms: f64,
  rtp_timestamp: u32,
) -> Result<bool, WebRtcError> {
  peer(state, rid)?.media(|core| {
    core.write_frame_at(key, data, duration_ms, Some(rtp_timestamp))
  })
}

/// `receiver.transform` set or cleared: the receiver's frames are
/// delivered as `encodedframe` events while it is set.
#[op2(fast)]
fn op_webrtc_receiver_transform(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
  on: bool,
) -> Result<(), WebRtcError> {
  peer(state, rid)?.media(|core| core.receiver_transform(key, on))
}

/// Whether `navigator.mediaDevices` has its fake devices
/// (`DENO_FAKE_MEDIA_DEVICES=1`).
#[op2(fast)]
fn op_webrtc_fake_media_devices() -> bool {
  fake_media::enabled()
}

#[op2(fast)]
fn op_webrtc_receiver_read_rtp(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
  on: bool,
) -> Result<(), WebRtcError> {
  peer(state, rid)?.media(|core| {
    if on {
      core.media.reading.insert(key);
    } else {
      core.media.reading.remove(&key);
    }
    Ok(())
  })
}

// wpt100:mediasink
/// `Deno.webrtc.readFrames()`: whether a receiver's frames are put together
/// and delivered (`mediaframe` events).
#[op2(fast)]
fn op_webrtc_receiver_read_frames(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
  on: bool,
) -> Result<(), WebRtcError> {
  peer(state, rid)?.media(|core| core.receiver_read_frames(key, on))
}

/// A receiver's track was muted (its remote side stopped sending): the next
/// RTP packet unmutes it again (`trackunmute`).
#[op2(fast)]
fn op_webrtc_receiver_muted(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
  generation: u32,
) -> Result<(), WebRtcError> {
  peer(state, rid)?.media(|core| {
    core.receiver_muted(key, generation);
    Ok(())
  })
}

/// `RTCDTMFSender`: one telephone event.
#[op2(fast)]
fn op_webrtc_sender_send_dtmf(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
  event: u32,
  duration_ms: u32,
) -> Result<bool, WebRtcError> {
  peer(state, rid)?
    .media(|core| core.send_dtmf(key, event.min(15) as u8, duration_ms))
}

#[op2(fast)]
fn op_webrtc_receiver_request_key_frame(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  key: u32,
) -> Result<bool, WebRtcError> {
  peer(state, rid)?.media(|core| core.request_key_frame(key))
}

/// `setConfiguration`: new ICE servers and policy, for the next gathering
/// (an ICE restart).
#[op2(stack_trace)]
fn op_webrtc_pc_set_ice_servers(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[serde] config: ConfigArg,
) -> Result<(), WebRtcError> {
  let mut permissions = state.borrow::<PermissionsContainer>().clone();
  let specs = ice_server_specs(&mut permissions, &config.ice_servers)?;
  let tls = if specs.iter().any(|s| s.transport == ServerTransport::Tls) {
    Some(tls_config(state)?)
  } else {
    None
  };
  peer(state, rid)?.set_ice_servers(
    specs,
    config.ice_transport_policy == "relay",
    tls,
  );
  Ok(())
}

/// The selected candidate pair's (local, remote) addresses, once the
/// connection has sent on one.
#[op2]
#[serde]
fn op_webrtc_pc_selected_pair(
  state: &mut OpState,
  #[smi] rid: ResourceId,
) -> Result<Option<(String, String, &'static str)>, WebRtcError> {
  let peer = peer(state, rid)?;
  let protocol = if peer.selected_is_tcp() { "tcp" } else { "udp" };
  Ok(
    peer
      .selected_pair()
      .map(|(l, r)| (l.to_string(), r.to_string(), protocol)),
  )
}

/// The ICE agent's selected candidate pair (W3C webrtc-pc 5.6
/// `getSelectedCandidatePair()`), with each candidate's fields; `None` until
/// ICE has selected one.
#[op2]
#[serde]
fn op_webrtc_pc_ice_selected_pair(
  state: &mut OpState,
  #[smi] rid: ResourceId,
) -> Result<Option<peer::SelectedPair>, WebRtcError> {
  let peer = peer(state, rid)?;
  Ok(peer.ice_selected_pair())
}

// --- RTCIceTransport (W3C webrtc-ice) ---------------------------------------
// wpt100:ice: the standalone transport of `new RTCIceTransport()`.

struct IceTransportResource(Rc<ice_transport::IceTransport>);

impl Resource for IceTransportResource {
  fn name(&self) -> std::borrow::Cow<'_, str> {
    "rtcIceTransport".into()
  }

  fn close(self: Rc<Self>) {
    self.0.stop();
  }
}

fn ice_transport(
  state: &OpState,
  rid: ResourceId,
) -> Result<Rc<ice_transport::IceTransport>, WebRtcError> {
  Ok(
    state
      .resource_table
      .get::<IceTransportResource>(rid)?
      .0
      .clone(),
  )
}

#[op2(fast)]
#[smi]
fn op_webrtc_ice_create(
  state: &mut OpState,
) -> Result<ResourceId, WebRtcError> {
  let permissions = state.borrow::<PermissionsContainer>().clone();
  let guard = Rc::new(PermissionsGuard(RefCell::new(permissions)));
  let transport = ice_transport::IceTransport::new(guard)?;
  Ok(state.resource_table.add(IceTransportResource(transport)))
}

/// `(usernameFragment, password)`.
#[op2]
#[serde]
fn op_webrtc_ice_local_parameters(
  state: &mut OpState,
  #[smi] rid: ResourceId,
) -> Result<(String, String), WebRtcError> {
  Ok(ice_transport(state, rid)?.local_parameters())
}

/// `gather()`; the ICE servers are validated by `ice_server_specs` (the
/// permission and syntax checks a connection makes) but not used yet.
#[op2]
fn op_webrtc_ice_gather(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[serde] ice_servers: Vec<IceServerArg>,
  relay_only: bool,
) -> Result<(), WebRtcError> {
  let transport = ice_transport(state, rid)?;
  let mut permissions = state.borrow::<PermissionsContainer>().clone();
  ice_server_specs(&mut permissions, &ice_servers)?;
  transport.gather(relay_only)
}

#[op2(fast)]
fn op_webrtc_ice_start(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] ufrag: String,
  #[string] pwd: String,
  controlling: bool,
) -> Result<(), WebRtcError> {
  ice_transport(state, rid)?.start(ufrag, pwd, controlling)
}

#[op2(fast)]
fn op_webrtc_ice_add_remote_candidate(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] candidate: String,
) -> Result<(), WebRtcError> {
  ice_transport(state, rid)?.add_remote_candidate(&candidate)
}

#[op2]
#[serde]
fn op_webrtc_ice_selected_pair(
  state: &mut OpState,
  #[smi] rid: ResourceId,
) -> Result<Option<peer::SelectedPair>, WebRtcError> {
  Ok(ice_transport(state, rid)?.selected_pair())
}

#[op2(fast)]
fn op_webrtc_ice_stop(state: &mut OpState, #[smi] rid: ResourceId) {
  if let Ok(resource) = state.resource_table.take::<IceTransportResource>(rid) {
    resource.0.stop();
  }
}

#[op2]
#[serde]
async fn op_webrtc_ice_next_events(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
) -> Result<Option<Vec<ice_transport::IceEvent>>, WebRtcError> {
  let transport = {
    let state = state.borrow();
    match state.resource_table.get::<IceTransportResource>(rid) {
      Ok(r) => r.0.clone(),
      Err(_) => return Ok(None),
    }
  };
  Ok(transport.next_events().await)
}

// --- Deno.webrtc.Network ---------------------------------------------------

struct NetworkResource(Rc<net::Network>);

impl Resource for NetworkResource {
  fn name(&self) -> std::borrow::Cow<'_, str> {
    "rtcNetwork".into()
  }

  fn close(self: Rc<Self>) {
    self.0.close();
  }
}

/// `Deno.webrtc.createNetwork(options)`. A `port` network binds its shared
/// sockets now (each bind needs the listen permission for its address).
#[op2(stack_trace)]
#[smi]
fn op_webrtc_network_create(
  state: &mut OpState,
  #[serde] arg: net::NetworkArg,
) -> Result<ResourceId, WebRtcError> {
  let options = net::NetworkOptions::parse(arg)?;
  let permissions = state.borrow::<PermissionsContainer>().clone();
  let guard = PermissionsGuard(RefCell::new(permissions));
  let network = net::Network::new(options, &guard)?;
  Ok(state.resource_table.add(NetworkResource(network)))
}

#[op2]
#[serde]
fn op_webrtc_network_addresses(
  state: &mut OpState,
  #[smi] rid: ResourceId,
) -> Result<Vec<String>, WebRtcError> {
  Ok(
    state
      .resource_table
      .get::<NetworkResource>(rid)?
      .0
      .addresses(),
  )
}

#[op2]
#[serde]
fn op_webrtc_network_tcp_addresses(
  state: &mut OpState,
  #[smi] rid: ResourceId,
) -> Result<Vec<String>, WebRtcError> {
  Ok(
    state
      .resource_table
      .get::<NetworkResource>(rid)?
      .0
      .tcp_addresses(),
  )
}

/// `Deno.webrtc.setLogLevel(level)`.
#[op2(fast)]
fn op_webrtc_set_log_level(#[string] level: &str) -> Result<(), WebRtcError> {
  set_engine_log_level(level)
}

#[op2(fast)]
fn op_webrtc_network_close(
  state: &mut OpState,
  #[smi] rid: ResourceId,
) -> Result<(), WebRtcError> {
  if let Ok(resource) = state.resource_table.take::<NetworkResource>(rid) {
    resource.0.close();
  }
  Ok(())
}

#[cfg(test)]
mod tests;
