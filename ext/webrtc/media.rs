// Copyright 2018-2026 the Deno authors. MIT license.

//! Media: transceivers, senders and receivers over the engine's RTP stack
//! (SRTP, RTCP reports, NACK, TWCC), and the codec capabilities.
//!
//! There is no capture device and no codec here. The standard surface
//! (`addTransceiver`, `addTrack`, `getTransceivers`, `ontrack`, parameters,
//! capabilities, codec preferences, per-sender / per-receiver stats) works
//! without one; what goes out on a sender is what the program writes through
//! the `Deno.webrtc` extension (encoded frames, packetized here, or finished
//! RTP packets), and what a receiver gets is delivered as RTP packets to a
//! `Deno.webrtc.readRtp()` reader.
//!
//! The engine names transceivers by their index, which a rollback can shift;
//! [`Core::keys`](crate::peer::Core) maps each index to a stable key that
//! JavaScript uses instead.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::time::Instant;

use bytes::Bytes;
use rtc::interceptor::Attribute;
use rtc::interceptor::Interceptor;
use rtc::interceptor::Packet as InterceptorPacket;
use rtc::interceptor::Registry;
use rtc::interceptor::Slot;
use rtc::interceptor::StreamInfo;
use rtc::interceptor::TaggedPacket;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::RTCPeerConnectionBuilder;
use rtc::peer_connection::configuration::interceptor_registry::register_default_interceptors;
use rtc::peer_connection::configuration::media_engine::MediaEngine;
use rtc::rtcp::payload_feedbacks::full_intra_request::FullIntraRequest;
use rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use rtc::rtp::packetizer::Packetizer;
use rtc::rtp_transceiver::RTCRtpTransceiverDirection;
use rtc::rtp_transceiver::RTCRtpTransceiverInit;
use rtc::rtp_transceiver::rtp_sender::RTCRtpCodec;
use rtc::rtp_transceiver::rtp_sender::RTCRtpCodecParameters;
use rtc::rtp_transceiver::rtp_sender::RTCRtpCodingParameters;
use rtc::rtp_transceiver::rtp_sender::RTCRtpEncodingParameters;
use rtc::rtp_transceiver::rtp_sender::RtpCodecKind;
use rtc::sansio::Protocol;
use rtc::shared::marshal::Unmarshal;
use serde::Deserialize;
use serde::Serialize;

use crate::WebRtcError;
use crate::peer::Core;

/// The MTU RTP packets are cut to (what browsers use; leaves room for SRTP,
/// UDP, IP and a TURN header).
const RTP_MTU: usize = 1200;

/// The engine's media configuration: every codec browsers negotiate (Opus,
/// G.722, PCMU/PCMA, VP8, VP9, H.264, AV1, H.265, RTX, FEC) and the default
/// interceptors (NACK, RTCP sender/receiver reports, TWCC feedback,
/// simulcast header extensions).
pub fn media_stack() -> Result<(MediaEngine, Registry), WebRtcError> {
  let mut media_engine = MediaEngine::default();
  media_engine
    .register_default_codecs()
    .map_err(crate::peer::rtc_err)?;
  let registry =
    register_default_interceptors(Registry::new(), &mut media_engine)
      .map_err(crate::peer::rtc_err)?
      .with(
        Slot::Custom(KEY_FRAME_REQUESTS_SLOT),
        KeyFrameRequests::default(),
      );
  Ok((media_engine, registry))
}

/// Application-ward of every default interceptor (the jitter buffer is at
/// 13 000).
const KEY_FRAME_REQUESTS_SLOT: usize = 14_000;

/// Lets inbound key-frame requests (RTCP PLI and FIR) past the chain's
/// terminus, which ends every RTCP packet no interceptor vouches for: a
/// program feeding an `EncodedMediaTrack` is the encoder that has to act on
/// them (its `keyframerequest` event).
#[derive(Default)]
struct KeyFrameRequests {
  reads: VecDeque<TaggedPacket>,
  writes: VecDeque<TaggedPacket>,
}

impl Protocol<TaggedPacket, TaggedPacket, ()> for KeyFrameRequests {
  type Rout = TaggedPacket;
  type Wout = TaggedPacket;
  type Eout = ();
  type Error = rtc::shared::error::Error;
  type Time = Instant;

  fn handle_read(&mut self, mut msg: TaggedPacket) -> Result<(), Self::Error> {
    if let InterceptorPacket::Rtcp(packets) = &msg.message.packet
      && wants_key_frame(packets)
      && !msg.message.has(&Attribute::DeliverToApplication)
    {
      msg.message.attributes.push(Attribute::DeliverToApplication);
    }
    self.reads.push_back(msg);
    Ok(())
  }

  fn poll_read(&mut self) -> Option<TaggedPacket> {
    self.reads.pop_front()
  }

  fn handle_write(&mut self, msg: TaggedPacket) -> Result<(), Self::Error> {
    self.writes.push_back(msg);
    Ok(())
  }

  fn poll_write(&mut self) -> Option<TaggedPacket> {
    self.writes.pop_front()
  }
}

impl Interceptor for KeyFrameRequests {
  fn bind_local_stream(&mut self, _info: &StreamInfo) {}
  fn unbind_local_stream(&mut self, _info: &StreamInfo) {}
  fn bind_remote_stream(&mut self, _info: &StreamInfo) {}
  fn unbind_remote_stream(&mut self, _info: &StreamInfo) {}
}

pub fn parse_kind(kind: &str) -> Result<RtpCodecKind, WebRtcError> {
  match kind {
    "audio" => Ok(RtpCodecKind::Audio),
    "video" => Ok(RtpCodecKind::Video),
    other => Err(WebRtcError::Type(format!(
      "'{other}' is not a valid media kind"
    ))),
  }
}

fn kind_str(kind: RtpCodecKind) -> &'static str {
  match kind {
    RtpCodecKind::Audio => "audio",
    RtpCodecKind::Video => "video",
    _ => "",
  }
}

pub fn parse_direction(
  direction: &str,
) -> Result<RTCRtpTransceiverDirection, WebRtcError> {
  match RTCRtpTransceiverDirection::from(direction) {
    RTCRtpTransceiverDirection::Unspecified => Err(WebRtcError::Type(format!(
      "'{direction}' is not a valid RTCRtpTransceiverDirection"
    ))),
    d => Ok(d),
  }
}

fn direction_str(direction: RTCRtpTransceiverDirection) -> Option<String> {
  match direction {
    RTCRtpTransceiverDirection::Unspecified => None,
    d => Some(d.to_string()),
  }
}

/// `RTCRtpCodec` as JavaScript sees it.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CodecArg {
  pub mime_type: String,
  pub clock_rate: u32,
  #[serde(default)]
  pub channels: Option<u16>,
  #[serde(default)]
  pub sdp_fmtp_line: Option<String>,
}

impl CodecArg {
  fn from_codec(codec: &RTCRtpCodec) -> Self {
    Self {
      mime_type: codec.mime_type.clone(),
      clock_rate: codec.clock_rate,
      channels: (codec.channels > 0).then_some(codec.channels),
      sdp_fmtp_line: (!codec.sdp_fmtp_line.is_empty())
        .then(|| codec.sdp_fmtp_line.clone()),
    }
  }

  fn to_codec(&self) -> RTCRtpCodec {
    RTCRtpCodec {
      mime_type: self.mime_type.clone(),
      clock_rate: self.clock_rate,
      channels: self.channels.unwrap_or(0),
      sdp_fmtp_line: self.sdp_fmtp_line.clone().unwrap_or_default(),
      rtcp_feedback: vec![],
    }
  }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
  codecs: Vec<CodecArg>,
  header_extensions: Vec<HeaderExtensionCapability>,
}

#[derive(Serialize)]
pub struct HeaderExtensionCapability {
  uri: String,
}

/// `RTCRtpSender.getCapabilities(kind)` / `RTCRtpReceiver.getCapabilities`:
/// what the media stack offers, read from the offer of a connection that is
/// never used for anything else (the engine's codec lists are internal).
/// Repair formats (RTX, RED, FEC) are listed once, without the `apt` that
/// ties each RTX payload type to its primary, as browsers list them.
pub fn capabilities(
  kind: &str,
  _sender: bool,
) -> Result<Option<Capabilities>, WebRtcError> {
  let Ok(kind_enum) = parse_kind(kind) else {
    return Ok(None);
  };
  let (media_engine, registry) = media_stack()?;
  let mut pc = RTCPeerConnectionBuilder::new()
    .with_media_engine(media_engine)
    .with_interceptor_registry(registry)
    .build(Instant::now())
    .map_err(crate::peer::rtc_err)?;
  pc.add_transceiver_from_kind(
    kind_enum,
    Some(RTCRtpTransceiverInit {
      direction: RTCRtpTransceiverDirection::Sendrecv,
      streams: vec![],
      send_encodings: vec![RTCRtpEncodingParameters::default()],
    }),
  )
  .map_err(crate::peer::rtc_err)?;
  let offer = pc.create_offer(None).map_err(crate::peer::rtc_err)?;
  let _ = pc.close();
  Ok(Some(capabilities_from_sdp(kind, &offer.sdp)))
}

/// The codecs (`a=rtpmap` + `a=fmtp`) and header extensions (`a=extmap`)
/// of an SDP's media sections of `kind`.
pub fn capabilities_from_sdp(kind: &str, sdp: &str) -> Capabilities {
  let mut codecs: Vec<(String, CodecArg)> = Vec::new();
  let mut fmtp: HashMap<String, String> = HashMap::new();
  let mut header_extensions: Vec<HeaderExtensionCapability> = Vec::new();
  let mut in_kind = false;
  for line in sdp.lines() {
    let line = line.trim_end();
    if let Some(m) = line.strip_prefix("m=") {
      in_kind = m.split_ascii_whitespace().next() == Some(kind);
      continue;
    }
    if !in_kind {
      continue;
    }
    if let Some(value) = line.strip_prefix("a=rtpmap:") {
      let Some((pt, rest)) = value.split_once(' ') else {
        continue;
      };
      let mut parts = rest.split('/');
      let name = parts.next().unwrap_or_default();
      let clock_rate = parts.next().and_then(|c| c.parse().ok()).unwrap_or(0);
      let channels = parts.next().and_then(|c| c.parse().ok());
      codecs.push((
        pt.to_string(),
        CodecArg {
          mime_type: format!("{kind}/{name}"),
          clock_rate,
          channels,
          sdp_fmtp_line: None,
        },
      ));
    } else if let Some(value) = line.strip_prefix("a=fmtp:") {
      if let Some((pt, params)) = value.split_once(' ') {
        fmtp.insert(pt.to_string(), params.to_string());
      }
    } else if let Some(value) = line.strip_prefix("a=extmap:")
      && let Some(uri) = value.split_ascii_whitespace().nth(1)
      && !header_extensions.iter().any(|h| h.uri == uri)
    {
      header_extensions.push(HeaderExtensionCapability {
        uri: uri.to_string(),
      });
    }
  }
  let mut out: Vec<CodecArg> = Vec::new();
  for (pt, mut codec) in codecs {
    let lower = codec.mime_type.to_ascii_lowercase();
    let repair = lower.ends_with("/rtx")
      || lower.ends_with("/red")
      || lower.contains("fec");
    if !repair {
      codec.sdp_fmtp_line = fmtp.get(&pt).cloned();
    }
    let duplicate = out.iter().any(|c| {
      c.mime_type.eq_ignore_ascii_case(&codec.mime_type)
        && c.clock_rate == codec.clock_rate
        && c.channels == codec.channels
        && c.sdp_fmtp_line == codec.sdp_fmtp_line
    });
    if !duplicate {
      out.push(codec);
    }
  }
  Capabilities {
    codecs: out,
    header_extensions,
  }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackArg {
  pub id: String,
  pub label: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EncodingArg {
  #[serde(default)]
  pub rid: Option<String>,
  #[serde(default = "yes")]
  pub active: bool,
}

fn yes() -> bool {
  true
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransceiverArg {
  pub kind: String,
  pub track: Option<TrackArg>,
  pub direction: String,
  pub streams: Vec<String>,
  pub send_encodings: Vec<EncodingArg>,
  /// The `Deno.webrtc.EncodedMediaTrack` codec, which becomes the sender's
  /// preferred codec.
  pub codec: Option<CodecArg>,
}

/// One m-section of the remote description, by mid.
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSection {
  /// The remote side's direction attribute (`sendrecv` / `sendonly` /
  /// `recvonly` / `inactive`).
  pub direction: String,
  /// The `a=msid` stream ids (`-` dropped).
  pub streams: Vec<String>,
  /// Port 0: the section was rejected.
  pub rejected: bool,
  pub kind: String,
}

/// A transceiver as JavaScript reconciles it after every operation.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransceiverInfo {
  pub key: u32,
  pub kind: String,
  pub mid: Option<String>,
  pub direction: Option<String>,
  pub current_direction: Option<String>,
  pub remote: Option<RemoteSection>,
}

/// The m-sections of an SDP, keyed by mid.
pub fn sections(sdp: &str) -> HashMap<String, RemoteSection> {
  let mut out = HashMap::new();
  let mut current: Option<(Option<String>, RemoteSection)> = None;
  let mut flush = |current: &mut Option<(Option<String>, RemoteSection)>| {
    if let Some((Some(mid), section)) = current.take() {
      out.insert(mid, section);
    }
  };
  for line in sdp.lines() {
    let line = line.trim_end();
    if let Some(m) = line.strip_prefix("m=") {
      flush(&mut current);
      let mut fields = m.split_ascii_whitespace();
      let kind = fields.next().unwrap_or_default().to_string();
      let port = fields.next().unwrap_or_default();
      current = Some((
        None,
        RemoteSection {
          direction: "sendrecv".to_string(),
          streams: vec![],
          rejected: port == "0",
          kind,
        },
      ));
      continue;
    }
    let Some((mid, section)) = current.as_mut() else {
      continue;
    };
    let Some(attr) = line.strip_prefix("a=") else {
      continue;
    };
    if let Some(value) = attr.strip_prefix("mid:") {
      *mid = Some(value.trim().to_string());
    } else if let Some(value) = attr.strip_prefix("msid:") {
      if let Some(stream) = value.split_ascii_whitespace().next()
        && stream != "-"
        && !section.streams.iter().any(|s| s == stream)
      {
        section.streams.push(stream.to_string());
      }
    } else if matches!(attr, "sendrecv" | "sendonly" | "recvonly" | "inactive")
    {
      section.direction = attr.to_string();
    }
  }
  flush(&mut current);
  out
}

/// What a sender's frames are cut into RTP packets with, per SSRC.
pub struct SenderPacketizer {
  ssrc: u32,
  payload_type: u8,
  mime_type: String,
  clock_rate: u32,
  packetizer: Box<dyn Packetizer>,
}

fn payloader_for(
  mime: &str,
) -> Option<Box<dyn rtc::rtp::packetizer::Payloader>> {
  use rtc::rtp::codec;
  let mime = mime.to_ascii_lowercase();
  Some(match mime.as_str() {
    "audio/opus" => Box::<codec::opus::OpusPayloader>::default(),
    "audio/g722" | "audio/pcmu" | "audio/pcma" => {
      Box::<codec::g7xx::G7xxPayloader>::default()
    }
    "video/vp8" => Box::<codec::vp8::Vp8Payloader>::default(),
    "video/vp9" => Box::<codec::vp9::Vp9Payloader>::default(),
    "video/h264" => Box::<codec::h264::H264Payloader>::default(),
    "video/h265" => Box::<codec::h265::HevcPayloader>::default(),
    "video/av1" => Box::<codec::av1::Av1Payloader>::default(),
    _ => return None,
  })
}

/// RTP packets in, per transceiver key, while a reader wants them.
#[derive(Default)]
pub struct MediaState {
  /// Engine transceiver index -> stable key.
  pub keys: Vec<u32>,
  pub next_key: u32,
  /// Keys of transceivers the pending remote offer created (removed again by
  /// a rollback when they have no sender, as the engine does).
  pub created_by_remote: Vec<u32>,
  /// Kinds of the transceivers, by key.
  pub kinds: HashMap<u32, RtpCodecKind>,
  /// Engine track id -> transceiver key (receivers' and senders' tracks).
  pub track_keys: HashMap<String, u32>,
  /// Keys whose receiver's RTP is read from JavaScript.
  pub reading: std::collections::HashSet<u32>,
  /// Keys whose sender's track is `null` or that a `setParameters` made
  /// inactive: what is written to them is dropped.
  pub paused: std::collections::HashSet<u32>,
  pub packetizers: HashMap<u32, SenderPacketizer>,
  /// Keys whose receiver delivered its first packet (`unmute` was sent).
  pub flowing: std::collections::HashSet<u32>,
  /// The SSRCs and CSRCs each receiver got packets from, for
  /// `getSynchronizationSources()` / `getContributingSources()`.
  pub sources: HashMap<u32, Vec<Source>>,
}

/// A synchronization or contributing source a receiver heard from.
#[derive(Clone, Debug)]
pub struct Source {
  pub source: u32,
  pub contributing: bool,
  /// When its last packet arrived, in ms since the epoch.
  pub timestamp: f64,
  pub rtp_timestamp: u32,
}

/// Sources older than this are not reported (the spec's 10 seconds).
const SOURCE_LIFETIME_MS: f64 = 10_000.0;

fn epoch_ms() -> f64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_secs_f64() * 1000.0)
    .unwrap_or(0.0)
}

impl MediaState {
  /// Records the sources of a received RTP packet.
  pub fn saw_rtp(&mut self, key: u32, header: &rtc::rtp::Header) {
    let now = epoch_ms();
    let list = self.sources.entry(key).or_default();
    let mut upsert = |source: u32, contributing: bool| {
      match list
        .iter_mut()
        .find(|s| s.source == source && s.contributing == contributing)
      {
        Some(s) => {
          s.timestamp = now;
          s.rtp_timestamp = header.timestamp;
        }
        None => {
          // Bounded: a stream can't carry more than 15 CSRCs a packet,
          // and stale ones are dropped below.
          list.push(Source {
            source,
            contributing,
            timestamp: now,
            rtp_timestamp: header.timestamp,
          });
        }
      }
    };
    upsert(header.ssrc, false);
    for csrc in &header.csrc {
      upsert(*csrc, true);
    }
    list.retain(|s| now - s.timestamp <= SOURCE_LIFETIME_MS);
  }
}

impl MediaState {
  fn new_key(&mut self) -> u32 {
    self.next_key += 1;
    self.next_key
  }
}

fn random_id() -> String {
  use std::fmt::Write;
  let mut out = String::with_capacity(32);
  for _ in 0..16 {
    let _ = write!(out, "{:02x}", rand_byte());
  }
  out
}

fn rand_byte() -> u8 {
  let mut b = [0u8; 1];
  // A track id only needs to be unique, not secret.
  let _ = aws_lc_rs::rand::fill(&mut b);
  b[0]
}

impl Core {
  /// Gives every engine transceiver a key (new ones are appended by the
  /// engine; `from_remote` marks those a remote offer created).
  pub(crate) fn sync_transceiver_keys(&mut self, from_remote: bool) {
    let count = self.pc.get_transceivers().count();
    while self.media.keys.len() < count {
      let key = self.media.new_key();
      let index = self.media.keys.len();
      self.media.keys.push(key);
      if from_remote {
        self.media.created_by_remote.push(key);
      }
      let kind = self
        .pc
        .rtp_receiver(index.into())
        .map(|r| r.track().kind())
        .filter(|k| *k != RtpCodecKind::Unspecified)
        .or_else(|| self.pc.rtp_sender(index.into()).map(|s| s.track().kind()));
      if let Some(kind) = kind.filter(|k| *k != RtpCodecKind::Unspecified) {
        self.media.kinds.insert(key, kind);
      }
    }
  }

  /// Before a rollback: the keys the engine is about to remove (created by
  /// the pending remote offer, with no sender), in index order.
  pub(crate) fn rollback_victims(&mut self) -> Vec<usize> {
    let mut out = Vec::new();
    for (index, key) in self.media.keys.clone().iter().enumerate() {
      if self.media.created_by_remote.contains(key)
        && self.pc.rtp_sender(index.into()).is_none()
      {
        out.push(index);
      }
    }
    out
  }

  pub(crate) fn after_rollback(&mut self, victims: Vec<usize>) {
    for index in victims.into_iter().rev() {
      if index < self.media.keys.len() {
        let key = self.media.keys.remove(index);
        self.media.kinds.remove(&key);
        self.media.track_keys.retain(|_, k| *k != key);
      }
    }
    self.media.created_by_remote.clear();
    // Anything the engine still has beyond what we track gets a key.
    self.sync_transceiver_keys(false);
  }

  pub(crate) fn negotiation_settled(&mut self) {
    self.media.created_by_remote.clear();
  }

  fn index_of(&self, key: u32) -> Result<usize, WebRtcError> {
    self
      .media
      .keys
      .iter()
      .position(|k| *k == key)
      .ok_or_else(|| {
        WebRtcError::InvalidState("The transceiver is gone".to_string())
      })
  }

  pub(crate) fn add_transceiver(
    &mut self,
    arg: TransceiverArg,
  ) -> Result<u32, WebRtcError> {
    let kind = parse_kind(&arg.kind)?;
    let direction = parse_direction(&arg.direction)?;
    // Every W3C transceiver has a sender and a receiver: the engine's are
    // made by creating it sendrecv (a sender needs an encoding there), then
    // the asked-for direction is set.
    let mut encodings: Vec<RTCRtpEncodingParameters> = arg
      .send_encodings
      .iter()
      .map(|e| RTCRtpEncodingParameters {
        rtp_coding_parameters: RTCRtpCodingParameters {
          rid: e.rid.clone().unwrap_or_default(),
          ..Default::default()
        },
        active: e.active,
        codec: arg
          .codec
          .as_ref()
          .map(CodecArg::to_codec)
          .unwrap_or_default(),
        ..Default::default()
      })
      .collect();
    if encodings.is_empty() {
      encodings.push(RTCRtpEncodingParameters {
        active: true,
        codec: arg
          .codec
          .as_ref()
          .map(CodecArg::to_codec)
          .unwrap_or_default(),
        ..Default::default()
      });
    }
    let init = RTCRtpTransceiverInit {
      direction: RTCRtpTransceiverDirection::Sendrecv,
      streams: arg.streams.clone(),
      send_encodings: encodings.clone(),
    };
    let index = match &arg.track {
      Some(track) => {
        let stream = arg.streams.first().cloned().unwrap_or_else(random_id);
        let engine_track = MediaStreamTrack::new(
          stream,
          track.id.clone(),
          track.label.clone(),
          kind,
          encodings,
        );
        self
          .pc
          .add_transceiver_from_track(engine_track, Some(init))
          .map_err(crate::peer::rtc_err)?
      }
      None => self
        .pc
        .add_transceiver_from_kind(kind, Some(init))
        .map_err(crate::peer::rtc_err)?,
    };
    self.sync_transceiver_keys(false);
    let key = self.media.keys[index];
    self.media.kinds.insert(key, kind);
    if let Some(mut sender) = self.pc.rtp_sender(index.into()) {
      sender.set_streams(arg.streams);
      let id = sender.track().track_id().to_string();
      self.media.track_keys.insert(id, key);
    }
    if arg.track.is_none() {
      self.media.paused.insert(key);
    }
    if let Some(codec) = &arg.codec {
      let _ =
        self.set_codec_preferences_inner(index, std::slice::from_ref(codec));
    }
    if direction != RTCRtpTransceiverDirection::Sendrecv
      && let Some(mut t) = self.pc.rtp_transceiver(index)
    {
      t.set_direction(direction);
    }
    Ok(key)
  }

  /// `addTrack` onto a transceiver JavaScript chose for reuse: the sender
  /// takes the track (its id and streams go in the msid) and starts sending.
  pub(crate) fn attach_track(
    &mut self,
    key: u32,
    track: TrackArg,
    streams: Vec<String>,
  ) -> Result<(), WebRtcError> {
    let index = self.index_of(key)?;
    let kind = self.media.kinds.get(&key).copied().unwrap_or_default();
    let Some(mut sender) = self.pc.rtp_sender(index.into()) else {
      return Err(WebRtcError::InvalidState(
        "The transceiver has no sender".to_string(),
      ));
    };
    let codings = sender.track().codings().to_vec();
    let old = sender.track().track_id().to_string();
    let stream = streams.first().cloned().unwrap_or_else(random_id);
    sender
      .replace_track(MediaStreamTrack::new(
        stream,
        track.id.clone(),
        track.label,
        kind,
        codings,
      ))
      .map_err(crate::peer::rtc_err)?;
    sender.set_streams(streams);
    self.media.track_keys.remove(&old);
    self.media.track_keys.insert(track.id, key);
    self.media.paused.remove(&key);
    if let Some(mut t) = self.pc.rtp_transceiver(index) {
      let has_recv = t.direction().has_recv();
      t.set_direction(RTCRtpTransceiverDirection::from_send_recv(
        true, has_recv,
      ));
    }
    Ok(())
  }

  pub(crate) fn set_sender_paused(&mut self, key: u32, paused: bool) {
    if paused {
      self.media.paused.insert(key);
    } else {
      self.media.paused.remove(&key);
    }
  }

  pub(crate) fn set_streams(
    &mut self,
    key: u32,
    streams: Vec<String>,
  ) -> Result<(), WebRtcError> {
    let index = self.index_of(key)?;
    if let Some(mut sender) = self.pc.rtp_sender(index.into()) {
      sender.set_streams(streams);
    }
    Ok(())
  }

  pub(crate) fn set_direction(
    &mut self,
    key: u32,
    direction: &str,
  ) -> Result<(), WebRtcError> {
    let direction = parse_direction(direction)?;
    let index = self.index_of(key)?;
    if let Some(mut t) = self.pc.rtp_transceiver(index) {
      t.set_direction(direction);
    }
    Ok(())
  }

  pub(crate) fn stop_transceiver(
    &mut self,
    key: u32,
  ) -> Result<(), WebRtcError> {
    let index = self.index_of(key)?;
    self.media.paused.insert(key);
    self.media.reading.remove(&key);
    if let Some(mut t) = self.pc.rtp_transceiver(index) {
      t.stop().map_err(crate::peer::rtc_err)?;
    }
    Ok(())
  }

  fn set_codec_preferences_inner(
    &mut self,
    index: usize,
    codecs: &[CodecArg],
  ) -> Result<(), WebRtcError> {
    let kind = self
      .media
      .kinds
      .get(&self.media.keys[index])
      .copied()
      .unwrap_or_default();
    let params: Vec<RTCRtpCodecParameters> = codecs
      .iter()
      .map(|c| RTCRtpCodecParameters {
        rtp_codec: c.to_codec(),
        payload_type: 0,
      })
      .collect();
    if let Some(mut t) = self.pc.rtp_transceiver(index) {
      t.set_codec_preferences(params).map_err(|e| {
        WebRtcError::InvalidModification(format!(
          "setCodecPreferences: {e} ({} codec)",
          kind_str(kind)
        ))
      })?;
    }
    Ok(())
  }

  pub(crate) fn set_codec_preferences(
    &mut self,
    key: u32,
    codecs: Vec<CodecArg>,
  ) -> Result<(), WebRtcError> {
    let index = self.index_of(key)?;
    self.set_codec_preferences_inner(index, &codecs)
  }

  pub(crate) fn transceivers(&mut self) -> Vec<TransceiverInfo> {
    self.sync_transceiver_keys(false);
    let remote = self
      .pc
      .remote_description()
      .map(|d| sections(&d.sdp))
      .unwrap_or_default();
    let mut out = Vec::with_capacity(self.media.keys.len());
    for (index, key) in self.media.keys.clone().into_iter().enumerate() {
      let Some(t) = self.pc.rtp_transceiver(index) else {
        continue;
      };
      let mid = t.mid().clone();
      let direction = direction_str(t.direction());
      let current_direction = direction_str(t.current_direction());
      let section = mid.as_ref().and_then(|m| remote.get(m)).cloned();
      let kind = match self.media.kinds.get(&key) {
        Some(kind) => kind_str(*kind).to_string(),
        None => {
          let kind = section
            .as_ref()
            .and_then(|s| parse_kind(&s.kind).ok())
            .unwrap_or_default();
          if kind != RtpCodecKind::Unspecified {
            self.media.kinds.insert(key, kind);
          }
          kind_str(kind).to_string()
        }
      };
      // A receiver's engine track id reaches the RTP it delivers.
      if let Some(r) = self.pc.rtp_receiver(index.into()) {
        let id = r.track().track_id().to_string();
        if !id.is_empty() {
          self.media.track_keys.insert(id, key);
        }
      }
      out.push(TransceiverInfo {
        key,
        kind,
        mid,
        direction,
        current_direction,
        remote: section,
      });
    }
    out
  }

  pub(crate) fn sender_parameters(
    &mut self,
    key: u32,
  ) -> Result<serde_json::Value, WebRtcError> {
    let index = self.index_of(key)?;
    let Some(mut sender) = self.pc.rtp_sender(index.into()) else {
      return Ok(serde_json::Value::Null);
    };
    let params = sender.get_parameters().clone();
    Ok(serde_json::json!({
      "codecs": params.rtp_parameters.codecs.iter().map(codec_params_json).collect::<Vec<_>>(),
      "headerExtensions": params.rtp_parameters.header_extensions.iter().map(|h| serde_json::json!({
        "uri": h.uri, "id": h.id, "encrypted": h.encrypted,
      })).collect::<Vec<_>>(),
      "encodings": params.encodings.iter().map(|e| serde_json::json!({
        "rid": e.rtp_coding_parameters.rid,
        "ssrc": e.rtp_coding_parameters.ssrc,
      })).collect::<Vec<_>>(),
    }))
  }

  pub(crate) fn receiver_parameters(
    &mut self,
    key: u32,
  ) -> Result<serde_json::Value, WebRtcError> {
    let index = self.index_of(key)?;
    let Some(mut receiver) = self.pc.rtp_receiver(index.into()) else {
      return Ok(serde_json::Value::Null);
    };
    let params = receiver.get_parameters().clone();
    Ok(serde_json::json!({
      "codecs": params.rtp_parameters.codecs.iter().map(codec_params_json).collect::<Vec<_>>(),
      "headerExtensions": params.rtp_parameters.header_extensions.iter().map(|h| serde_json::json!({
        "uri": h.uri, "id": h.id, "encrypted": h.encrypted,
      })).collect::<Vec<_>>(),
    }))
  }

  /// `getSynchronizationSources()` / `getContributingSources()`: the
  /// sources heard in the last 10 seconds, most recent first, with
  /// timestamps on the wall clock as the spec has them (ms since the epoch).
  pub(crate) fn receiver_sources(
    &mut self,
    key: u32,
    contributing: bool,
  ) -> Result<Vec<serde_json::Value>, WebRtcError> {
    self.index_of(key)?;
    let now = epoch_ms();
    let mut list: Vec<Source> = self
      .media
      .sources
      .get(&key)
      .map(|l| {
        l.iter()
          .filter(|s| {
            s.contributing == contributing
              && now - s.timestamp <= SOURCE_LIFETIME_MS
          })
          .cloned()
          .collect()
      })
      .unwrap_or_default();
    list.sort_by(|a, b| b.timestamp.total_cmp(&a.timestamp));
    Ok(
      list
        .into_iter()
        .map(|s| {
          serde_json::json!({
            "timestamp": s.timestamp,
            "source": s.source,
            "rtpTimestamp": s.rtp_timestamp,
          })
        })
        .collect(),
    )
  }

  /// `Deno.webrtc` `writeRtp`: a finished RTP packet; the SSRC becomes the
  /// sender's (its first encoding's) and an unknown payload type the
  /// negotiated one.
  pub(crate) fn write_rtp(
    &mut self,
    key: u32,
    data: &[u8],
  ) -> Result<bool, WebRtcError> {
    if self.media.paused.contains(&key) {
      return Ok(false);
    }
    let index = self.index_of(key)?;
    let mut buf = data;
    let mut packet = rtc::rtp::packet::Packet::unmarshal(&mut buf)
      .map_err(|e| WebRtcError::Type(format!("Not an RTP packet: {e}")))?;
    let Some(mut sender) = self.pc.rtp_sender(index.into()) else {
      return Ok(false);
    };
    let Some(ssrc) = sender.track().ssrcs().next() else {
      return Ok(false);
    };
    packet.header.ssrc = ssrc;
    match sender.write_rtp(Instant::now(), packet) {
      Ok(()) => Ok(true),
      // Not negotiated (yet): dropped, as a sender that isn't sending
      // drops what its track produces.
      Err(_) => Ok(false),
    }
  }

  /// `Deno.webrtc` `writeFrame`: one encoded frame, cut into RTP packets
  /// for the negotiated codec; `samples` advances the RTP timestamp.
  pub(crate) fn write_frame(
    &mut self,
    key: u32,
    data: &[u8],
    duration_ms: f64,
  ) -> Result<bool, WebRtcError> {
    if self.media.paused.contains(&key) {
      return Ok(false);
    }
    let index = self.index_of(key)?;
    let now = Instant::now();
    let Some(mut sender) = self.pc.rtp_sender(index.into()) else {
      return Ok(false);
    };
    let Some(ssrc) = sender.track().ssrcs().next() else {
      return Ok(false);
    };
    let params = sender.get_parameters().clone();
    // The first negotiated codec that isn't a repair format.
    let Some(codec) = params.rtp_parameters.codecs.iter().find(|c| {
      let m = c.rtp_codec.mime_type.to_ascii_lowercase();
      !m.ends_with("/rtx") && !m.contains("fec") && !m.ends_with("/red")
    }) else {
      return Ok(false);
    };
    let needs_new = match self.media.packetizers.get(&key) {
      Some(p) => {
        p.ssrc != ssrc
          || p.payload_type != codec.payload_type
          || !p.mime_type.eq_ignore_ascii_case(&codec.rtp_codec.mime_type)
      }
      None => true,
    };
    if needs_new {
      let Some(payloader) = payloader_for(&codec.rtp_codec.mime_type) else {
        return Err(WebRtcError::NotSupported(format!(
          "writeFrame can't packetize {}; write RTP packets instead",
          codec.rtp_codec.mime_type
        )));
      };
      let packetizer = rtc::rtp::packetizer::new_packetizer(
        now,
        RTP_MTU,
        codec.payload_type,
        ssrc,
        payloader,
        Box::new(rtc::rtp::sequence::new_random_sequencer()),
        codec.rtp_codec.clock_rate,
      );
      self.media.packetizers.insert(
        key,
        SenderPacketizer {
          ssrc,
          payload_type: codec.payload_type,
          mime_type: codec.rtp_codec.mime_type.clone(),
          clock_rate: codec.rtp_codec.clock_rate,
          packetizer: Box::new(packetizer),
        },
      );
    }
    let p = self.media.packetizers.get_mut(&key).unwrap();
    let samples = ((duration_ms / 1000.0) * p.clock_rate as f64).round() as u32;
    let packets = p
      .packetizer
      .packetize(now, &Bytes::copy_from_slice(data), samples)
      .map_err(|e| WebRtcError::Type(format!("Could not packetize: {e}")))?;
    let Some(mut sender) = self.pc.rtp_sender(index.into()) else {
      return Ok(false);
    };
    let mut sent = false;
    for packet in packets {
      sent |= sender.write_rtp(now, packet).is_ok();
    }
    Ok(sent)
  }

  /// `Deno.webrtc` `requestKeyFrame`: a PLI to the remote sender.
  pub(crate) fn request_key_frame(
    &mut self,
    key: u32,
  ) -> Result<bool, WebRtcError> {
    let index = self.index_of(key)?;
    let Some(mut receiver) = self.pc.rtp_receiver(index.into()) else {
      return Ok(false);
    };
    let ssrcs: Vec<u32> = receiver.track().ssrcs().collect();
    let mut sent = false;
    for media_ssrc in ssrcs {
      sent |= receiver
        .write_rtcp(
          Instant::now(),
          vec![Box::new(PictureLossIndication {
            sender_ssrc: 0,
            media_ssrc,
          })],
        )
        .is_ok();
    }
    Ok(sent)
  }

  pub(crate) fn key_for_track(&mut self, track_id: &str) -> Option<u32> {
    if let Some(key) = self.media.track_keys.get(track_id) {
      return Some(*key);
    }
    for (index, key) in self.media.keys.clone().into_iter().enumerate() {
      if let Some(r) = self.pc.rtp_receiver(index.into())
        && r.track().track_id() == track_id
      {
        self.media.track_keys.insert(track_id.to_string(), key);
        return Some(key);
      }
    }
    None
  }

  pub(crate) fn key_for_receiver_index(&self, index: usize) -> Option<u32> {
    self.media.keys.get(index).copied()
  }
}

/// Whether an RTCP compound asks for a key frame (PLI or FIR).
pub fn wants_key_frame(packets: &[Box<dyn rtc::rtcp::Packet>]) -> bool {
  packets.iter().any(|p| {
    p.as_any().downcast_ref::<PictureLossIndication>().is_some()
      || p.as_any().downcast_ref::<FullIntraRequest>().is_some()
  })
}

fn codec_params_json(c: &RTCRtpCodecParameters) -> serde_json::Value {
  let mut v = serde_json::to_value(CodecArg::from_codec(&c.rtp_codec))
    .unwrap_or(serde_json::Value::Null);
  if let Some(obj) = v.as_object_mut() {
    obj.insert("payloadType".to_string(), c.payload_type.into());
  }
  v
}
