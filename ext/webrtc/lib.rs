// Copyright 2018-2026 the Deno authors. MIT license.

//! WebRTC data channels: `RTCPeerConnection`, `RTCDataChannel`,
//! `RTCSessionDescription`, `RTCIceCandidate` and their events, as globals
//! (DENEXT.md item 370).
//!
//! The protocol stack (ICE, DTLS, SCTP, the data channel protocol) is
//! webrtc-rs's sans-I/O `rtc` engine; `peer.rs` owns the sockets, the
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
use std::net::IpAddr;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;

use deno_core::OpState;
use deno_core::Resource;
use deno_core::ResourceId;
use deno_core::op2;
use deno_net::DefaultTlsOptions;
use deno_net::UnsafelyIgnoreCertificateErrors;
use deno_permissions::PermissionsContainer;
use rtc::data_channel::RTCDataChannelInit;
use serde::Deserialize;
use serde::Serialize;

mod peer;

pub use peer::IceServerSpec;
pub use peer::JsEvent;
pub use peer::NetGuard;
pub use peer::Peer;
pub use peer::PeerOptions;
pub use peer::ServerTransport;

const API_NAME: &str = "RTCPeerConnection";

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
    op_webrtc_dc_buffered_amount,
    op_webrtc_dc_set_low_threshold,
    op_webrtc_dc_close,
  ],
  lazy_loaded_js = ["01_webrtc.js"],
);

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
}

#[op2(stack_trace)]
#[smi]
fn op_webrtc_pc_create(
  state: &mut OpState,
  #[serde] config: ConfigArg,
) -> Result<ResourceId, WebRtcError> {
  let mut specs = Vec::new();
  for server in &config.ice_servers {
    for url in &server.urls {
      specs.push(IceServerSpec::parse(
        url,
        server.username.as_deref(),
        server.credential.as_deref(),
      )?);
    }
  }
  let mut permissions = state.borrow::<PermissionsContainer>().clone();
  for spec in &specs {
    permissions
      .check_net_connect(&(spec.host.as_str(), Some(spec.port)), API_NAME)?;
  }
  let tls = if specs.iter().any(|s| s.transport == ServerTransport::Tls) {
    Some(tls_config(state)?)
  } else {
    None
  };
  let guard = Rc::new(PermissionsGuard(RefCell::new(permissions)));
  let peer = Peer::new(
    PeerOptions {
      ice_servers: specs,
      relay_only: config.ice_transport_policy == "relay",
      tls,
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

#[derive(Serialize)]
struct Descriptions {
  local: Option<(String, String)>,
  remote: Option<(String, String)>,
}

#[op2]
#[serde]
fn op_webrtc_pc_descriptions(
  state: &mut OpState,
  #[smi] rid: ResourceId,
) -> Result<Descriptions, WebRtcError> {
  let peer = peer(state, rid)?;
  Ok(Descriptions {
    local: peer.local_description(),
    remote: peer.remote_description(),
  })
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

#[op2]
#[serde]
fn op_webrtc_pc_get_stats(
  state: &mut OpState,
  #[smi] rid: ResourceId,
) -> Result<Vec<serde_json::Value>, WebRtcError> {
  peer(state, rid)?.stats()
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

/// `RTCDataChannel.bufferedAmount`: 0 once the connection is gone.
#[op2(fast)]
fn op_webrtc_dc_buffered_amount(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[smi] channel: u32,
) -> u32 {
  match state.resource_table.get::<PeerResource>(rid) {
    Ok(r) => r.0.buffered_amount(channel),
    Err(_) => 0,
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

#[cfg(test)]
mod tests;
