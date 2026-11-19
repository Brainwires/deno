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
use std::time::Instant;

use bytes::Bytes;
use rtc::interceptor::Registry;
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
  // RFC 4733 telephone events, for RTCDTMFSender (the engine registers
  // none): at Opus' clock and at 8 kHz, on payload types the default video
  // codecs don't use (they share the bundle).
  for (payload_type, clock_rate) in [(110, 48000), (113, 8000)] {
    media_engine
      .register_codec(
        RTCRtpCodecParameters {
          rtp_codec: RTCRtpCodec {
            mime_type: "audio/telephone-event".to_string(),
            clock_rate,
            channels: 0,
            sdp_fmtp_line: "0-15".to_string(),
            rtcp_feedback: vec![],
          },
          payload_type,
        },
        RtpCodecKind::Audio,
      )
      .map_err(crate::peer::rtc_err)?;
  }
  // The defaults include the fork's key-frame request interceptor, which
  // delivers inbound PLI and FIR to the application (here, an
  // `EncodedMediaTrack`'s `keyframerequest`): upstream ends them at the
  // chain's terminus.
  let registry =
    register_default_interceptors(Registry::new(), &mut media_engine)
      .map_err(crate::peer::rtc_err)?;
  Ok((media_engine, registry))
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

/// `RTCRtpCodec` as JavaScript sees it: a dictionary, so a member the codec
/// doesn't have (a video codec's `channels`, an absent fmtp line) is left
/// out rather than `null`.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CodecArg {
  pub mime_type: String,
  pub clock_rate: u32,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub channels: Option<u16>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub sdp_fmtp_line: Option<String>,
}

impl CodecArg {
  fn from_codec(codec: &RTCRtpCodec) -> Self {
    let audio = codec.mime_type.to_ascii_lowercase().starts_with("audio/");
    Self {
      mime_type: codec.mime_type.clone(),
      clock_rate: codec.clock_rate,
      // An audio codec without a channel count is mono (RFC 4566's
      // default for the rtpmap encoding parameters).
      channels: if codec.channels > 0 {
        Some(codec.channels)
      } else {
        audio.then_some(1)
      },
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
      let channels = parts
        .next()
        .and_then(|c| c.parse().ok())
        .or((kind == "audio").then_some(1));
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
  /// `addTrack` made the transceiver (not `addTransceiver`): a remote
  /// offer's new media section of the same kind may take it.
  #[serde(default)]
  pub add_track: bool,
}

/// The stream ids of a sender's `a=msid` lines: the `MediaStream`s it is in,
/// or `-` for none (RFC 9429 section 5.2.1).
fn msid_streams(streams: Vec<String>) -> Vec<String> {
  if streams.is_empty() {
    vec!["-".to_string()]
  } else {
    streams
  }
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

/// The id of the stream a remote track with no `a=msid` is in.
const DEFAULT_STREAM_ID: &str = "default";

/// The m-sections of an SDP, keyed by mid.
pub fn sections(sdp: &str) -> HashMap<String, RemoteSection> {
  let mut out = HashMap::new();
  let mut current: Option<(Option<String>, RemoteSection)> = None;
  // Whether the current section has an `a=msid` line at all.
  let mut has_msid = false;
  let mut flush = |current: &mut Option<(Option<String>, RemoteSection)>,
                   has_msid: bool| {
    if let Some((Some(mid), mut section)) = current.take() {
      // A media section with no `a=msid` at all (an endpoint that predates
      // it): its track is in one default stream (RFC 9429 section 5.10; the
      // id `-` means "no stream" and stays none).
      if !has_msid && (section.kind == "audio" || section.kind == "video") {
        section.streams.push(DEFAULT_STREAM_ID.to_string());
      }
      out.insert(mid, section);
    }
  };
  for line in sdp.lines() {
    let line = line.trim_end();
    if let Some(m) = line.strip_prefix("m=") {
      flush(&mut current, has_msid);
      has_msid = false;
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
      has_msid = true;
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
  flush(&mut current, has_msid);
  out
}

/// The codec preferences an `EncodedMediaTrack` gives its transceiver: its
/// codec, then (for audio) the telephone-event formats `RTCDTMFSender`
/// needs, which a preference list would otherwise leave out.
pub fn track_preferences(codec: &CodecArg) -> Vec<CodecArg> {
  let mut out = vec![codec.clone()];
  if codec.mime_type.to_ascii_lowercase().starts_with("audio/") {
    for clock_rate in [48000, 8000] {
      out.push(CodecArg {
        mime_type: "audio/telephone-event".to_string(),
        clock_rate,
        channels: None,
        sdp_fmtp_line: Some("0-15".to_string()),
      });
    }
  }
  out
}

/// What a sender's frames are cut into RTP packets with, per SSRC.
pub struct SenderPacketizer {
  ssrc: u32,
  payload_type: u8,
  mime_type: String,
  clock_rate: u32,
  /// The next frame's RTP timestamp.
  timestamp: u32,
  packetizer: Box<dyn Packetizer>,
}

/// What an encoded frame's metadata names about its sender.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SenderFrameInfo {
  pub synchronization_source: u32,
  pub payload_type: u8,
  pub mime_type: String,
  pub rtp_timestamp: u32,
}

/// A fake capture device's frame, for an encoded transform.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FakeFrame {
  pub data: deno_core::ToJsBuffer,
  pub duration_ms: f64,
  pub key_frame: bool,
  pub width_height: Option<(u16, u16)>,
  pub info: SenderFrameInfo,
}

/// Whether an encoded video frame is a key frame (audio frames are not
/// typed): from its bitstream.
pub fn is_key_frame(mime_type: &str, data: &[u8]) -> bool {
  match mime_type.to_ascii_lowercase().as_str() {
    // RFC 6386 9.1: bit 0 of the frame tag is 0 on a key frame.
    "video/vp8" => data.first().is_some_and(|b| b & 1 == 0),
    // VP9 uncompressed header: frame_marker (2), profile (2, a reserved
    // zero bit for profile 3), show_existing_frame, then frame_type (0 =
    // key).
    "video/vp9" => data.first().is_some_and(|b| {
      let profile = ((b >> 5) & 1) | (((b >> 4) & 1) << 1);
      let shift = if profile == 3 { 1 } else { 0 };
      (b >> (3 - shift)) & 1 == 0 && (b >> (2 - shift)) & 1 == 0
    }),
    "video/h264" => {
      nal_units(data).any(|n| n.first().is_some_and(|b| b & 0x1f == 5))
    }
    "video/h265" => nal_units(data).any(|n| {
      n.first()
        .is_some_and(|b| (16..=23).contains(&((b >> 1) & 0x3f)))
    }),
    "video/av1" => {
      // A sequence header OBU (type 1) starts every key frame's temporal
      // unit.
      data.iter().take(16).any(|b| (b >> 3) & 0xf == 1)
    }
    _ => false,
  }
}

/// The NAL units of an Annex B stream.
fn nal_units(data: &[u8]) -> impl Iterator<Item = &[u8]> {
  let mut starts = Vec::new();
  let mut i = 0;
  while i + 3 <= data.len() {
    if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
      starts.push(i + 3);
      i += 3;
    } else {
      i += 1;
    }
  }
  let ends: Vec<usize> = starts
    .iter()
    .skip(1)
    .map(|s| s - 3)
    .chain(std::iter::once(data.len()))
    .collect();
  starts
    .into_iter()
    .zip(ends)
    .map(move |(a, b)| &data[a..b.max(a)])
}

/// A key frame's size, where its bitstream says it simply (VP8).
pub fn frame_size(mime_type: &str, data: &[u8]) -> Option<(u16, u16)> {
  if !mime_type.eq_ignore_ascii_case("video/vp8")
    || data.len() < 10
    || data[0] & 1 != 0
    || data[3..6] != [0x9d, 0x01, 0x2a]
  {
    return None;
  }
  let w = u16::from_le_bytes([data[6], data[7]]) & 0x3fff;
  let h = u16::from_le_bytes([data[8], data[9]]) & 0x3fff;
  Some((w, h))
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
  /// RFC 4733 state per sender: the next sequence number and timestamp.
  pub dtmf: HashMap<u32, (u16, u32)>,
  /// The SSRCs and CSRCs each receiver got packets from, for
  /// `getSynchronizationSources()` / `getContributingSources()`.
  pub sources: HashMap<u32, Vec<Source>>,
  /// Where each sender fed by a fake capture device is in its clip.
  pub fake: HashMap<u32, crate::fake_media::Cursor>,
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
      // The engine removes what the offer created unless `addTrack` touched
      // it (attach_track drops the key from created_by_remote).
      if self.media.created_by_remote.contains(key)
        && self.pc.rtp_transceiver(index).is_some()
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

  pub(crate) fn index_of(&self, key: u32) -> Result<usize, WebRtcError> {
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
        let stream = msid_streams(arg.streams.clone()).remove(0);
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
      sender.set_streams(msid_streams(arg.streams));
      let id = sender.track().track_id().to_string();
      self.media.track_keys.insert(id, key);
    }
    // RFC 9429 section 5.10: a remote offer's new media section takes a
    // transceiver `addTrack` created, never one `addTransceiver` did.
    if !arg.add_track
      && let Some(mut t) = self.pc.rtp_transceiver(index)
    {
      t.set_associable_with_remote_offer(false);
    }
    if arg.track.is_none() {
      self.media.paused.insert(key);
    }
    if let Some(codec) = &arg.codec {
      let _ =
        self.set_codec_preferences_inner(index, &track_preferences(codec));
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
  /// A transceiver a remote offer created has no engine sender until now; the
  /// engine hands out a sender only while the direction sends, so the
  /// direction gains `send` first. One `addTransceiver` created stays out of a
  /// remote offer's new m-sections (WPT checkAddTransceiverThenAddTrackPairs:
  /// browsers keep it unassociable), and `addTrack` keeps a transceiver a
  /// rolled-back remote offer created (W3C 4.4.1.6).
  pub(crate) fn attach_track(
    &mut self,
    key: u32,
    track: TrackArg,
    streams: Vec<String>,
  ) -> Result<(), WebRtcError> {
    let index = self.index_of(key)?;
    let kind = self.media.kinds.get(&key).copied().unwrap_or_default();
    let streams = msid_streams(streams);
    let stream = streams[0].clone();
    if let Some(mut t) = self.pc.rtp_transceiver(index) {
      let has_recv = t.direction().has_recv();
      t.set_direction(RTCRtpTransceiverDirection::from_send_recv(
        true, has_recv,
      ));
      t.keep_on_rollback();
    }
    self.media.created_by_remote.retain(|k| *k != key);
    let has_sender = self
      .pc
      .rtp_transceiver(index)
      .is_some_and(|t| t.sender().is_some());
    if !has_sender {
      self
        .pc
        .add_track_to_transceiver(
          index,
          MediaStreamTrack::new(
            stream,
            track.id.clone(),
            track.label,
            kind,
            vec![RTCRtpEncodingParameters {
              active: true,
              ..Default::default()
            }],
          ),
        )
        .map_err(crate::peer::rtc_err)?;
      if let Some(mut sender) = self.pc.rtp_sender(index.into()) {
        sender.set_streams(streams);
      }
      self.media.track_keys.insert(track.id, key);
      self.media.paused.remove(&key);
      return Ok(());
    }
    let Some(mut sender) = self.pc.rtp_sender(index.into()) else {
      return Err(WebRtcError::InvalidState(
        "The transceiver has no sender".to_string(),
      ));
    };
    let codings = sender.track().codings().to_vec();
    let old = sender.track().track_id().to_string();
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
    Ok(())
  }

  /// A transceiver a remote offer created has no engine sender. Made to send
  /// (a sending direction, or `replaceTrack` with a track) without `addTrack`,
  /// it gets one: SSRCs in the answer, an outbound stream. A rollback still
  /// removes it (only `addTrack` keeps it).
  fn ensure_sender(&mut self, key: u32) {
    if let Ok(index) = self.index_of(key) {
      let _ = self.pc.ensure_transceiver_sender(index);
    }
  }

  pub(crate) fn set_sender_paused(&mut self, key: u32, paused: bool) {
    if paused {
      self.media.paused.insert(key);
    } else {
      self.ensure_sender(key);
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
      sender.set_streams(msid_streams(streams));
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
    if direction.has_send() {
      self.ensure_sender(key);
    }
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
    self.write_frame_at(key, data, duration_ms, None)
  }

  /// The packetizer of a sender's negotiated codec (the first that isn't a
  /// repair format), made anew when the codec or SSRC changed; `None` before
  /// negotiation or while the sender is paused.
  fn sender_packetizer(
    &mut self,
    key: u32,
  ) -> Result<Option<&mut SenderPacketizer>, WebRtcError> {
    if self.media.paused.contains(&key) {
      return Ok(None);
    }
    let index = self.index_of(key)?;
    let now = Instant::now();
    let Some(mut sender) = self.pc.rtp_sender(index.into()) else {
      return Ok(None);
    };
    let Some(ssrc) = sender.track().ssrcs().next() else {
      return Ok(None);
    };
    let params = sender.get_parameters().clone();
    let Some(codec) = params.rtp_parameters.codecs.iter().find(|c| {
      let m = c.rtp_codec.mime_type.to_ascii_lowercase();
      !m.ends_with("/rtx") && !m.contains("fec") && !m.ends_with("/red")
    }) else {
      return Ok(None);
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
      let mut start = [0u8; 4];
      let _ = aws_lc_rs::rand::fill(&mut start);
      self.media.packetizers.insert(
        key,
        SenderPacketizer {
          ssrc,
          payload_type: codec.payload_type,
          mime_type: codec.rtp_codec.mime_type.clone(),
          clock_rate: codec.rtp_codec.clock_rate,
          timestamp: u32::from_le_bytes(start),
          packetizer: Box::new(packetizer),
        },
      );
    }
    Ok(self.media.packetizers.get_mut(&key))
  }

  /// Takes the RTP timestamp of a sender's next frame of `duration_ms`
  /// (an encoded transform's frame, written later with
  /// [`write_frame_at`](Self::write_frame_at)), with what its metadata
  /// names: `None` before negotiation.
  pub(crate) fn reserve_frame(
    &mut self,
    key: u32,
    duration_ms: f64,
  ) -> Result<Option<SenderFrameInfo>, WebRtcError> {
    let Some(p) = self.sender_packetizer(key)? else {
      return Ok(None);
    };
    let samples = ((duration_ms / 1000.0) * p.clock_rate as f64).round() as u32;
    let rtp_timestamp = p.timestamp;
    p.timestamp = p.timestamp.wrapping_add(samples);
    Ok(Some(SenderFrameInfo {
      synchronization_source: p.ssrc,
      payload_type: p.payload_type,
      mime_type: p.mime_type.clone(),
      rtp_timestamp,
    }))
  }

  /// Writes one encoded frame on a sender, cut into RTP packets: at the next
  /// timestamp, or at `rtp_timestamp` (one [`reserve_frame`](Self::reserve_frame)
  /// took). Returns whether it went out.
  pub(crate) fn write_frame_at(
    &mut self,
    key: u32,
    data: &[u8],
    duration_ms: f64,
    rtp_timestamp: Option<u32>,
  ) -> Result<bool, WebRtcError> {
    let index = self.index_of(key)?;
    let now = Instant::now();
    let Some(p) = self.sender_packetizer(key)? else {
      return Ok(false);
    };
    let samples = ((duration_ms / 1000.0) * p.clock_rate as f64).round() as u32;
    let timestamp = match rtp_timestamp {
      Some(t) => t,
      None => {
        let t = p.timestamp;
        p.timestamp = p.timestamp.wrapping_add(samples);
        t
      }
    };
    let mut packets = p
      .packetizer
      .packetize(now, &Bytes::copy_from_slice(data), samples)
      .map_err(|e| WebRtcError::Type(format!("Could not packetize: {e}")))?;
    // The timestamps are ours, so a frame's metadata can name its own.
    for packet in &mut packets {
      packet.header.timestamp = timestamp;
    }
    let Some(mut sender) = self.pc.rtp_sender(index.into()) else {
      return Ok(false);
    };
    let mut sent = false;
    for packet in packets {
      sent |= sender.write_rtp(now, packet).is_ok();
    }
    Ok(sent)
  }

  /// A fake capture device's next frame for a sender (one with an encoded
  /// transform, which writes it later), in the codec it negotiated: its
  /// bytes, duration and whether it is a key frame, and the frame's
  /// metadata. `None` before negotiation or for a codec there is no clip of.
  pub(crate) fn fake_frame(
    &mut self,
    key: u32,
    stereo: bool,
  ) -> Result<Option<FakeFrame>, WebRtcError> {
    let Some(mime) = self.sender_packetizer(key)?.map(|p| p.mime_type.clone())
    else {
      return Ok(None);
    };
    let cursor = self.media.fake.entry(key).or_default();
    let Some((data, duration_ms)) = cursor.advance(&mime, stereo) else {
      return Ok(None);
    };
    let Some(info) = self.reserve_frame(key, duration_ms)? else {
      return Ok(None);
    };
    Ok(Some(FakeFrame {
      data: data.to_vec().into(),
      duration_ms,
      key_frame: is_key_frame(&mime, data),
      width_height: frame_size(&mime, data),
      info,
    }))
  }

  /// A fake capture device's next frame (`fake_media.rs`) on a sender, in
  /// the codec it negotiated; `stereo` takes Opus' two-channel clip.
  /// Returns whether it went out (false before negotiation, or for a codec
  /// there is no clip of).
  pub(crate) fn write_fake(
    &mut self,
    key: u32,
    stereo: bool,
  ) -> Result<bool, WebRtcError> {
    if self.media.paused.contains(&key) {
      return Ok(false);
    }
    let index = self.index_of(key)?;
    let Some(mut sender) = self.pc.rtp_sender(index.into()) else {
      return Ok(false);
    };
    let Some(mime) = sender
      .get_parameters()
      .rtp_parameters
      .codecs
      .iter()
      .map(|c| c.rtp_codec.mime_type.clone())
      .find(|m| {
        let m = m.to_ascii_lowercase();
        !m.ends_with("/rtx") && !m.contains("fec") && !m.ends_with("/red")
      })
    else {
      return Ok(false);
    };
    let cursor = self.media.fake.entry(key).or_default();
    let Some((frame, duration)) = cursor.advance(&mime, stereo) else {
      return Ok(false);
    };
    self.write_frame(key, frame, duration)
  }

  /// `RTCDTMFSender`: one RFC 4733 event (0-9 → 0-9, `*` 10, `#` 11, A-D
  /// 12-15) lasting `duration_ms`: a start packet (marker set) and the end
  /// packet three times, on the negotiated telephone-event payload type at
  /// the clock of the sender's audio codec. Returns whether it went out.
  pub(crate) fn send_dtmf(
    &mut self,
    key: u32,
    event: u8,
    duration_ms: u32,
  ) -> Result<bool, WebRtcError> {
    let index = self.index_of(key)?;
    let Some(mut sender) = self.pc.rtp_sender(index.into()) else {
      return Ok(false);
    };
    let Some(ssrc) = sender.track().ssrcs().next() else {
      return Ok(false);
    };
    let params = sender.get_parameters().clone();
    let codecs = &params.rtp_parameters.codecs;
    let primary_rate = codecs
      .iter()
      .find(|c| {
        let m = c.rtp_codec.mime_type.to_ascii_lowercase();
        !m.ends_with("/telephone-event")
          && !m.ends_with("/cn")
          && !m.ends_with("/red")
          && !m.ends_with("/rtx")
      })
      .map(|c| c.rtp_codec.clock_rate);
    let events: Vec<&RTCRtpCodecParameters> = codecs
      .iter()
      .filter(|c| {
        c.rtp_codec
          .mime_type
          .eq_ignore_ascii_case("audio/telephone-event")
      })
      .collect();
    let Some(codec) = events
      .iter()
      .find(|c| Some(c.rtp_codec.clock_rate) == primary_rate)
      .or_else(|| events.first())
    else {
      return Ok(false);
    };
    let (payload_type, clock_rate) =
      (codec.payload_type, codec.rtp_codec.clock_rate);
    let entry = self.media.dtmf.entry(key).or_insert_with(|| {
      let mut seed = [0u8; 6];
      let _ = aws_lc_rs::rand::fill(&mut seed);
      (
        u16::from_le_bytes([seed[0], seed[1]]),
        u32::from_le_bytes([seed[2], seed[3], seed[4], seed[5]]),
      )
    });
    let (mut seq, timestamp) = *entry;
    let samples = (u64::from(duration_ms) * u64::from(clock_rate) / 1000)
      .min(u64::from(u16::MAX)) as u16;
    let now = Instant::now();
    let packet = |seq: u16, end: bool, marker: bool, duration: u16| {
      let flags = if end { 0x80 } else { 0x00 } | 10; // volume -10 dBm0
      let d = duration.to_be_bytes();
      rtc::rtp::packet::Packet {
        header: rtc::rtp::Header {
          version: 2,
          marker,
          payload_type,
          sequence_number: seq,
          timestamp,
          ssrc,
          ..Default::default()
        },
        payload: Bytes::copy_from_slice(&[event, flags, d[0], d[1]]),
      }
    };
    let mut sent = false;
    let Some(mut sender) = self.pc.rtp_sender(index.into()) else {
      return Ok(false);
    };
    sent |= sender.write_rtp(now, packet(seq, false, true, 0)).is_ok();
    for _ in 0..3 {
      seq = seq.wrapping_add(1);
      sent |= sender
        .write_rtp(now, packet(seq, true, false, samples))
        .is_ok();
    }
    *self.media.dtmf.get_mut(&key).unwrap() = (
      seq.wrapping_add(1),
      timestamp.wrapping_add(u32::from(samples)),
    );
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
