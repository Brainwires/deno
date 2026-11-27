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
//! The engine names transceivers by their index, which a removal (a rollback,
//! or an answer rejecting a stopped transceiver's section) shifts;
//! [`Core::keys`](crate::peer::Core) maps each index to a stable key that
//! JavaScript uses instead.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Instant;

use bytes::Bytes;
use rtci::interceptor::Attribute;
use rtci::interceptor::AttributedPacket;
use rtci::interceptor::CONGESTION_CONTROL_DEFAULT_PRUNE_HORIZON;
use rtci::interceptor::GCC_INITIAL_BITRATE;
use rtci::interceptor::Gcc;
use rtci::interceptor::Interceptor;
use rtci::interceptor::PacerBuilder;
use rtci::interceptor::PacerInterceptor;
use rtci::interceptor::Packet;
use rtci::interceptor::Registry;
use rtci::interceptor::Slot;
use rtci::interceptor::StreamInfo;
use rtci::interceptor::TaggedPacket;
use rtci::media_stream::MediaStreamTrack;
use rtci::peer_connection::RTCPeerConnectionBuilder;
use rtci::peer_connection::configuration::interceptor_registry::CongestionFeedback;
use rtci::peer_connection::configuration::interceptor_registry::configure_congestion_control;
use rtci::peer_connection::configuration::interceptor_registry::configure_congestion_control_feedback;
use rtci::peer_connection::configuration::interceptor_registry::register_default_interceptors;
use rtci::peer_connection::configuration::media_engine::MediaEngine;
use rtci::peer_connection::cryptex::RTCRtpHeaderEncryptionPolicy;
use rtci::rtcp::payload_feedbacks::full_intra_request::FullIntraRequest;
use rtci::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use rtci::rtcp::transport_feedbacks::cc_feedback_report::CcFeedbackReport;
use rtci::rtcp::transport_feedbacks::transport_layer_cc::TransportLayerCc;
use rtci::rtp::packetizer::Packetizer;
use rtci::rtp_transceiver::RTCRtpTransceiverDirection;
use rtci::rtp_transceiver::RTCRtpTransceiverInit;
use rtci::rtp_transceiver::rtp_sender::RTCP_FB_ACK_CCFB;
use rtci::rtp_transceiver::rtp_sender::RTCRtpCodec;
use rtci::rtp_transceiver::rtp_sender::RTCRtpCodecParameters;
use rtci::rtp_transceiver::rtp_sender::RTCRtpCodingParameters;
use rtci::rtp_transceiver::rtp_sender::RTCRtpEncodingParameters;
use rtci::rtp_transceiver::rtp_sender::RTCRtpHeaderExtensionCapability;
use rtci::rtp_transceiver::rtp_sender::RTCRtpHeaderExtensionEncryption;
use rtci::rtp_transceiver::rtp_sender::RTCRtpHeaderExtensionToNegotiate;
use rtci::rtp_transceiver::rtp_sender::RtpCodecKind;
use rtci::rtp_transceiver::rtp_sender::TYPE_RTCP_FB_ACK;
use rtci::sansio::Protocol;
use rtci::shared::marshal::MarshalSize;
use rtci::shared::marshal::Unmarshal;
use serde::Deserialize;
use serde::Serialize;

use crate::WebRtcError;
use crate::peer::Core;

/// The MTU RTP packets are cut to (what browsers use; leaves room for SRTP,
/// UDP, IP and a TURN header).
const RTP_MTU: usize = 1200;

/// The engine's media configuration: every codec browsers negotiate (Opus,
/// G.722, PCMU/PCMA, VP8, VP9, H.264, AV1, H.265, RTX), the default
/// interceptors (NACK, RTCP sender/receiver reports, TWCC feedback,
/// simulcast header extensions), send-side congestion control (GCC over
/// TWCC, with a pacer) and RFC 8888 congestion control feedback (`ack
/// ccfb`), which carries ECN.
pub fn media_stack() -> Result<(MediaEngine, Registry), WebRtcError> {
  media_stack_counting(Arc::default())
}

/// [`media_stack`], with the pacer counting the packets its full queue
/// refuses into `refusals` (a connection's `getStats()` reports them).
pub(crate) fn media_stack_counting(
  refusals: Arc<PacerRefusals>,
) -> Result<(MediaEngine, Registry), WebRtcError> {
  let mut media_engine = MediaEngine::default();
  media_engine
    .register_default_codecs()
    .map_err(crate::peer::rtc_err)?;
  // The defaults register RFC 4733 telephone events (RFC 7874 section 3),
  // at Opus' clock and at 8 kHz, for RTCDTMFSender: a second registration
  // here would put another format on a payload type the video RTX formats
  // use (RFC 9143 section 9.1.1 refuses one payload type with two
  // configurations in a BUNDLE group).
  // The defaults include the fork's key-frame request interceptor, which
  // delivers inbound PLI and FIR to the application (here, an
  // `EncodedMediaTrack`'s `keyframerequest`): upstream ends them at the
  // chain's terminus.
  let registry =
    register_default_interceptors(Registry::new(), &mut media_engine)
      .map_err(crate::peer::rtc_err)?;
  // Send-side congestion control: transport-wide sequence numbers on what
  // leaves, GCC over the TWCC feedback that comes back, and a pacer at its
  // estimate. The defaults already negotiate TWCC (`transport-cc` feedback and
  // the header extension, deduplicated here) and record arrivals for the
  // remote side; this takes the receiver's slot with the same recorder. The
  // pacer only queues (bounded), and the driver releases from it at the
  // engine's `poll_timeout`, so writes never wait on it. It paces at
  // PACING_FACTOR times the estimate, and only while the remote's congestion
  // feedback is in use: see [`FactorPacer`] for when it lets packets straight
  // through instead, what the stats report then, and how a packet its full
  // queue refuses is counted.
  let registry = configure_congestion_control(
    registry,
    Gcc::default(),
    CongestionFeedback::Twcc,
    &mut media_engine,
  )
  .map_err(crate::peer::rtc_err)?
  .with(Slot::Pacer, FactorPacer::new(refusals));
  // RFC 8888 feedback beside TWCC: `a=rtcp-fb:<pt> ack ccfb` on every codec
  // and the recorder that reports each received packet with the ECN
  // codepoint it arrived with. A stream sends ECT(1) (L4S) only once `ack
  // ccfb` is negotiated, since an L4S sender needs CE reported back; a peer
  // that doesn't answer it (browsers, unless Chrome's field trial is on)
  // negotiates without it and keeps TWCC alone. Where both are negotiated
  // the send history keeps one record per packet, so a packet both report
  // reaches the estimator once.
  let registry =
    configure_congestion_control_feedback(registry, &mut media_engine);
  register_header_extensions(&mut media_engine)?;
  // The audio level extensions are offered in the clear only, as Chromium,
  // WebKit and Firefox offer them (and as the 0.22 engine did): rtci 0.2's
  // default interceptors offer each also in its RFC 6904 encrypted form
  // (best effort), and answer an offer that has both with the encrypted one
  // and the clear one `a=extmap:<id>/inactive`. That answer's first
  // "inactive" is then an extmap line's, not the section's direction (WPT
  // `protocol/direction.html` changes the first one to make an invalid
  // answer), and every audio section carries two mappings per extension.
  // RFC 9335 cryptex (`rtpHeaderEncryptionPolicy`) encrypts every extension
  // and is not affected.
  for uri in [
    rtci::sdp::extmap::AUDIO_LEVEL_URI,
    rtci::rtp::extension::CSRC_AUDIO_LEVEL_URI,
  ] {
    media_engine
      .set_header_extension_encryption(
        uri,
        RTCRtpHeaderExtensionEncryption::Clear,
      )
      .map_err(crate::peer::rtc_err)?;
  }
  Ok((media_engine, registry))
}

/// How much faster than the congestion controller's estimate the pacer lets
/// packets out: libwebrtc's pacing factor (`kDefaultPaceMultiplier`, 2.5).
/// The pacer smooths bursts (a key frame's packets); it is not what holds a
/// sender to the estimate, which is the encoder's job in a browser.
pub const PACING_FACTOR: f64 = 2.5;

/// How long the pacer keeps pacing without congestion feedback: from the
/// first packet of a stream that negotiated it, and from each report after
/// that. It is the congestion controller's own horizon
/// (`DEFAULT_PRUNE_HORIZON`, 2 s): past it the send history has written off
/// every packet the remote had not reported, so no report can still bring
/// the estimate up to date, and holding packets at it would cap the
/// connection at a number nothing measures. Remotes send TWCC every 50 to
/// 250 ms (libwebrtc) and RFC 8888 reports about once per round trip, so
/// this is eight reports or more: a few lost ones never trip it.
pub const FEEDBACK_TIMEOUT: std::time::Duration =
  CONGESTION_CONTROL_DEFAULT_PRUNE_HORIZON;

/// The longest the pacer lets its queue grow, in the time it takes to drain:
/// past it the pacer releases faster than the estimate, at what drains the
/// queue in this time (libwebrtc's pacer queue time limit,
/// `kMaxExpectedQueueLength`, 2 s). A source steadily above the estimate is
/// then delayed by this much at most, rather than by however long its
/// backlog takes at the estimate (minutes, at a low one).
pub const QUEUE_TIME_LIMIT: std::time::Duration =
  std::time::Duration::from_secs(2);

/// The engine pacer's rate while [`FactorPacer`] is not pacing: high enough
/// that what it still holds from before (packets queued while it was) goes
/// at the next wake-up.
const UNPACED_BITRATE: f64 = 1e9;

/// The packets (and their bytes) a connection's pacer refused because its
/// queue was full. Shared with the connection (`MediaState`), whose
/// `getStats()` adds them to the selected candidate pair's
/// `packetsDiscardedOnSend` / `bytesDiscardedOnSend`.
#[derive(Default, Debug)]
pub struct PacerRefusals {
  packets: AtomicU64,
  bytes: AtomicU64,
}

impl PacerRefusals {
  /// The packets refused so far, and their bytes.
  pub fn get(&self) -> (u64, u64) {
    (
      self.packets.load(Ordering::Relaxed),
      self.bytes.load(Ordering::Relaxed),
    )
  }

  fn add(&self, bytes: usize) {
    self.packets.fetch_add(1, Ordering::Relaxed);
    self.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
  }
}

/// The pacer slot: the engine's pacer at [`PACING_FACTOR`] times the
/// congestion controller's estimate, while the remote's congestion feedback
/// is in use, and a pass-through otherwise.
///
/// - **Which packets.** Those of a local video stream (its RTX and FEC
///   SSRCs included) that negotiated feedback: the transport-wide sequence
///   number extension (TWCC) or `ack ccfb` (RFC 8888). A stream that
///   negotiated neither is never paced: the remote will not report on it, so
///   the estimate never moves and pacing would hold it at the initial rate
///   (GCC's 300 kb/s, so 750 kb/s here) for the life of the connection.
///   Audio is never paced either: its rate is small and steady, and behind a
///   video key frame in one queue it would arrive late (libwebrtc sends it
///   ahead of video). The congestion controller still counts it.
/// - **How fast.** At [`PACING_FACTOR`] times the estimate, or faster when
///   the queue would take longer than [`QUEUE_TIME_LIMIT`] to drain at that:
///   then at what drains it in that time.
/// - **When.** While reports keep coming: TWCC or RFC 8888 feedback (or an
///   estimate the controller made from one) went past within
///   [`FEEDBACK_TIMEOUT`]. Before the first report the window runs from
///   the first packet of a negotiated stream, at the initial rate, as a
///   browser starts; a remote that negotiated feedback but sends none, or
///   whose reports stop, gets pass-through once it passes, and pacing at the
///   estimate again with its next report. What was queued then goes at
///   once, and later packets of a stream wait behind it, so a stream's
///   packets keep their order.
/// - **Stats.** The estimate reaches `availableOutgoingBitrate` and
///   `targetBitrate` only while it paces: from the first report (the
///   initial rate until the controller moves it) to when reports stop, and
///   again from the next one. Without feedback `availableOutgoingBitrate` is
///   absent and `targetBitrate` is 0, as before any estimate: nothing is
///   estimated, and nothing but the source and the path limits the rate.
/// - **A full queue.** The engine pacer refuses a packet past its limit, as
///   a congested path would drop it, without an error to its caller (the
///   write reaches it later, from the driver). Each refusal is counted
///   ([`PacerRefusals`]) and logged once per run of them.
///
/// The engine paces at the estimate itself (it adds no headroom of its own)
/// and hands the same number to the stats, so the estimate stays what the
/// stats report; only the rate this pacer releases at is scaled.
pub(crate) struct FactorPacer {
  pacer: PacerInterceptor,
  /// The SSRCs (primary, RTX, FEC) of each bound local stream it paces
  /// ([`paces`](Self::paces)), by its primary SSRC.
  negotiated: HashMap<u32, Vec<u32>>,
  /// The controller's latest estimate (GCC's initial one before any).
  estimate: f64,
  /// When the last report (or estimate) came by.
  last_feedback: Option<Instant>,
  /// Before any report: when a negotiated stream's first packet went out.
  waiting_since: Option<Instant>,
  /// Whether packets of negotiated streams are paced now.
  paced: bool,
  /// What the engine pacer holds, oldest first (its queue is a FIFO): when
  /// each packet was written, and its bits.
  held: VecDeque<(Instant, f64)>,
  /// The bits in `held`.
  held_bits: f64,
  /// The latest instant handed in.
  now: Option<Instant>,
  /// Whether the stats hold an estimate this pacer let through.
  estimate_reported: bool,
  /// Packets let straight through, behind what the engine pacer releases.
  passed: VecDeque<TaggedPacket>,
  /// Attribute carriers for the stats (an estimate withdrawn).
  carriers: VecDeque<TaggedPacket>,
  refusals: Arc<PacerRefusals>,
  /// In a run of refusals (logged at its first).
  refusing: bool,
}

impl FactorPacer {
  pub(crate) fn new(refusals: Arc<PacerRefusals>) -> Self {
    Self::with_pacer(
      PacerBuilder::new()
        .with_target_bitrate(GCC_INITIAL_BITRATE * PACING_FACTOR)
        // The queue's time limit is this pacer's (`rate`: drained faster
        // past QUEUE_TIME_LIMIT, never discarded for its age), not the
        // engine pacer's default, which drops what waited 2 s.
        .with_queue_time_limit(None)
        .build(),
      refusals,
    )
  }

  pub(crate) fn with_pacer(
    pacer: PacerInterceptor,
    refusals: Arc<PacerRefusals>,
  ) -> Self {
    Self {
      pacer,
      negotiated: HashMap::new(),
      estimate: GCC_INITIAL_BITRATE,
      last_feedback: None,
      waiting_since: None,
      paced: true,
      held: VecDeque::new(),
      held_bits: 0.0,
      now: None,
      estimate_reported: false,
      passed: VecDeque::new(),
      carriers: VecDeque::new(),
      refusals,
      refusing: false,
    }
  }

  /// The estimate a packet on the read leg carries (the congestion
  /// controller attaches it to the feedback that produced it).
  fn estimate(msg: &TaggedPacket) -> Option<f64> {
    match msg.message.get(&Attribute::TargetBitrateChanged {
      bits_per_second: 0.0,
    }) {
      Some(Attribute::TargetBitrateChanged { bits_per_second }) => {
        Some(*bits_per_second)
      }
      _ => None,
    }
  }

  /// Whether an inbound packet is congestion feedback: a TWCC or an RFC
  /// 8888 report.
  fn is_feedback(msg: &TaggedPacket) -> bool {
    let Packet::Rtcp(packets) = &msg.message.packet else {
      return false;
    };
    packets.iter().any(|p| {
      let p = p.as_any();
      p.is::<TransportLayerCc>() || p.is::<CcFeedbackReport>()
    })
  }

  /// Whether a stream is paced: video that negotiated feedback the remote
  /// reports it with.
  fn paces(info: &StreamInfo) -> bool {
    !info.mime_type.to_ascii_lowercase().starts_with("audio/")
      && Self::negotiates_feedback(info)
  }

  /// Whether a stream negotiated feedback the remote reports it with.
  fn negotiates_feedback(info: &StreamInfo) -> bool {
    info
      .rtp_header_extensions
      .iter()
      .any(|e| e.uri == rtci::sdp::extmap::TRANSPORT_CC_URI)
      || info
        .rtcp_feedback
        .iter()
        .any(|f| f.typ == TYPE_RTCP_FB_ACK && f.parameter == RTCP_FB_ACK_CCFB)
  }

  fn is_negotiated(&self, ssrc: u32) -> bool {
    self.negotiated.values().any(|ssrcs| ssrcs.contains(&ssrc))
  }

  /// Whether negotiated streams are to be paced at `now`.
  fn pacing(&self, now: Instant) -> bool {
    if self.negotiated.is_empty() {
      return false;
    }
    match self.last_feedback.or(self.waiting_since) {
      Some(since) => now.saturating_duration_since(since) < FEEDBACK_TIMEOUT,
      None => true,
    }
  }

  /// The rate the engine pacer releases at now: the scaled estimate, or,
  /// when that is slower, what lets the queue out by the time its oldest
  /// packet has waited [`QUEUE_TIME_LIMIT`] (libwebrtc's rule: the time left
  /// is the limit less that wait, and at least a millisecond).
  fn rate(&self) -> f64 {
    if !self.paced {
      return UNPACED_BITRATE;
    }
    let paced = self.estimate * PACING_FACTOR;
    let (Some(&(oldest, _)), Some(now)) = (self.held.front(), self.now) else {
      return paced;
    };
    let left = QUEUE_TIME_LIMIT
      .saturating_sub(now.saturating_duration_since(oldest))
      .max(std::time::Duration::from_millis(1));
    paced.max(self.held_bits / left.as_secs_f64())
  }

  /// Records the latest instant handed in.
  fn observe(&mut self, now: Instant) {
    if self.now.is_none_or(|last| now > last) {
      self.now = Some(now);
    }
  }

  /// Hands the engine pacer [`rate`](Self::rate).
  fn apply_rate(&mut self) {
    let rate = self.rate();
    self.pacer.pacer_mut().set_target_bitrate(rate);
  }

  /// Starts or stops pacing as feedback came or stopped.
  fn update(&mut self, now: Instant) {
    self.observe(now);
    let paced = self.pacing(now);
    if paced == self.paced {
      return;
    }
    self.paced = paced;
    self.apply_rate();
    if !paced && self.estimate_reported {
      // The stats stop reporting an estimate nothing is paced at.
      self.estimate_reported = false;
      let mut message = AttributedPacket::new(Packet::Rtcp(Vec::new()));
      message.add(Attribute::TargetBitrateChanged {
        bits_per_second: 0.0,
      });
      self.carriers.push_back(TaggedPacket {
        now,
        transport: Default::default(),
        message,
      });
    }
  }

  /// The engine pacer's target rate.
  #[cfg(test)]
  pub(crate) fn target_bitrate(&self) -> f64 {
    self.pacer.pacer().target_bitrate()
  }
}

impl rtci::sansio::Protocol<TaggedPacket, TaggedPacket, ()> for FactorPacer {
  type Rout = TaggedPacket;
  type Wout = TaggedPacket;
  type Eout = ();
  type Error = rtci::shared::error::Error;
  type Time = Instant;

  fn handle_read(&mut self, mut msg: TaggedPacket) -> Result<(), Self::Error> {
    let estimate = Self::estimate(&msg);
    let feedback = estimate.is_some() || Self::is_feedback(&msg);
    if let Some(bits_per_second) = estimate {
      self.estimate = bits_per_second;
    }
    if feedback {
      self.last_feedback = Some(msg.now);
    }
    self.update(msg.now);
    if feedback && self.paced {
      if estimate.is_none() && !self.estimate_reported {
        // Pacing (again) at an estimate the stats don't hold: the first
        // report, or the first since reports stopped.
        msg.message.add(Attribute::TargetBitrateChanged {
          bits_per_second: self.estimate,
        });
      }
      self.estimate_reported = true;
    }
    if estimate.is_some() && !self.paced {
      // Not pacing (no congestion feedback negotiated, or none for
      // FEEDBACK_TIMEOUT): the stats report no estimate, even one the
      // controller attached without congestion feedback (rtci 0.2 does, on a
      // connection that negotiated none).
      msg
        .message
        .attributes
        .retain(|a| !matches!(a, Attribute::TargetBitrateChanged { .. }));
    }
    // The engine's pacer follows an estimate going past; the scaled rate
    // (or the pass-through's) replaces what it set.
    self.pacer.handle_read(msg)?;
    self.apply_rate();
    Ok(())
  }

  fn poll_read(&mut self) -> Option<TaggedPacket> {
    self.pacer.poll_read().or_else(|| self.carriers.pop_front())
  }

  fn handle_write(&mut self, msg: TaggedPacket) -> Result<(), Self::Error> {
    let Packet::Rtp(rtp) = &msg.message.packet else {
      // RTCP: the engine's pacer passes it straight on.
      return self.pacer.handle_write(msg);
    };
    let bytes = rtp.marshal_size();
    if !self.is_negotiated(rtp.header.ssrc) {
      self.passed.push_back(msg);
      return Ok(());
    }
    if self.last_feedback.is_none() && self.waiting_since.is_none() {
      self.waiting_since = Some(msg.now);
    }
    self.update(msg.now);
    if !self.paced && self.pacer.queued() == 0 {
      self.passed.push_back(msg);
      return Ok(());
    }
    let refused = self.pacer.dropped();
    self.pacer.handle_write(msg)?;
    if self.pacer.dropped() > refused {
      self.refusals.add(bytes);
      if !self.refusing {
        self.refusing = true;
        log::warn!(
          "webrtc: the pacer's queue is full ({} packets); refusing RTP until it drains",
          self.pacer.queued()
        );
      }
    } else {
      self.refusing = false;
      let bits = (bytes * 8) as f64;
      self
        .held
        .push_back((self.now.unwrap_or_else(Instant::now), bits));
      self.held_bits += bits;
      self.apply_rate();
    }
    Ok(())
  }

  fn poll_write(&mut self) -> Option<TaggedPacket> {
    // What the engine pacer releases was written before anything still let
    // through here.
    if let Some(out) = self.pacer.poll_write() {
      if matches!(out.message.packet, Packet::Rtp(_))
        && let Some((_, bits)) = self.held.pop_front()
      {
        self.held_bits = if self.held.is_empty() {
          0.0
        } else {
          (self.held_bits - bits).max(0.0)
        };
        self.apply_rate();
      }
      return Some(out);
    }
    self.passed.pop_front()
  }

  fn handle_event(&mut self, evt: ()) -> Result<(), Self::Error> {
    self.pacer.handle_event(evt)
  }

  fn poll_event(&mut self) -> Option<()> {
    self.pacer.poll_event()
  }

  fn handle_timeout(&mut self, now: Instant) -> Result<(), Self::Error> {
    self.update(now);
    self.apply_rate();
    self.pacer.handle_timeout(now)
  }

  fn poll_timeout(&mut self) -> Option<Instant> {
    self.pacer.poll_timeout()
  }

  fn close(&mut self) -> Result<(), Self::Error> {
    self.pacer.close()
  }
}

impl Interceptor for FactorPacer {
  fn bind_local_stream(&mut self, info: &StreamInfo) {
    if Self::paces(info) {
      let ssrcs = std::iter::once(info.ssrc)
        .chain(info.ssrc_rtx)
        .chain(info.ssrc_fec)
        .collect();
      self.negotiated.insert(info.ssrc, ssrcs);
    }
    self.pacer.bind_local_stream(info);
  }

  fn unbind_local_stream(&mut self, info: &StreamInfo) {
    self.negotiated.remove(&info.ssrc);
    self.pacer.unbind_local_stream(info);
  }

  fn bind_remote_stream(&mut self, info: &StreamInfo) {
    self.pacer.bind_remote_stream(info);
  }

  fn unbind_remote_stream(&mut self, info: &StreamInfo) {
    self.pacer.unbind_remote_stream(info);
  }
}

/// The RTP header extensions RFC 8834 section 5.2 has a WebRTC endpoint
/// support, on top of the ones the default interceptors register (the
/// simulcast `mid` / `rid` / `repaired-rid` for video, transport-wide
/// congestion control for both): `mid` for audio too (5.2.4, mandatory with
/// BUNDLE), the client-to-mixer audio level (5.2.2, RFC 6464) and the
/// coordination of video orientation (5.2.5), and absolute capture time
/// (abs-capture-time; RTCRtpTransceiver's header extension list starts it
/// "stopped", so an offer carries it only once the application asks, while an
/// answer takes it when offered). Registered after the defaults, so the ids
/// those have stay as they were. A sender writes the audio level and the
/// capture time (`header_extensions.rs`); the engine reads them from
/// received packets as any other extension.
fn register_header_extensions(
  media_engine: &mut MediaEngine,
) -> Result<(), WebRtcError> {
  for (uri, kind) in [
    (rtci::sdp::extmap::SDES_MID_URI, RtpCodecKind::Audio),
    (rtci::sdp::extmap::AUDIO_LEVEL_URI, RtpCodecKind::Audio),
    (
      rtci::sdp::extmap::VIDEO_ORIENTATION_URI,
      RtpCodecKind::Video,
    ),
    (
      crate::header_extensions::ABS_CAPTURE_TIME_URI,
      RtpCodecKind::Audio,
    ),
    (
      crate::header_extensions::ABS_CAPTURE_TIME_URI,
      RtpCodecKind::Video,
    ),
  ] {
    media_engine
      .register_header_extension(
        RTCRtpHeaderExtensionCapability {
          uri: uri.to_string(),
        },
        kind,
        None,
      )
      .map_err(crate::peer::rtc_err)?;
  }
  Ok(())
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
    let audio = self.mime_type.to_ascii_lowercase().starts_with("audio/");
    RTCRtpCodec {
      mime_type: self.mime_type.clone(),
      clock_rate: self.clock_rate,
      // A mono audio codec is the engine's 0 (no encoding parameter in its
      // rtpmap), the reverse of `from_codec`: the engine matches a codec
      // dictionary's channels exactly (W3C webrtc "codec dictionary
      // match"), so `channels: 1`, which getCapabilities() and
      // getParameters() report for PCMU, PCMA, G722, CN and
      // telephone-event, would match none of its codecs.
      channels: match self.channels {
        Some(1) if audio => 0,
        channels => channels.unwrap_or(0),
      },
      sdp_fmtp_line: self.sdp_fmtp_line.clone().unwrap_or_default(),
      rtcp_feedback: vec![],
    }
  }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
  pub(crate) codecs: Vec<CodecArg>,
  header_extensions: Vec<HeaderExtensionCapability>,
}

#[derive(Serialize)]
pub struct HeaderExtensionCapability {
  uri: String,
}

/// `RTCRtpSender.getCapabilities(kind)` / `RTCRtpReceiver.getCapabilities`:
/// what the media stack offers, read from the offer of a connection that is
/// never used for anything else (the engine's codec lists are internal).
/// RTX is listed once, without the `apt` that ties each RTX payload type to
/// its primary, as browsers list it.
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
  let index = pc
    .add_transceiver_from_kind(
      kind_enum,
      Some(RTCRtpTransceiverInit {
        direction: RTCRtpTransceiverDirection::Sendrecv,
        streams: vec![],
        send_encodings: vec![RTCRtpEncodingParameters::default()],
      }),
    )
    .map_err(crate::peer::rtc_err)?;
  let offer = pc.create_offer(None).map_err(crate::peer::rtc_err)?;
  // The codecs come from the engine's registry (its own
  // `RTCRtpSender.getCapabilities`), not from the offer: rtci 0.2 writes an
  // offered format's fmtp line from the first registered format that matches
  // it (`MediaEngine::local_fmtp_line_of_codec` through
  // `codec_parameters_fuzzy_search`), and H.264 matches on profile and
  // packetization mode only, so the High profile levels 4.0, 4.1, 4.2 and
  // 5.1 (payload types 118 to 121) are offered with level 5.0's
  // `profile-level-id=640032` and read back from the offer as one codec
  // (WPT `protocol/codecs-filtered-by-direction`: 10 capabilities against
  // 14 offered formats).
  let registered = pc
    .rtp_sender(index.into())
    .and_then(|sender| sender.get_capabilities(kind_enum));
  let _ = pc.close();
  let mut caps = capabilities_from_sdp(kind, &offer.sdp);
  if let Some(registered) = registered {
    caps.codecs = codecs_from_registry(&registered.codecs);
  }
  Ok(Some(caps))
}

/// The engine's registered codecs as `getCapabilities()` lists them, each
/// once: RTX without the `apt` that ties it to one primary (the engine's
/// list already has none), every other format with its parameters, as
/// [`capabilities_from_sdp`] lists them.
fn codecs_from_registry(codecs: &[RTCRtpCodec]) -> Vec<CodecArg> {
  let mut out: Vec<CodecArg> = Vec::new();
  for codec in codecs {
    let mut arg = CodecArg::from_codec(codec);
    if arg.mime_type.to_ascii_lowercase().ends_with("/rtx") {
      arg.sdp_fmtp_line = None;
    }
    let duplicate = out.iter().any(|c| {
      c.mime_type.eq_ignore_ascii_case(&arg.mime_type)
        && c.clock_rate == arg.clock_rate
        && c.channels == arg.channels
        && c.sdp_fmtp_line == arg.sdp_fmtp_line
    });
    if !duplicate {
      out.push(arg);
    }
  }
  out
}

/// The codecs (`a=rtpmap` + `a=fmtp`) and header extensions (`a=extmap`)
/// of an SDP's media sections of `kind`.
pub fn capabilities_from_sdp(kind: &str, sdp: &str) -> Capabilities {
  let mut codecs: Vec<(String, CodecArg)> = Vec::new();
  let mut fmtp: HashMap<String, String> = HashMap::new();
  let mut header_extensions: Vec<(u16, HeaderExtensionCapability)> = Vec::new();
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
      && let Some(uri) = extmap_uri(value)
    {
      // By id: the engine writes the lines in no particular order.
      let id = value
        .split(|c: char| c == '/' || c.is_ascii_whitespace())
        .next()
        .and_then(|id| id.parse::<u16>().ok())
        .unwrap_or(u16::MAX);
      header_extensions.push((
        id,
        HeaderExtensionCapability {
          uri: uri.to_string(),
        },
      ));
    }
  }
  header_extensions.sort_by_key(|(id, _)| *id);
  // An extension offered both in the clear and encrypted (RFC 6904) is one
  // capability.
  let mut unique: Vec<HeaderExtensionCapability> = Vec::new();
  for (_, h) in header_extensions {
    if !unique.iter().any(|u| u.uri == h.uri) {
      unique.push(h);
    }
  }
  let header_extensions = unique;
  let mut out: Vec<CodecArg> = Vec::new();
  for (pt, mut codec) in codecs {
    // RTX is listed without the `apt` that ties its payload type to one
    // primary. Every other format keeps its parameters, an audio RED's
    // redundancy list (`111/111`) included, as browsers list it and as the
    // engine registered it: setCodecPreferences() matches them exactly.
    if !codec.mime_type.to_ascii_lowercase().ends_with("/rtx") {
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

/// The RFC 6904 URI that marks an `a=extmap` line as the encrypted form of
/// the extension it names next.
const ENCRYPT_URI: &str = "urn:ietf:params:rtp-hdrext:encrypt";

/// The extension URI of an `a=extmap` value (`<id>[/<direction>] <uri>
/// [attributes]`); of the encrypted form (`<id>
/// urn:ietf:params:rtp-hdrext:encrypt <uri> [attributes]`, RFC 6904 section
/// 4, which rtci 0.2 offers for the audio level extensions beside the clear
/// one), the URI it encrypts.
pub(crate) fn extmap_uri(value: &str) -> Option<&str> {
  let mut tokens = value.split_ascii_whitespace().skip(1);
  match tokens.next()? {
    ENCRYPT_URI => tokens.next(),
    uri => Some(uri),
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
  /// Port 0 without `a=bundle-only`: the section was rejected.
  pub rejected: bool,
  pub kind: String,
  /// The layers of the section's `a=simulcast` `recv` list (RFC 8853): the
  /// first of each layer's alternatives, without the paused marker `~`,
  /// each once, in the list's order.
  pub simulcast_recv: Vec<String>,
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
          simulcast_recv: vec![],
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
    } else if let Some(value) = attr.strip_prefix("simulcast:") {
      section.simulcast_recv = simulcast_recv_layers(value);
    } else if matches!(attr, "sendrecv" | "sendonly" | "recvonly" | "inactive")
    {
      section.direction = attr.to_string();
    } else if attr == "bundle-only" {
      // RFC 8843 section 6 / RFC 9429 section 5.10: port zero with
      // `a=bundle-only` is a section reached through its BUNDLE group, not a
      // rejected one.
      section.rejected = false;
    }
  }
  flush(&mut current, has_msid);
  out
}

/// The layers of an `a=simulcast` value's `recv` list (`send 1;2 recv
/// a,b;~c`): the first alternative of each, `~` dropped, each once.
fn simulcast_recv_layers(value: &str) -> Vec<String> {
  let mut out: Vec<String> = Vec::new();
  let mut fields = value.split_ascii_whitespace();
  while let (Some(direction), Some(list)) = (fields.next(), fields.next()) {
    if !direction.eq_ignore_ascii_case("recv") {
      continue;
    }
    for layer in list.split(';') {
      let first = layer.split(',').next().unwrap_or_default();
      let rid = first.strip_prefix('~').unwrap_or(first);
      if !rid.is_empty() && !out.iter().any(|r| r == rid) {
        out.push(rid.to_string());
      }
    }
  }
  out
}

/// The codec preferences an `EncodedMediaTrack` gives its transceiver: its
/// codec, then (for audio) the telephone-event formats `RTCDTMFSender`
/// needs, and (for video) retransmission, which a preference list leaves out
/// unless it lists it (W3C `setCodecPreferences`). The engine offers no FEC
/// format by default, and a preference it doesn't offer is refused.
pub fn track_preferences(codec: &CodecArg) -> Vec<CodecArg> {
  let mut out = vec![codec.clone()];
  if codec.mime_type.to_ascii_lowercase().starts_with("video/") {
    out.push(CodecArg {
      mime_type: "video/rtx".to_string(),
      clock_rate: 90000,
      channels: None,
      sdp_fmtp_line: None,
    });
  }
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
  /// The RFC 6464 level (-dBov) measured from the frame's PCM, as the sender
  /// writes it in the audio level header extension
  /// (`header_extensions::audio_level`); `None` where the PCM isn't at hand
  /// (and for video).
  pub audio_level: Option<u8>,
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
) -> Option<Box<dyn rtci::rtp::packetizer::Payloader>> {
  use rtci::rtp::codec;
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
  /// The generation JavaScript gave each receiver's last mute
  /// (`op_webrtc_receiver_muted`); a `trackunmute` carries it, so one
  /// queued before a newer mute is told apart.
  pub unmute_generation: HashMap<u32, u32>,
  /// RFC 4733 state per sender: the next sequence number and timestamp.
  pub dtmf: HashMap<u32, (u16, u32)>,
  /// The SSRCs and CSRCs each receiver got packets from, for
  /// `getSynchronizationSources()` / `getContributingSources()`.
  pub sources: HashMap<u32, Vec<Source>>,
  /// Where each sender fed by a fake capture device is in its clip.
  pub fake: HashMap<u32, crate::fake_media::Cursor>,
  /// The encoded video frames each outbound RTP stream (by SSRC) took and
  /// sent, which the engine counts only when told (webrtc-stats encoder
  /// members: the encoder is upstream of the sender here).
  pub sent_video: HashMap<u32, SentVideo>,
  /// The complete video frames each inbound RTP stream (by SSRC) brought.
  pub received_video: HashMap<u32, ReceivedVideo>,
  /// What this connection's pacer refused ([`FactorPacer`]).
  pub pacer_refusals: Arc<PacerRefusals>,
  /// When each inbound stream (by SSRC) last asked its sender for a key
  /// frame after a loss ([`KEY_FRAME_REQUEST_INTERVAL`]).
  pub loss_key_frame_requests: HashMap<u32, Instant>,
}

/// How often a receiver whose stream lost a frame asks the sender for a key
/// frame again while no key frame came: once per SSRC per this interval, as
/// libwebrtc paces its repeated requests. Each delta frame after the loss
/// would otherwise send one.
pub const KEY_FRAME_REQUEST_INTERVAL: std::time::Duration =
  std::time::Duration::from_millis(500);

/// What an outbound video stream's frames were, as they reached its sender:
/// encoded frames, from a fake capture device's clip, an
/// `EncodedMediaTrack` or an encoded transform. Only what the frames
/// themselves show is counted (their number, key frames from the bitstream,
/// their coded size, their byte size); the encoder's own measures (QP,
/// encode time, quality limitation) are not known here and stay absent.
#[derive(Debug, Default)]
pub struct SentVideo {
  pub frames_encoded: u32,
  pub key_frames_encoded: u32,
  pub frames_sent: u32,
  pub huge_frames_sent: u32,
  /// Bytes of the frames sent, for their average size.
  pub bytes_sent: u64,
  /// The coded size of the last frame that named one.
  pub size: Option<(u32, u32)>,
  /// When each frame of the last second reached the sender.
  pub recent: std::collections::VecDeque<Instant>,
}

/// webrtc-stats `hugeFramesSent`: a frame at least 2.5 times the average
/// size of the frames sent before it.
const HUGE_FRAME_FACTOR: f64 = 2.5;

impl SentVideo {
  /// Frames that reached the sender in the second up to `now`
  /// (`framesPerSecond`).
  pub fn frames_per_second(&mut self, now: Instant) -> f64 {
    while self.recent.front().is_some_and(|t| {
      now.saturating_duration_since(*t) > std::time::Duration::from_secs(1)
    }) {
      self.recent.pop_front();
    }
    self.recent.len() as f64
  }

  /// One encoded frame reached the sender at `now`; `sent` when its packets
  /// went out.
  pub(crate) fn frame(
    &mut self,
    mime_type: &str,
    data: &[u8],
    sent: bool,
    now: Instant,
  ) {
    self.frames_encoded = self.frames_encoded.saturating_add(1);
    if is_key_frame(mime_type, data) {
      self.key_frames_encoded = self.key_frames_encoded.saturating_add(1);
    }
    if let Some(size) = crate::media_sink::frame_coded_size(mime_type, data) {
      self.size = Some(size);
    }
    self.recent.push_back(now);
    self.frames_per_second(now);
    if !sent {
      return;
    }
    if self.frames_sent > 0 {
      let average = self.bytes_sent as f64 / self.frames_sent as f64;
      if data.len() as f64 >= HUGE_FRAME_FACTOR * average {
        self.huge_frames_sent = self.huge_frames_sent.saturating_add(1);
      }
    }
    self.frames_sent = self.frames_sent.saturating_add(1);
    self.bytes_sent = self.bytes_sent.saturating_add(data.len() as u64);
  }

  fn update(&mut self, now: Instant) -> rtci::statistics::EncoderStatsUpdate {
    rtci::statistics::EncoderStatsUpdate {
      frames_encoded: Some(self.frames_encoded),
      key_frames_encoded: Some(self.key_frames_encoded),
      frames_sent: Some(self.frames_sent),
      huge_frames_sent: Some(self.huge_frames_sent),
      frame_width: self.size.map(|(w, _)| w),
      frame_height: self.size.map(|(_, h)| h),
      frames_per_second: Some(self.frames_per_second(now)),
      ..Default::default()
    }
  }
}

/// The frames of an inbound video stream as its packets arrive: a frame is
/// complete (webrtc-stats `framesReceived`) when its packets, from the one
/// after the previous frame's last (RTP marker) packet through its own
/// marker packet, all arrived in sequence. There is no jitter buffer, so a
/// frame with a packet missing or out of order is not counted, as the frame
/// assemblers (`encoded_frames.rs`) drop it; the first frame, whose start
/// can't be told, is not counted either.
#[derive(Debug, Default)]
pub struct ReceivedVideo {
  pub frames_received: u32,
  /// The RTP timestamp of the frame in the making.
  timestamp: Option<u32>,
  /// The sequence number the next packet should have.
  next: Option<u16>,
  /// Whether the frame in the making lost a packet (or its start).
  broken: bool,
  /// Whether the last packet ended a frame.
  ended: bool,
}

impl ReceivedVideo {
  /// A received packet without a payload: padding, or a ULPFEC packet of a
  /// RED stream (the engine delivers it emptied). It carries no frame but
  /// takes a sequence number of the stream, so the next packet is still in
  /// sequence after it; one that comes after a gap leaves the next frame
  /// uncounted, as its start may be what was lost.
  pub fn empty(&mut self, header: &rtci::rtp::Header) {
    if self.next != Some(header.sequence_number) {
      self.broken = true;
      self.ended = false;
    }
    self.next = Some(header.sequence_number.wrapping_add(1));
  }

  /// A received packet with a payload; returns whether it completed a frame.
  pub fn packet(&mut self, header: &rtci::rtp::Header) -> bool {
    let in_sequence = self.next == Some(header.sequence_number);
    if self.timestamp != Some(header.timestamp) {
      // A new frame starts whole only right after a frame's last packet.
      self.timestamp = Some(header.timestamp);
      self.broken = !(self.ended && in_sequence);
    } else if !in_sequence {
      self.broken = true;
    }
    self.next = Some(header.sequence_number.wrapping_add(1));
    self.ended = header.marker;
    if !header.marker {
      return false;
    }
    self.timestamp = None;
    if self.broken {
      return false;
    }
    self.frames_received = self.frames_received.saturating_add(1);
    true
  }
}

/// At most this many inbound video streams are followed per connection.
const MAX_RECEIVED_VIDEO_STREAMS: usize = 64;

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
  pub fn saw_rtp(&mut self, key: u32, header: &rtci::rtp::Header) {
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

impl Core {
  /// A received RTP packet of a video receiver, for its stream's
  /// `framesReceived` (padding-only packets carry no frame).
  pub(crate) fn count_received_frame(
    &mut self,
    key: u32,
    packet: &rtci::rtp::Packet,
  ) {
    if self.media.kinds.get(&key) != Some(&RtpCodecKind::Video) {
      return;
    }
    let ssrc = packet.header.ssrc;
    let streams = &mut self.media.received_video;
    if !streams.contains_key(&ssrc)
      && streams.len() >= MAX_RECEIVED_VIDEO_STREAMS
    {
      return;
    }
    let video = streams.entry(ssrc).or_default();
    if packet.payload.is_empty() {
      video.empty(&packet.header);
      return;
    }
    if video.packet(&packet.header) {
      let frames_received = video.frames_received;
      self.pc.update_decoder_stats(
        ssrc,
        rtci::statistics::DecoderStatsUpdate {
          frames_received: Some(frames_received),
          ..Default::default()
        },
      );
    }
  }

  /// Brings the members measured over a window (`framesPerSecond`) up to
  /// `now` before a report.
  pub(crate) fn refresh_media_stats(&mut self, now: Instant) {
    for (ssrc, video) in &mut self.media.sent_video {
      let frames_per_second = video.frames_per_second(now);
      self.pc.update_encoder_stats(
        *ssrc,
        rtci::statistics::EncoderStatsUpdate {
          frames_per_second: Some(frames_per_second),
          ..Default::default()
        },
      );
    }
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
  /// engine).
  pub(crate) fn sync_transceiver_keys(&mut self) {
    let count = self.pc.get_transceivers().count();
    while self.media.keys.len() < count {
      let key = self.media.new_key();
      let index = self.media.keys.len();
      self.media.keys.push(key);
      let kind = self
        .pc
        .rtp_receiver(index.into())
        .map(|r| r.track().kind())
        .filter(|k| *k != RtpCodecKind::Unspecified)
        .or_else(|| {
          self
            .pc
            .rtp_sender(index.into())
            .and_then(|s| s.track().map(|t| t.kind()))
        });
      if let Some(kind) = kind.filter(|k| *k != RtpCodecKind::Unspecified) {
        self.media.kinds.insert(key, kind);
      }
    }
  }

  /// After a description (or a rollback): drops the keys of the transceivers
  /// the engine removed from its set, which moves the later ones down (a
  /// rollback undoing the remote offer that created them, RFC 9429 section
  /// 5.7; an answer leaving a stopped transceiver's m= section rejected, W3C
  /// webrtc-pc 4.4.1.6), then keys the ones it added. The engine reports
  /// each removal by the index the transceiver had then, in removal order,
  /// so removing them in that order keeps the keys in step. An index past
  /// the keys is a transceiver the same call added and removed again.
  pub(crate) fn sync_removed_transceivers(&mut self) {
    for index in self.pc.take_removed_transceivers() {
      if index >= self.media.keys.len() {
        continue;
      }
      let key = self.media.keys.remove(index);
      self.forget_transceiver(key);
    }
    self.sync_transceiver_keys();
  }

  /// Per-key state of a transceiver that left the set.
  fn forget_transceiver(&mut self, key: u32) {
    let media = &mut self.media;
    media.kinds.remove(&key);
    media.track_keys.retain(|_, k| *k != key);
    media.reading.remove(&key);
    media.paused.remove(&key);
    media.packetizers.remove(&key);
    media.flowing.remove(&key);
    media.dtmf.remove(&key);
    media.sources.remove(&key);
    media.fake.remove(&key);
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
    self.sync_transceiver_keys();
    let key = self.media.keys[index];
    self.media.kinds.insert(key, kind);
    if let Some(mut sender) = self.pc.rtp_sender(index.into()) {
      sender
        .set_streams(msid_streams(arg.streams))
        .map_err(crate::peer::rtc_err)?;
      // A transceiver made from a kind has no engine track (W3C null).
      if let Some(track) = sender.track() {
        let id = track.track_id().to_string();
        self.media.track_keys.insert(id, key);
      }
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
      t.set_direction(direction).map_err(crate::peer::rtc_err)?;
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
      ))
      .map_err(crate::peer::rtc_err)?;
      t.keep_on_rollback();
    }
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
        sender.set_streams(streams).map_err(crate::peer::rtc_err)?;
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
    // A sender made from a kind has no engine track: the new one takes one
    // active encoding, unless the sender has encodings of its own (which
    // the engine keeps, with their SSRCs).
    let codings = sender.track().map_or_else(
      || {
        vec![RTCRtpEncodingParameters {
          active: true,
          ..Default::default()
        }]
      },
      |t| t.codings().to_vec(),
    );
    let old = sender.track().map(|t| t.track_id().to_string());
    sender
      .replace_track(Some(MediaStreamTrack::new(
        stream,
        track.id.clone(),
        track.label,
        kind,
        codings,
      )))
      .map_err(crate::peer::rtc_err)?;
    sender.set_streams(streams).map_err(crate::peer::rtc_err)?;
    if let Some(old) = old {
      self.media.track_keys.remove(&old);
    }
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
      sender
        .set_streams(msid_streams(streams))
        .map_err(crate::peer::rtc_err)?;
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
      t.set_direction(direction).map_err(crate::peer::rtc_err)?;
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

  /// `setHeaderExtensionsToNegotiate()` and its effect on the next offer or
  /// answer (webrtc-extensions): the URIs this transceiver's media section
  /// may carry. JavaScript hands the list over before each offer and answer.
  pub(crate) fn set_header_extensions_to_negotiate(
    &mut self,
    key: u32,
    uris: Vec<String>,
  ) -> Result<(), WebRtcError> {
    let index = self.index_of(key)?;
    let Some(mut t) = self.pc.rtp_transceiver(index) else {
      return Ok(());
    };
    // The engine takes the whole list with a direction per extension: the
    // ones JavaScript names are negotiated (an extension stopped before
    // comes back "sendrecv"), the others "stopped".
    let list: Vec<RTCRtpHeaderExtensionToNegotiate> = t
      .get_header_extensions_to_negotiate()
      .into_iter()
      .map(|mut e| {
        if !uris.contains(&e.uri) {
          e.direction = RTCRtpTransceiverDirection::Stopped;
        } else if e.direction == RTCRtpTransceiverDirection::Stopped {
          e.direction = RTCRtpTransceiverDirection::Sendrecv;
        }
        e
      })
      .collect();
    // JavaScript refuses to stop an extension every WebRTC endpoint must
    // implement (webrtc-extensions; RFC 8834 section 5.2), as the engine
    // does: what reaches here is what the engine takes.
    t.set_header_extensions_to_negotiate(list)
      .map_err(|e| WebRtcError::InvalidModification(e.to_string()))?;
    Ok(())
  }

  /// `RTCConfiguration.rtpHeaderEncryptionPolicy` and
  /// `alwaysNegotiateDataChannels` (webrtc-extensions).
  pub(crate) fn set_negotiation_options(
    &mut self,
    rtp_header_encryption_policy: &str,
    always_negotiate_data_channels: bool,
  ) -> Result<(), WebRtcError> {
    let policy = match rtp_header_encryption_policy {
      "disable" => RTCRtpHeaderEncryptionPolicy::Disable,
      "negotiate" => RTCRtpHeaderEncryptionPolicy::Negotiate,
      "require" => RTCRtpHeaderEncryptionPolicy::Require,
      other => {
        return Err(WebRtcError::Type(format!(
          "'{other}' is not a valid RTCRtpHeaderEncryptionPolicy"
        )));
      }
    };
    self
      .pc
      .set_rtp_header_encryption_policy(policy)
      .map_err(crate::peer::rtc_err)?;
    self
      .pc
      .set_always_negotiate_data_channels(always_negotiate_data_channels)
      .map_err(crate::peer::rtc_err)?;
    Ok(())
  }

  pub(crate) fn transceivers(&mut self) -> Vec<TransceiverInfo> {
    self.sync_transceiver_keys();
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
      let current_direction = t.current_direction().and_then(direction_str);
      let section = mid.as_ref().and_then(|m| remote.get(m)).cloned();
      // The remote side doesn't send on this section (now): its track is
      // muted, and the next RTP that arrives unmutes it again.
      if section.as_ref().is_some_and(|s| {
        s.rejected || !matches!(s.direction.as_str(), "sendrecv" | "sendonly")
      }) {
        self.media.flowing.remove(&key);
      }
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

  /// The codecs of the transceiver's m-section in the current local
  /// (`local`) or remote description, in the section's order.
  fn negotiated_section_codecs(
    &mut self,
    index: usize,
    local: bool,
  ) -> Option<(String, Vec<(u8, CodecArg)>)> {
    let mid = self.pc.rtp_transceiver(index)?.mid().clone()?;
    let sdp = if local {
      self
        .pc
        .current_local_description()
        .or_else(|| self.pc.pending_local_description())
        .map(|d| d.sdp)
    } else {
      self
        .pc
        .current_remote_description()
        .or(self.pc.remote_description())
        .map(|d| d.sdp.clone())
    }?;
    section_codecs(&sdp, &mid)
  }

  pub(crate) fn sender_parameters(
    &mut self,
    key: u32,
  ) -> Result<serde_json::Value, WebRtcError> {
    let index = self.index_of(key)?;
    // webrtc-pc 5.2 getParameters(): the codecs negotiated for sending, in
    // the remote description's order with its payload types and fmtp lines
    // (RFC 9429 5.3.1: the answerer sends what the offer lists), those this
    // stack can send; a retransmission format with the primary it repairs.
    let remote_codecs = self
      .negotiated_section_codecs(index, false)
      .map(|(kind, codecs)| sendable_codecs(&kind, codecs));
    let Some(mut sender) = self.pc.rtp_sender(index.into()) else {
      // A transceiver a remote offer made has no engine sender until it
      // sends; what it may send is negotiated all the same.
      return Ok(match remote_codecs {
        Some(codecs) => serde_json::json!({
          "codecs": codecs
            .iter()
            .map(|(pt, c)| section_codec_json(*pt, c))
            .collect::<Vec<_>>(),
          "headerExtensions": [],
          "encodings": [],
        }),
        None => serde_json::Value::Null,
      });
    };
    let params = sender.get_parameters().clone();
    let codecs: Vec<serde_json::Value> = match remote_codecs {
      Some(codecs) => codecs
        .iter()
        .map(|(pt, c)| section_codec_json(*pt, c))
        .collect(),
      None => params
        .rtp_parameters
        .codecs
        .iter()
        .map(codec_params_json)
        .collect(),
    };
    Ok(serde_json::json!({
      "codecs": codecs,
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
    // webrtc-pc 5.3 getParameters(): the codecs the receiver is prepared to
    // receive, which are the local description's for its m-section.
    let local_codecs = self.negotiated_section_codecs(index, true);
    let Some(mut receiver) = self.pc.rtp_receiver(index.into()) else {
      return Ok(serde_json::Value::Null);
    };
    let params = receiver.get_parameters().clone();
    let codecs: Vec<serde_json::Value> = match local_codecs {
      Some((_, codecs)) => codecs
        .iter()
        .map(|(pt, c)| section_codec_json(*pt, c))
        .collect(),
      None => params
        .rtp_parameters
        .codecs
        .iter()
        .map(codec_params_json)
        .collect(),
    };
    Ok(serde_json::json!({
      "codecs": codecs,
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
    let mut packet = rtci::rtp::packet::Packet::unmarshal(&mut buf)
      .map_err(|e| WebRtcError::Type(format!("Not an RTP packet: {e}")))?;
    let Some(mut sender) = self.pc.rtp_sender(index.into()) else {
      return Ok(false);
    };
    let Some(ssrc) = sender.ssrcs().next() else {
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
    let Some(ssrc) = sender.ssrcs().next() else {
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
      let packetizer = rtci::rtp::packetizer::new_packetizer(
        now,
        RTP_MTU,
        codec.payload_type,
        ssrc,
        payloader,
        Box::new(rtci::rtp::sequence::new_random_sequencer()),
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
    let mime_type = p.mime_type.clone();
    let ssrc = p.ssrc;
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
    // wpt100:hdrext — the negotiated audio level (RFC 6464) on each packet
    // and the absolute capture time on the frame's first one.
    let mut extension_id = |uri: &str| {
      sender
        .get_parameters()
        .rtp_parameters
        .header_extensions
        .iter()
        .find(|h| h.uri == uri)
        .and_then(|h| u8::try_from(h.id).ok())
    };
    let level_id = extension_id(rtci::sdp::extmap::AUDIO_LEVEL_URI);
    let capture_id =
      extension_id(crate::header_extensions::ABS_CAPTURE_TIME_URI);
    if let Some(id) = level_id
      && let Some(level) =
        crate::header_extensions::audio_level(&mime_type, data)
    {
      let payload = crate::header_extensions::audio_level_payload(level);
      for packet in &mut packets {
        let _ = packet.header.set_extension(id, payload.clone());
      }
    }
    if let Some(id) = capture_id
      && let Some(first) = packets.first_mut()
    {
      let payload = crate::header_extensions::abs_capture_time_payload(
        std::time::SystemTime::now(),
      );
      let _ = first.header.set_extension(id, payload);
    }
    let mut sent = false;
    for packet in packets {
      sent |= sender.write_rtp(now, packet).is_ok();
    }
    // A video frame that reached the sender counts in its stream's encoder
    // stats, and in `framesSent` when it went out.
    if mime_type.to_ascii_lowercase().starts_with("video/") {
      let video = self.media.sent_video.entry(ssrc).or_default();
      video.frame(&mime_type, data, sent, now);
      let update = video.update(now);
      self.pc.update_encoder_stats(ssrc, update);
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
      audio_level: crate::header_extensions::audio_level(&mime, data),
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
    let Some(ssrc) = sender.ssrcs().next() else {
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
      rtci::rtp::packet::Packet {
        header: rtci::rtp::Header {
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
      sent |= write_pli(&mut receiver, media_ssrc, Instant::now());
    }
    Ok(sent)
  }

  /// A receiver's frame assembler lost a frame of the stream `media_ssrc`
  /// and has seen no key frame since: a PLI to the remote sender (RFC 4585
  /// section 6.3.1), at most once per [`KEY_FRAME_REQUEST_INTERVAL`] per
  /// stream. Returns whether one was sent.
  pub(crate) fn request_key_frame_after_loss(
    &mut self,
    key: u32,
    media_ssrc: u32,
    now: Instant,
  ) -> bool {
    let requests = &mut self.media.loss_key_frame_requests;
    if requests.get(&media_ssrc).is_some_and(|last| {
      now.saturating_duration_since(*last) < KEY_FRAME_REQUEST_INTERVAL
    }) {
      return false;
    }
    // Bounded: only streams that asked within the interval are kept.
    requests.retain(|_, last| {
      now.saturating_duration_since(*last) < KEY_FRAME_REQUEST_INTERVAL
    });
    let Ok(index) = self.index_of(key) else {
      return false;
    };
    let Some(mut receiver) = self.pc.rtp_receiver(index.into()) else {
      return false;
    };
    let sent = write_pli(&mut receiver, media_ssrc, now);
    if sent {
      self.media.loss_key_frame_requests.insert(media_ssrc, now);
      self.wrote_rtcp = true;
    }
    sent
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

/// A PLI for the stream `media_ssrc`, written by its receiver. The sender
/// SSRC is 0, as the engine's own requests have it.
fn write_pli(
  receiver: &mut rtci::rtp_transceiver::rtp_receiver::RTCRtpReceiver<'_>,
  media_ssrc: u32,
  now: Instant,
) -> bool {
  receiver
    .write_rtcp(
      now,
      vec![Box::new(PictureLossIndication {
        sender_ssrc: 0,
        media_ssrc,
      })],
    )
    .is_ok()
}

/// Whether an RTCP compound asks for a key frame (PLI or FIR).
pub fn wants_key_frame(packets: &[Box<dyn rtci::rtcp::Packet>]) -> bool {
  packets.iter().any(|p| {
    p.as_any().downcast_ref::<PictureLossIndication>().is_some()
      || p.as_any().downcast_ref::<FullIntraRequest>().is_some()
  })
}

fn section_codec_json(payload_type: u8, c: &CodecArg) -> serde_json::Value {
  let mut v = serde_json::to_value(c).unwrap_or(serde_json::Value::Null);
  if let Some(obj) = v.as_object_mut() {
    obj.insert("payloadType".to_string(), payload_type.into());
  }
  v
}

/// RFC 3551's static audio payload types, which may come without an
/// `a=rtpmap` line.
fn static_audio_codec(payload_type: u8) -> Option<(&'static str, u32)> {
  match payload_type {
    0 => Some(("PCMU", 8000)),
    3 => Some(("GSM", 8000)),
    4 => Some(("G723", 8000)),
    8 => Some(("PCMA", 8000)),
    9 => Some(("G722", 8000)),
    13 => Some(("CN", 8000)),
    18 => Some(("G729", 8000)),
    _ => None,
  }
}

/// The media kind and codecs (payload type, `a=rtpmap`, `a=fmtp`) of the
/// m-section of `sdp` whose mid is `mid`, in the m= line's format order. A
/// dynamic payload type with no `a=rtpmap` is left out.
pub fn section_codecs(
  sdp: &str,
  mid: &str,
) -> Option<(String, Vec<(u8, CodecArg)>)> {
  struct Section {
    kind: String,
    formats: Vec<String>,
    rtpmap: HashMap<String, String>,
    fmtp: HashMap<String, String>,
    mid: Option<String>,
  }
  let mut sections: Vec<Section> = Vec::new();
  for line in sdp.lines() {
    let line = line.trim_end();
    if let Some(m) = line.strip_prefix("m=") {
      let mut fields = m.split_ascii_whitespace();
      let kind = fields.next().unwrap_or_default().to_string();
      let formats = fields.skip(2).map(str::to_string).collect();
      sections.push(Section {
        kind,
        formats,
        rtpmap: HashMap::new(),
        fmtp: HashMap::new(),
        mid: None,
      });
      continue;
    }
    let Some(section) = sections.last_mut() else {
      continue;
    };
    if let Some(value) = line.strip_prefix("a=mid:") {
      section.mid = Some(value.trim().to_string());
    } else if let Some(value) = line.strip_prefix("a=rtpmap:") {
      if let Some((pt, rest)) = value.split_once(' ') {
        section
          .rtpmap
          .entry(pt.to_string())
          .or_insert_with(|| rest.trim().to_string());
      }
    } else if let Some(value) = line.strip_prefix("a=fmtp:")
      && let Some((pt, params)) = value.split_once(' ')
    {
      section
        .fmtp
        .entry(pt.to_string())
        .or_insert_with(|| params.trim().to_string());
    }
  }
  let section = sections
    .into_iter()
    .find(|s| s.mid.as_deref() == Some(mid))?;
  let audio = section.kind == "audio";
  let mut out: Vec<(u8, CodecArg)> = Vec::new();
  for format in &section.formats {
    let Ok(pt) = format.parse::<u8>() else {
      continue;
    };
    if out.iter().any(|(p, _)| *p == pt) {
      continue;
    }
    let (name, clock_rate, channels) = match section.rtpmap.get(format) {
      Some(map) => {
        let mut parts = map.split('/');
        let name = parts.next().unwrap_or_default().to_string();
        let clock_rate = parts.next().and_then(|c| c.parse().ok()).unwrap_or(0);
        let channels = parts.next().and_then(|c| c.parse().ok());
        (name, clock_rate, channels)
      }
      None => match static_audio_codec(pt).filter(|_| audio) {
        Some((name, clock_rate)) => (name.to_string(), clock_rate, None),
        None => continue,
      },
    };
    out.push((
      pt,
      CodecArg {
        mime_type: format!("{}/{name}", section.kind),
        clock_rate,
        // An audio codec without a channel count is mono (RFC 4566).
        channels: if audio { channels.or(Some(1)) } else { None },
        sdp_fmtp_line: section.fmtp.get(format).cloned(),
      },
    ));
  }
  Some((section.kind, out))
}

/// The send codecs this stack implements for `kind` (computed once).
fn supported_codecs(kind: &str) -> &'static [CodecArg] {
  static AUDIO: std::sync::OnceLock<Vec<CodecArg>> = std::sync::OnceLock::new();
  static VIDEO: std::sync::OnceLock<Vec<CodecArg>> = std::sync::OnceLock::new();
  let cell = match kind {
    "audio" => &AUDIO,
    "video" => &VIDEO,
    _ => return &[],
  };
  cell.get_or_init(|| {
    capabilities(kind, true)
      .ok()
      .flatten()
      .map(|c| c.codecs)
      .unwrap_or_default()
  })
}

fn is_rtx(c: &CodecArg) -> bool {
  c.mime_type.to_ascii_lowercase().ends_with("/rtx")
}

/// The `apt` (RFC 4588) of a retransmission format's fmtp line.
fn rtx_apt(c: &CodecArg) -> Option<u8> {
  c.sdp_fmtp_line.as_deref()?.split(';').find_map(|p| {
    p.trim()
      .strip_prefix("apt=")
      .and_then(|v| v.trim().parse().ok())
  })
}

/// Of a remote m-section's codecs, those this stack can send (same name,
/// clock rate and, for audio, channels as one it implements); a
/// retransmission format only with the primary it repairs.
fn sendable_codecs(
  kind: &str,
  codecs: Vec<(u8, CodecArg)>,
) -> Vec<(u8, CodecArg)> {
  let supported = supported_codecs(kind);
  let known = |c: &CodecArg| {
    supported.iter().any(|s| {
      s.mime_type.eq_ignore_ascii_case(&c.mime_type)
        && s.clock_rate == c.clock_rate
        && (kind != "audio" || s.channels == c.channels)
    })
  };
  let primaries: Vec<u8> = codecs
    .iter()
    .filter(|(_, c)| !is_rtx(c) && known(c))
    .map(|(pt, _)| *pt)
    .collect();
  codecs
    .into_iter()
    .filter(|(pt, c)| {
      if is_rtx(c) {
        known(c) && rtx_apt(c).is_some_and(|apt| primaries.contains(&apt))
      } else {
        primaries.contains(pt)
      }
    })
    .collect()
}

fn codec_params_json(c: &RTCRtpCodecParameters) -> serde_json::Value {
  let mut v = serde_json::to_value(CodecArg::from_codec(&c.rtp_codec))
    .unwrap_or(serde_json::Value::Null);
  if let Some(obj) = v.as_object_mut() {
    obj.insert("payloadType".to_string(), c.payload_type.into());
  }
  v
}

#[cfg(test)]
mod factor_pacer_tests {
  use std::sync::Arc;
  use std::time::Duration;
  use std::time::Instant;

  use bytes::Bytes;
  use rtci::interceptor::Attribute;
  use rtci::interceptor::AttributedPacket;
  use rtci::interceptor::GCC_INITIAL_BITRATE;
  use rtci::interceptor::Interceptor;
  use rtci::interceptor::PacerBuilder;
  use rtci::interceptor::Packet;
  use rtci::interceptor::RTCPFeedback;
  use rtci::interceptor::RTPHeaderExtension;
  use rtci::interceptor::StreamInfo;
  use rtci::interceptor::TaggedPacket;
  use rtci::rtcp::transport_feedbacks::cc_feedback_report::CcFeedbackReport;
  use rtci::rtcp::transport_feedbacks::transport_layer_cc::TransportLayerCc;
  use rtci::sansio::Protocol;
  use rtci::shared::TransportContext;
  use rtci::shared::marshal::MarshalSize;

  use super::FEEDBACK_TIMEOUT;
  use super::FactorPacer;
  use super::PACING_FACTOR;
  use super::PacerRefusals;
  use super::QUEUE_TIME_LIMIT;

  const SSRC: u32 = 1234;

  fn rtcp(
    now: Instant,
    packet: Box<dyn rtci::rtcp::Packet>,
    estimate: Option<f64>,
  ) -> TaggedPacket {
    let mut message = AttributedPacket::new(Packet::Rtcp(vec![packet]));
    if let Some(bits_per_second) = estimate {
      message.add(Attribute::TargetBitrateChanged { bits_per_second });
    }
    TaggedPacket {
      now,
      transport: TransportContext::default(),
      message,
    }
  }

  /// A receiver report: not congestion feedback, but the estimate the
  /// controller attaches makes it one.
  fn feedback(estimate: Option<f64>) -> TaggedPacket {
    rtcp(
      Instant::now(),
      Box::new(rtci::rtcp::receiver_report::ReceiverReport::default()),
      estimate,
    )
  }

  fn twcc(now: Instant, estimate: Option<f64>) -> TaggedPacket {
    rtcp(now, Box::new(TransportLayerCc::default()), estimate)
  }

  fn ccfb(now: Instant) -> TaggedPacket {
    rtcp(now, Box::new(CcFeedbackReport::default()), None)
  }

  fn rtp(now: Instant, ssrc: u32, seq: u16) -> TaggedPacket {
    TaggedPacket {
      now,
      transport: TransportContext::default(),
      message: AttributedPacket::new(Packet::Rtp(rtci::rtp::packet::Packet {
        header: rtci::rtp::Header {
          version: 2,
          sequence_number: seq,
          ssrc,
          ..Default::default()
        },
        payload: Bytes::from(vec![0u8; 1000]),
      })),
    }
  }

  /// A stream that negotiated the transport-wide sequence number (TWCC).
  fn twcc_stream(ssrc: u32) -> StreamInfo {
    StreamInfo {
      ssrc,
      rtp_header_extensions: vec![RTPHeaderExtension {
        uri: rtci::sdp::extmap::TRANSPORT_CC_URI.to_string(),
        id: 5,
      }],
      ..Default::default()
    }
  }

  /// A stream that negotiated RFC 8888 feedback (`ack ccfb`) only.
  fn ccfb_stream(ssrc: u32) -> StreamInfo {
    StreamInfo {
      ssrc,
      rtcp_feedback: vec![RTCPFeedback {
        typ: "ack".to_string(),
        parameter: "ccfb".to_string(),
      }],
      ..Default::default()
    }
  }

  /// What leaves a paced stream at once: the engine pacer starts with one
  /// burst in its bucket and refills it to one burst (draft-ietf-rmcat-gcc-02
  /// section 4: the rate times 5 ms, at least one full-sized packet), which
  /// at the initial rate is one of these packets.
  const BURST: usize = 1;

  fn pacer() -> FactorPacer {
    FactorPacer::new(Arc::default())
  }

  /// Writes `count` RTP packets at `now` and returns how many came straight
  /// out (unpaced), without a timeout releasing them.
  fn write(p: &mut FactorPacer, now: Instant, ssrc: u32, count: u16) -> usize {
    for seq in 0..count {
      p.handle_write(rtp(now, ssrc, seq)).unwrap();
    }
    drain(p)
  }

  fn drain(p: &mut FactorPacer) -> usize {
    std::iter::from_fn(|| p.poll_write()).count()
  }

  /// The estimates the packets read through it carry to the stats.
  fn reported(p: &mut FactorPacer) -> Vec<f64> {
    std::iter::from_fn(|| p.poll_read())
      .filter_map(|out| FactorPacer::estimate(&out))
      .collect()
  }

  // A congestion controller estimate paces at PACING_FACTOR times it, and the
  // feedback that carried it goes on with the estimate itself (what the stats
  // report).
  #[test]
  fn paces_at_the_factor_times_the_estimate() {
    let mut p = pacer();
    p.bind_local_stream(&twcc_stream(SSRC));
    assert_eq!(p.target_bitrate(), GCC_INITIAL_BITRATE * PACING_FACTOR);
    p.handle_read(feedback(Some(400_000.0))).unwrap();
    assert_eq!(p.target_bitrate(), 400_000.0 * PACING_FACTOR);
    assert_eq!(reported(&mut p), vec![400_000.0]);
  }

  // Feedback without an estimate leaves the rate alone.
  #[test]
  fn keeps_the_rate_without_an_estimate() {
    let mut p = pacer();
    p.bind_local_stream(&twcc_stream(SSRC));
    p.handle_read(feedback(Some(500_000.0))).unwrap();
    p.handle_read(feedback(None)).unwrap();
    assert_eq!(p.target_bitrate(), 500_000.0 * PACING_FACTOR);
  }

  // A stream that negotiated neither TWCC nor `ack ccfb` is not paced: the
  // remote never reports on it, so the estimate would never leave its
  // initial value (PR-02: every such connection was held near 750 kb/s).
  // Its packets come straight out, however fast they are written.
  #[test]
  fn passes_a_stream_without_feedback_through() {
    let mut p = pacer();
    p.bind_local_stream(&StreamInfo {
      ssrc: SSRC,
      ..Default::default()
    });
    let now = Instant::now();
    assert_eq!(write(&mut p, now, SSRC, 500), 500);
    // Seconds later, still nothing held.
    assert_eq!(write(&mut p, now + Duration::from_secs(10), SSRC, 500), 500);
    assert_eq!(p.refusals.get(), (0, 0));
    // No estimate reaches the stats.
    assert!(reported(&mut p).is_empty());
  }

  // An unbound SSRC (no stream negotiated anything) passes through too.
  #[test]
  fn passes_an_unbound_stream_through() {
    let mut p = pacer();
    assert_eq!(write(&mut p, Instant::now(), SSRC, 100), 100);
  }

  // A stream that negotiated TWCC is paced from its first packet, at the
  // initial rate: the packets wait for the pacer's budget.
  #[test]
  fn paces_a_stream_that_negotiated_twcc() {
    let mut p = pacer();
    p.bind_local_stream(&twcc_stream(SSRC));
    let now = Instant::now();
    assert!(write(&mut p, now, SSRC, 100) <= BURST);
    p.handle_timeout(now).unwrap();
    let released = drain(&mut p);
    assert!(released < 100, "released {released}");
  }

  // RFC 8888 feedback (`ack ccfb`) counts as congestion feedback too.
  #[test]
  fn paces_a_stream_that_negotiated_ccfb() {
    let mut p = pacer();
    p.bind_local_stream(&ccfb_stream(SSRC));
    let now = Instant::now();
    assert!(write(&mut p, now, SSRC, 10) <= BURST);
    // An RFC 8888 report keeps it paced past the feedback timeout.
    let later = now + FEEDBACK_TIMEOUT - Duration::from_millis(100);
    p.handle_read(ccfb(later)).unwrap();
    assert!(
      write(&mut p, later + Duration::from_millis(500), SSRC, 10) <= BURST
    );
  }

  // Only the negotiated stream is paced: another stream of the same
  // connection that negotiated nothing passes beside it.
  #[test]
  fn paces_only_the_negotiated_streams() {
    let mut p = pacer();
    p.bind_local_stream(&twcc_stream(SSRC));
    p.bind_local_stream(&StreamInfo {
      ssrc: SSRC + 1,
      ..Default::default()
    });
    let now = Instant::now();
    assert!(write(&mut p, now, SSRC, 10) <= BURST);
    assert_eq!(write(&mut p, now, SSRC + 1, 10), 10);
  }

  // While reports keep coming, it keeps pacing, however long the call.
  #[test]
  fn keeps_pacing_while_feedback_arrives() {
    let mut p = pacer();
    p.bind_local_stream(&twcc_stream(SSRC));
    let start = Instant::now();
    for i in 0..20 {
      let now = start + Duration::from_millis(250) * i;
      p.handle_read(twcc(now, None)).unwrap();
      assert!(write(&mut p, now, SSRC, 5) <= BURST, "at {i}");
      p.handle_timeout(now).unwrap();
      drain(&mut p);
    }
  }

  // Negotiated, but no report within FEEDBACK_TIMEOUT of the first packet:
  // what was held goes at the next wake-up and later packets pass straight
  // through; the next report resumes pacing at its estimate.
  #[test]
  fn falls_back_without_feedback_then_resumes() {
    let mut p = pacer();
    p.bind_local_stream(&twcc_stream(SSRC));
    let start = Instant::now();
    let first = write(&mut p, start, SSRC, 100);
    assert!(first <= BURST);
    let before = start + FEEDBACK_TIMEOUT - Duration::from_millis(1);
    let second = write(&mut p, before, SSRC, 1);
    assert!(second <= BURST, "still paced");
    let after = start + FEEDBACK_TIMEOUT;
    p.handle_timeout(after).unwrap();
    assert_eq!(
      drain(&mut p),
      101 - first - second,
      "the held packets all go"
    );
    assert_eq!(write(&mut p, after, SSRC, 200), 200);
    // A report: paced again, at its estimate.
    let resumed = after + Duration::from_secs(5);
    p.handle_read(twcc(resumed, Some(400_000.0))).unwrap();
    assert_eq!(p.target_bitrate(), 400_000.0 * PACING_FACTOR);
    let third = write(&mut p, resumed, SSRC, 10);
    assert!(third <= BURST);
    // And reports that stop after that fall back the same way.
    let stopped = resumed + FEEDBACK_TIMEOUT;
    p.handle_timeout(stopped).unwrap();
    assert_eq!(drain(&mut p), 10 - third);
    assert_eq!(write(&mut p, stopped, SSRC, 10), 10);
  }

  // A packet written while the held ones haven't gone yet waits behind them,
  // so the stream keeps its order.
  #[test]
  fn keeps_a_streams_order_when_falling_back() {
    let mut p = pacer();
    p.bind_local_stream(&twcc_stream(SSRC));
    let start = Instant::now();
    let first = write(&mut p, start, SSRC, 50);
    assert!(first <= BURST);
    let after = start + FEEDBACK_TIMEOUT;
    p.handle_write(rtp(after, SSRC, 50)).unwrap();
    p.handle_timeout(after).unwrap();
    let order: Vec<u16> = std::iter::from_fn(|| p.poll_write())
      .map(|out| match out.message.packet {
        Packet::Rtp(rtp) => rtp.header.sequence_number,
        _ => unreachable!("only RTP was written"),
      })
      .collect();
    assert_eq!(order, (first as u16..=50).collect::<Vec<_>>());
  }

  // The stats hold an estimate only while it paces at one: the initial
  // estimate with the first report, the controller's after that, none once
  // reports stop (`availableOutgoingBitrate` absent, `targetBitrate` 0), and
  // the last one again with the next report.
  #[test]
  fn reports_an_estimate_only_while_pacing() {
    let mut p = pacer();
    p.bind_local_stream(&twcc_stream(SSRC));
    let start = Instant::now();
    write(&mut p, start, SSRC, 1);
    assert!(reported(&mut p).is_empty(), "nothing before a report");
    let first = start + Duration::from_millis(100);
    p.handle_read(twcc(first, None)).unwrap();
    assert_eq!(reported(&mut p), vec![GCC_INITIAL_BITRATE]);
    p.handle_read(twcc(first, Some(500_000.0))).unwrap();
    assert_eq!(reported(&mut p), vec![500_000.0]);
    p.handle_read(twcc(first, None)).unwrap();
    assert!(reported(&mut p).is_empty(), "unchanged: nothing new");
    p.handle_timeout(first + FEEDBACK_TIMEOUT).unwrap();
    let carrier = p.poll_read().expect("the estimate is withdrawn");
    assert!(
      matches!(&carrier.message.packet, Packet::Rtcp(packets) if packets.is_empty())
    );
    assert_eq!(FactorPacer::estimate(&carrier), Some(0.0));
    assert!(p.poll_read().is_none());
    let back = first + FEEDBACK_TIMEOUT * 2;
    p.handle_read(twcc(back, None)).unwrap();
    assert_eq!(reported(&mut p), vec![500_000.0]);
  }

  // A packet the full queue refuses is counted, with its bytes, for the
  // candidate pair's `packetsDiscardedOnSend` / `bytesDiscardedOnSend`.
  #[test]
  fn counts_what_a_full_queue_refuses() {
    let refusals = Arc::new(PacerRefusals::default());
    let mut p = FactorPacer::with_pacer(
      PacerBuilder::new()
        .with_target_bitrate(GCC_INITIAL_BITRATE * PACING_FACTOR)
        .with_queue_time_limit(None)
        .with_queue_limit(4)
        .build(),
      refusals.clone(),
    );
    p.bind_local_stream(&twcc_stream(SSRC));
    let now = Instant::now();
    let released = write(&mut p, now, SSRC, 10);
    assert!(released <= BURST);
    let size = match &rtp(now, SSRC, 0).message.packet {
      Packet::Rtp(rtp) => rtp.marshal_size() as u64,
      _ => unreachable!("an RTP packet"),
    };
    // Ten written: what left at once, four queued, the rest refused.
    let refused = (10 - released - 4) as u64;
    assert_eq!(refusals.get(), (refused, refused * size));
  }

  // A backlog drains within QUEUE_TIME_LIMIT whatever the estimate: 1,000
  // packets (about 8 Mb) at the initial 750 kb/s would take over 10 s; the
  // pacer releases them faster instead, while feedback keeps it pacing.
  #[test]
  fn drains_a_backlog_within_the_queue_time_limit() {
    let mut p = pacer();
    p.bind_local_stream(&twcc_stream(SSRC));
    let start = Instant::now();
    let mut released = write(&mut p, start, SSRC, 1000);
    assert!(released <= BURST);
    let mut now = start;
    while released < 1000 {
      p.handle_read(twcc(now, None)).unwrap();
      p.handle_timeout(now).unwrap();
      released += drain(&mut p);
      if released == 1000 {
        break;
      }
      now = p.poll_timeout().expect("a release is due").max(now);
      assert!(
        now <= start + QUEUE_TIME_LIMIT + Duration::from_millis(100),
        "{released} of 1000 released after {:?}",
        now - start
      );
    }
    // Drained, it paces at the estimate again.
    assert_eq!(p.target_bitrate(), GCC_INITIAL_BITRATE * PACING_FACTOR);
  }

  // Audio is never paced, even where it negotiated feedback: it doesn't wait
  // behind video in the queue.
  #[test]
  fn does_not_pace_audio() {
    let mut p = pacer();
    p.bind_local_stream(&StreamInfo {
      mime_type: "audio/opus".to_string(),
      ..twcc_stream(SSRC)
    });
    p.bind_local_stream(&twcc_stream(SSRC + 1));
    let now = Instant::now();
    assert!(write(&mut p, now, SSRC + 1, 100) <= BURST);
    assert_eq!(write(&mut p, now, SSRC, 50), 50);
  }
}
