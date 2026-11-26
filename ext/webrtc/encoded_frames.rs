// Copyright 2018-2026 the Deno authors. MIT license.

//! Encoded frames for WebRTC Encoded Transform (`RTCRtpScriptTransform` on
//! a receiver): a receiver's RTP packets put back together into the frames
//! they carry, in the order they arrive.
//!
//! There is no jitter buffer: a frame whose packets arrive out of order or
//! with one missing is dropped (the next key frame recovers the stream).
//! AV1 and the G.7xx codecs have no depacketizer in the engine; an AV1
//! frame is not delivered, a G.7xx payload is the frame as it is.

use std::collections::HashMap;

use bytes::Bytes;
use rtci::rtp::packetizer::Depacketizer;
use serde::Serialize;

use crate::WebRtcError;
use crate::peer::Core;

/// What a received frame's metadata names.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ReceivedFrameInfo {
  pub synchronization_source: u32,
  pub contributing_sources: Vec<u32>,
  pub payload_type: u8,
  pub mime_type: String,
  pub rtp_timestamp: u32,
  /// The first packet's (audio frames name it).
  pub sequence_number: u16,
  /// The RFC 6464 audio level (0 loudest to 127 silence) the frame's packet
  /// carried in `ssrc-audio-level`, when that extension was negotiated.
  pub audio_level: Option<u8>,
  /// abs-capture-time, when negotiated and sent: the capture time in Unix
  /// milliseconds, and the sender's estimate of the capture clock's offset
  /// from its own, in milliseconds (the extension's long form only).
  pub capture_time: Option<f64>,
  pub sender_capture_time_offset: Option<f64>,
}

const AUDIO_LEVEL_URI: &str = "urn:ietf:params:rtp-hdrext:ssrc-audio-level";
const ABS_CAPTURE_TIME_URI: &str =
  "http://www.webrtc.org/experiments/rtp-hdrext/abs-capture-time";
/// Seconds from the NTP epoch (1900) to the Unix epoch (1970).
const NTP_UNIX_OFFSET_SECS: f64 = 2_208_988_800.0;
const TWO_POW_32: f64 = 4_294_967_296.0;

/// RFC 6464 3: the V flag, then the level in -dBov.
fn audio_level(payload: &[u8]) -> Option<u8> {
  payload.first().map(|b| b & 0x7f)
}

/// abs-capture-time: a 64-bit NTP timestamp (UQ32.32), then optionally the
/// estimated capture clock offset (Q32.32, signed). Returns them in Unix
/// milliseconds and milliseconds.
fn abs_capture_time(payload: &[u8]) -> Option<(f64, Option<f64>)> {
  let ntp = u64::from_be_bytes(<[u8; 8]>::try_from(payload.get(..8)?).ok()?);
  let seconds = (ntp >> 32) as f64 - NTP_UNIX_OFFSET_SECS;
  let fraction = (ntp & 0xffff_ffff) as f64 / TWO_POW_32;
  let offset = payload
    .get(8..16)
    .and_then(|b| <[u8; 8]>::try_from(b).ok())
    .map(|b| i64::from_be_bytes(b) as f64 / TWO_POW_32 * 1000.0);
  Some(((seconds + fraction) * 1000.0, offset))
}

/// What the packets of the frame being put together carried in header
/// extensions (the first value of each).
#[derive(Default, Clone, Copy, Debug)]
struct FrameExtensions {
  audio_level: Option<u8>,
  capture_time: Option<f64>,
  sender_capture_time_offset: Option<f64>,
}

/// A received frame's type: the bitstream's own key frame flag. A VP8 key
/// frame also starts with the start code (RFC 6386 9.1), which tells a key
/// frame from a payload a transform replaced.
fn received_key_frame(mime_type: &str, data: &[u8]) -> bool {
  if mime_type.eq_ignore_ascii_case("video/vp8") {
    return data.len() >= 6
      && data[0] & 1 == 0
      && data[3..6] == [0x9d, 0x01, 0x2a];
  }
  crate::media::is_key_frame(mime_type, data)
}

/// A received frame, for a receiver's transform.
#[derive(Debug)]
pub struct ReceivedFrame {
  pub data: Vec<u8>,
  pub key_frame: bool,
  pub width_height: Option<(u16, u16)>,
  pub info: ReceivedFrameInfo,
}

fn depacketizer_for(mime: &str) -> Option<Box<dyn Depacketizer + Send>> {
  use rtci::rtp::codec;
  Some(match mime.to_ascii_lowercase().as_str() {
    "audio/opus" => Box::new(codec::opus::OpusPacket),
    "video/vp8" => Box::<codec::vp8::Vp8Packet>::default(),
    "video/vp9" => Box::<codec::vp9::Vp9Packet>::default(),
    "video/h264" => Box::<codec::h264::H264Packet>::default(),
    "video/h265" => Box::<codec::h265::H265Packet>::default(),
    _ => return None,
  })
}

/// One receiver's frame in the making.
pub struct FrameAssembler {
  payload_type: u8,
  mime_type: String,
  depacketizer: Option<Box<dyn Depacketizer + Send>>,
  audio: bool,
  /// The frame being put together: its timestamp, first sequence number,
  /// the next sequence number expected, and its bytes so far.
  current: Option<(u32, u16, u16, Vec<u8>)>,
  /// The negotiated ids of `ssrc-audio-level` and abs-capture-time.
  audio_level_id: Option<u8>,
  abs_capture_time_id: Option<u8>,
  /// What the current frame's packets carried of them.
  current_extensions: FrameExtensions,
  /// The sequence number the stream's next packet should have, across
  /// frames (the furthest seen plus one): a packet past it means packets
  /// were lost, even between frames.
  expected: Option<u16>,
  /// A video stream lost a frame (a sequence gap, a frame in the making
  /// dropped, a payload the depacketizer refused): what follows references
  /// it, so the stream stays broken until a key frame is put together.
  lost: bool,
}

impl FrameAssembler {
  pub fn new(payload_type: u8, mime_type: &str) -> Self {
    Self {
      payload_type,
      mime_type: mime_type.to_string(),
      depacketizer: depacketizer_for(mime_type),
      audio: mime_type.to_ascii_lowercase().starts_with("audio/"),
      current: None,
      audio_level_id: None,
      abs_capture_time_id: None,
      current_extensions: FrameExtensions::default(),
      expected: None,
      lost: false,
    }
  }

  /// Whether the video stream lost a frame and no key frame has repaired it
  /// since: the receiver should ask the sender for one (a PLI).
  pub fn needs_key_frame(&self) -> bool {
    self.lost
  }

  /// A frame was lost: a video stream is broken until its next key frame.
  fn lose(&mut self) {
    if !self.audio {
      self.lost = true;
    }
  }

  /// Notes the packet's sequence number against the one expected; returns
  /// whether packets before it are missing. A late packet (behind the
  /// furthest seen) moves nothing.
  fn note_sequence(&mut self, seq: u16) -> bool {
    let Some(expected) = self.expected else {
      self.expected = Some(seq.wrapping_add(1));
      return false;
    };
    let ahead = seq.wrapping_sub(expected) as i16;
    if ahead >= 0 {
      self.expected = Some(seq.wrapping_add(1));
    }
    ahead > 0
  }

  /// The header extension ids the receiver negotiated, whose values the
  /// frames' metadata name.
  pub fn with_extensions(
    mut self,
    audio_level_id: Option<u8>,
    abs_capture_time_id: Option<u8>,
  ) -> Self {
    self.audio_level_id = audio_level_id;
    self.abs_capture_time_id = abs_capture_time_id;
    self
  }

  fn note_extensions(&mut self, header: &rtci::rtp::Header) {
    let ext = &mut self.current_extensions;
    if ext.audio_level.is_none()
      && let Some(id) = self.audio_level_id
      && let Some(payload) = header.get_extension(id)
    {
      ext.audio_level = audio_level(&payload);
    }
    if ext.capture_time.is_none()
      && let Some(id) = self.abs_capture_time_id
      && let Some(payload) = header.get_extension(id)
      && let Some((time, offset)) = abs_capture_time(&payload)
    {
      ext.capture_time = Some(time);
      ext.sender_capture_time_offset = offset;
    }
  }

  /// Takes one RTP packet; returns the frame it completes.
  pub fn push(
    &mut self,
    header: &rtci::rtp::Header,
    payload: &Bytes,
  ) -> Option<ReceivedFrame> {
    let mime = self.mime_type.to_ascii_lowercase();
    if self.depacketizer.is_none() && !mime.starts_with("audio/") {
      return None; // AV1 or an unknown video codec.
    }
    let seq = header.sequence_number;
    // Packets missing before this one: whatever they carried (the end of the
    // frame in the making, or whole frames between) is lost.
    if self.note_sequence(seq) {
      self.lose();
    }
    // A packet without a payload (padding, or a ULPFEC packet of a RED
    // stream, which the engine delivers emptied under the media's payload
    // type) is present and no media: it takes the sequence number the frame
    // in the making expects and starts nothing.
    if payload.is_empty() {
      match &mut self.current {
        Some((_, _, next, _)) if *next == seq => *next = seq.wrapping_add(1),
        Some(_) => {
          self.current = None;
          self.lose();
        }
        None => {}
      }
      return None;
    }
    // A packet of another frame, or a gap: what was being built is lost.
    if let Some((ts, _, next, _)) = &self.current
      && (*ts != header.timestamp || *next != seq)
    {
      self.current = None;
      self.lose();
    }
    let head = match &self.depacketizer {
      Some(d) => d.is_partition_head(payload),
      None => true,
    };
    if self.current.is_none() {
      if !head {
        return None; // The middle of a frame whose start was lost.
      }
      self.current = Some((header.timestamp, seq, seq, Vec::new()));
      self.current_extensions = FrameExtensions::default();
    }
    let bytes = match &mut self.depacketizer {
      Some(d) => match d.depacketize(payload) {
        Ok(b) => b,
        Err(_) => {
          self.current = None;
          self.lose();
          return None;
        }
      },
      None => payload.clone(),
    };
    self.note_extensions(header);
    let tail = self.audio
      || match &self.depacketizer {
        Some(d) => d.is_partition_tail(header.marker, payload),
        None => header.marker,
      };
    let (ts, first, next, buf) = self.current.as_mut()?;
    buf.extend_from_slice(&bytes);
    *next = seq.wrapping_add(1);
    if !tail {
      return None;
    }
    let (ts, first, data) = (*ts, *first, std::mem::take(buf));
    self.current = None;
    if data.is_empty() {
      return None;
    }
    let ext = self.current_extensions;
    let key_frame = received_key_frame(&self.mime_type, &data);
    if key_frame {
      self.lost = false;
    }
    Some(ReceivedFrame {
      key_frame,
      width_height: crate::media::frame_size(&self.mime_type, &data),
      info: ReceivedFrameInfo {
        synchronization_source: header.ssrc,
        contributing_sources: header.csrc.clone(),
        payload_type: self.payload_type,
        mime_type: self.mime_type.clone(),
        rtp_timestamp: ts,
        sequence_number: first,
        audio_level: ext.audio_level,
        capture_time: ext.capture_time,
        sender_capture_time_offset: ext.sender_capture_time_offset,
      },
      data,
    })
  }
}

/// The receivers whose frames go to a transform, by transceiver key.
#[derive(Default)]
pub struct ReceiverTransforms {
  pub assemblers: HashMap<u32, Option<FrameAssembler>>,
}

impl Core {
  /// `receiver.transform` set (or cleared): its frames are delivered as
  /// `encodedframe` events.
  pub(crate) fn receiver_transform(
    &mut self,
    key: u32,
    on: bool,
  ) -> Result<(), WebRtcError> {
    self.index_of(key)?;
    if on {
      self.transforms.assemblers.entry(key).or_insert(None);
    } else {
      self.transforms.assemblers.remove(&key);
    }
    Ok(())
  }

  /// Whether the receiver's frames for its transform lost one that no key
  /// frame has repaired since.
  pub(crate) fn transform_needs_key_frame(&self, key: u32) -> bool {
    self
      .transforms
      .assemblers
      .get(&key)
      .and_then(Option::as_ref)
      .is_some_and(FrameAssembler::needs_key_frame)
  }

  /// A received RTP packet, for a receiver with a transform: the frame it
  /// completes.
  pub(crate) fn receive_for_transform(
    &mut self,
    key: u32,
    packet: &rtci::rtp::Packet,
  ) -> Option<ReceivedFrame> {
    if !self.transforms.assemblers.contains_key(&key) {
      return None;
    }
    let pt = packet.header.payload_type;
    let stale = match self.transforms.assemblers.get(&key) {
      Some(Some(a)) => a.payload_type != pt,
      _ => true,
    };
    if stale {
      let index = self.index_of(key).ok()?;
      let mut receiver = self.pc.rtp_receiver(index.into())?;
      let params = &receiver.get_parameters().rtp_parameters;
      let mime = params
        .codecs
        .iter()
        .find(|c| c.payload_type == pt)
        .map(|c| c.rtp_codec.mime_type.clone())?;
      let extension_id = |uri: &str| {
        params
          .header_extensions
          .iter()
          .find(|e| e.uri == uri)
          .and_then(|e| u8::try_from(e.id).ok())
      };
      let assembler = FrameAssembler::new(pt, &mime).with_extensions(
        extension_id(AUDIO_LEVEL_URI),
        extension_id(ABS_CAPTURE_TIME_URI),
      );
      self.transforms.assemblers.insert(key, Some(assembler));
    }
    self
      .transforms
      .assemblers
      .get_mut(&key)?
      .as_mut()?
      .push(&packet.header, &packet.payload)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn header(seq: u16, ts: u32, marker: bool) -> rtci::rtp::Header {
    rtci::rtp::Header {
      version: 2,
      sequence_number: seq,
      timestamp: ts,
      marker,
      payload_type: 96,
      ssrc: 1234,
      ..Default::default()
    }
  }

  #[test]
  fn audio_packets_are_frames() {
    let mut a = FrameAssembler::new(111, "audio/opus");
    let f = a
      .push(
        &header(7, 960, true),
        &Bytes::from_static(&[0xf8, 0xff, 0xfe]),
      )
      .unwrap();
    assert_eq!(f.data, vec![0xf8, 0xff, 0xfe]);
    assert_eq!(f.info.rtp_timestamp, 960);
    assert_eq!(f.info.sequence_number, 7);
    assert_eq!(f.info.synchronization_source, 1234);
    assert!(!f.key_frame);
  }

  #[test]
  fn vp8_frames_are_put_together_and_gaps_drop_them() {
    // VP8 payload descriptor 0x10 (S=1, start of partition) then 0x00. The
    // engine's depacketizer takes payloads of 4 bytes or more.
    let mut a = FrameAssembler::new(96, "video/VP8");
    let key = [0x10, 0, 0, 0, 0x9d, 0x01, 0x2a, 0x80, 0x02, 0xe0, 0x01];
    assert!(
      a.push(&header(1, 90, false), &Bytes::copy_from_slice(&key))
        .is_none()
    );
    let f = a
      .push(
        &header(2, 90, true),
        &Bytes::from_static(&[0x00, 0xaa, 0xbb, 0xcc]),
      )
      .unwrap();
    assert_eq!(
      f.data,
      vec![
        0, 0, 0, 0x9d, 0x01, 0x2a, 0x80, 0x02, 0xe0, 0x01, 0xaa, 0xbb, 0xcc
      ]
    );
    assert!(f.key_frame);
    assert_eq!(f.width_height, Some((640, 480)));
    // Sequence 4 follows 2: 3 was lost, the frame is dropped.
    let start = Bytes::from_static(&[0x10, 0x01, 0x02, 0x03]);
    assert!(a.push(&header(4, 180, false), &start).is_none());
    let end = Bytes::from_static(&[0x00, 0x02, 0x03, 0x04]);
    assert!(a.push(&header(6, 180, true), &end).is_none());
  }

  #[test]
  fn a_packet_without_a_payload_is_present_and_no_media() {
    // rtci 0.2 delivers a RED stream's ULPFEC packet emptied, with a
    // sequence number of the media stream: inside a frame it is no gap.
    let mut a = FrameAssembler::new(96, "video/VP8");
    let start = Bytes::from_static(&[0x10, 0x01, 0x02, 0x03]);
    assert!(a.push(&header(1, 90, false), &start).is_none());
    assert!(a.push(&header(2, 5000, false), &Bytes::new()).is_none());
    let end = Bytes::from_static(&[0x00, 0x04, 0x05, 0x06]);
    let f = a.push(&header(3, 90, true), &end).unwrap();
    assert_eq!(f.data, vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
    assert_eq!(f.info.sequence_number, 1);
    // Between frames it starts nothing, and the next frame is whole.
    assert!(a.push(&header(4, 90, true), &Bytes::new()).is_none());
    assert!(a.push(&header(5, 180, false), &start).is_none());
    assert!(a.push(&header(6, 180, true), &end).is_some());
    // After a gap it is no repair: the frame in the making is lost.
    assert!(a.push(&header(7, 270, false), &start).is_none());
    assert!(a.push(&header(9, 270, false), &Bytes::new()).is_none());
    assert!(a.push(&header(10, 270, true), &end).is_none());
  }

  #[test]
  fn a_lost_frame_needs_a_key_frame_until_one_arrives() {
    let key = Bytes::from_static(&[
      0x10, 0, 0, 0, 0x9d, 0x01, 0x2a, 0x80, 0x02, 0xe0, 0x01,
    ]);
    let delta = Bytes::from_static(&[0x10, 0x01, 0x02, 0x03]);
    let mut a = FrameAssembler::new(96, "video/VP8");
    assert!(a.push(&header(1, 90, true), &key).unwrap().key_frame);
    assert!(a.push(&header(2, 180, true), &delta).is_some());
    assert!(!a.needs_key_frame());
    // A whole one-packet frame lost between frames: the next is delivered,
    // but it references the lost one.
    assert!(a.push(&header(4, 360, true), &delta).is_some());
    assert!(a.needs_key_frame());
    // Delta frames keep it broken.
    assert!(a.push(&header(5, 450, true), &delta).is_some());
    assert!(a.needs_key_frame());
    // A late packet moves nothing back, and is no new loss once repaired.
    assert!(a.push(&header(6, 540, true), &key).unwrap().key_frame);
    assert!(!a.needs_key_frame());
    assert!(a.push(&header(3, 270, true), &delta).is_some());
    assert!(!a.needs_key_frame());
    assert!(a.push(&header(7, 630, true), &delta).is_some());
    assert!(!a.needs_key_frame());
    // The end of a frame lost: the frame in the making is dropped.
    let start = Bytes::from_static(&[0x10, 0x01, 0x02, 0x03]);
    let end = Bytes::from_static(&[0x00, 0x04, 0x05, 0x06]);
    assert!(a.push(&header(8, 720, false), &start).is_none());
    assert!(a.push(&header(10, 810, true), &delta).is_some());
    assert!(a.needs_key_frame());
    assert!(a.push(&header(11, 900, true), &key).unwrap().key_frame);
    assert!(!a.needs_key_frame());
    // Its start lost: the middle of a frame is no frame.
    assert!(a.push(&header(13, 990, true), &end).is_none());
    assert!(a.needs_key_frame());
  }

  #[test]
  fn audio_never_needs_a_key_frame() {
    let mut a = FrameAssembler::new(111, "audio/opus");
    let opus = Bytes::from_static(&[0xf8, 0xff, 0xfe]);
    assert!(a.push(&header(1, 960, true), &opus).is_some());
    assert!(a.push(&header(5, 4800, true), &opus).is_some());
    assert!(!a.needs_key_frame());
  }

  #[test]
  fn a_vp8_payload_without_the_start_code_is_not_a_key_frame() {
    // A transform replaced the frame with zeros: bit 0 is clear, but there
    // is no start code.
    assert!(!received_key_frame("video/VP8", &[0; 16]));
    assert!(received_key_frame(
      "video/vp8",
      &[0, 0, 0, 0x9d, 0x01, 0x2a, 0x80, 0x02, 0xe0, 0x01]
    ));
  }

  #[test]
  fn audio_level_and_abs_capture_time_are_read() {
    let mut a =
      FrameAssembler::new(111, "audio/opus").with_extensions(Some(1), Some(2));
    // 2026-01-01T00:00:00Z is Unix 1767225600 s; half a second more.
    let ntp_seconds: u64 = 1_767_225_600 + 2_208_988_800;
    let ntp = (ntp_seconds << 32) | 0x8000_0000;
    let mut abs = ntp.to_be_bytes().to_vec();
    // An offset of -0.25 s in Q32.32.
    abs.extend_from_slice(&(-(1i64 << 30)).to_be_bytes());
    let mut h = header(9, 1920, true);
    h.extension = true;
    h.extension_profile = 0xBEDE;
    h.extensions = vec![
      rtci::rtp::header::Extension {
        id: 1,
        payload: Bytes::from_static(&[0x80 | 30]),
      },
      rtci::rtp::header::Extension {
        id: 2,
        payload: Bytes::from(abs),
      },
    ];
    let f = a
      .push(&h, &Bytes::from_static(&[0xf8, 0xff, 0xfe]))
      .unwrap();
    assert_eq!(f.info.audio_level, Some(30));
    assert_eq!(f.info.capture_time, Some(1_767_225_600_500.0));
    assert_eq!(f.info.sender_capture_time_offset, Some(-250.0));
    // Without the extensions negotiated, none is named.
    let mut b = FrameAssembler::new(111, "audio/opus");
    let f = b
      .push(&h, &Bytes::from_static(&[0xf8, 0xff, 0xfe]))
      .unwrap();
    assert_eq!(f.info.audio_level, None);
    assert_eq!(f.info.capture_time, None);
  }
}
