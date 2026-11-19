// Copyright 2018-2026 the Deno authors. MIT license.

//! What a remote track really receives, for `Deno.webrtc.readFrames()` and
//! for the track's own state:
//!
//! - its frames, put back together from RTP (the encoded transform's
//!   `FrameAssembler`), while a `readFrames()` reader reads them;
//! - the coded size of a video track's frames, read from the bitstream (a
//!   VP8 key frame header, a VP9 uncompressed header, an H.264 / H.265 SPS),
//!   for `getSettings()` and each frame's `width` / `height`;
//! - silence: a receiver whose RTP stops for [`SILENCE`] mutes its track
//!   (mediacapture-main 4.3.1.1: "muted ... the source is temporarily unable
//!   to provide the track with data"), and its next packet unmutes it again.
//!
//! There is no decoder: a frame is what arrived, encoded. AV1 frames are not
//! delivered (the engine has no AV1 depacketizer), nor sized.

use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;

use bytes::Bytes;
use rtc::rtp::packetizer::Depacketizer;

use crate::WebRtcError;
use crate::encoded_frames::FrameAssembler;
use crate::peer::Core;
use crate::peer::JsEvent;

/// How long a receiver may go without RTP before its track mutes. The fake
/// devices and every real encoder send at least every 400 ms (Opus DTX's
/// comfort noise is the sparsest), so a gap this long means the remote side
/// stopped sending: a stopped transceiver, a closed connection, an inactive
/// encoding or a direction without `send`.
pub const SILENCE: Duration = Duration::from_millis(2000);

/// A received frame for `readFrames()` readers.
#[derive(Debug)]
pub struct SinkFrame {
  pub data: Vec<u8>,
  pub key_frame: bool,
  pub mime_type: String,
  pub rtp_timestamp: u32,
  /// The stream's coded size (video), once a key frame or SPS named it.
  pub width_height: Option<(u32, u32)>,
}

/// What a received packet changed for its receiver.
#[derive(Debug, Default)]
pub struct SinkOutput {
  /// The video's coded size changed to this.
  pub resize: Option<(u32, u32)>,
  /// The packet completed a frame a reader wants.
  pub frame: Option<SinkFrame>,
}

/// The receivers' sinks, by transceiver key.
#[derive(Default)]
pub struct MediaSinks {
  /// Receivers whose frames `readFrames()` reads, with the frame in the
  /// making (`None` until a packet names the payload type).
  assemblers: HashMap<u32, Option<(u8, FrameAssembler)>>,
  /// Each receiver's last payload type and its codec's MIME type.
  codecs: HashMap<u32, (u8, String)>,
  /// When each flowing receiver's last RTP packet arrived.
  last_packet: HashMap<u32, Instant>,
  /// The coded size each video receiver's frames have.
  sizes: HashMap<u32, (u32, u32)>,
}

impl MediaSinks {
  /// The next time a flowing receiver goes silent.
  pub fn silence_deadline(&self) -> Option<Instant> {
    self.last_packet.values().min().map(|t| *t + SILENCE)
  }
}

impl Core {
  /// `readFrames()` started (or its last reader stopped) on a receiver.
  pub(crate) fn receiver_read_frames(
    &mut self,
    key: u32,
    on: bool,
  ) -> Result<(), WebRtcError> {
    self.index_of(key)?;
    if on {
      self.sinks.assemblers.entry(key).or_insert(None);
    } else {
      self.sinks.assemblers.remove(&key);
    }
    Ok(())
  }

  /// The receiver's track was muted from JavaScript (its remote side
  /// stopped sending, by negotiation or silence): the next packet unmutes it.
  pub(crate) fn receiver_muted(&mut self, key: u32) {
    self.media.flowing.remove(&key);
    self.sinks.last_packet.remove(&key);
  }

  /// Mutes the tracks of receivers that went silent (an event each).
  pub(crate) fn check_silence(&mut self, now: Instant) {
    let silent: Vec<u32> = self
      .sinks
      .last_packet
      .iter()
      .filter(|(_, t)| now.saturating_duration_since(**t) >= SILENCE)
      .map(|(k, _)| *k)
      .collect();
    for key in silent {
      self.receiver_muted(key);
      self
        .events
        .push_back(JsEvent::TrackMute { transceiver: key });
    }
  }

  /// A received RTP packet: its arrival, the video's size, and the frame it
  /// completes for a reader.
  pub(crate) fn sink_packet(
    &mut self,
    key: u32,
    packet: &rtc::rtp::Packet,
    now: Instant,
  ) -> SinkOutput {
    let mut out = SinkOutput::default();
    self.sinks.last_packet.insert(key, now);
    let pt = packet.header.payload_type;
    let known = self.sinks.codecs.get(&key).is_some_and(|(p, _)| *p == pt);
    if !known {
      let Some(mime) = self.receiver_mime_type(key, pt) else {
        return out;
      };
      self.sinks.codecs.insert(key, (pt, mime));
    }
    let Some(mime) = self.sinks.codecs.get(&key).map(|(_, m)| m.clone()) else {
      return out;
    };
    if let Some(size) = packet_coded_size(&mime, &packet.payload)
      && self.sinks.sizes.get(&key) != Some(&size)
    {
      self.sinks.sizes.insert(key, size);
      out.resize = Some(size);
    }
    let Some(slot) = self.sinks.assemblers.get_mut(&key) else {
      return out;
    };
    if slot.as_ref().is_none_or(|(p, _)| *p != pt) {
      *slot = Some((pt, FrameAssembler::new(pt, &mime)));
    }
    let Some((_, assembler)) = slot.as_mut() else {
      return out;
    };
    let Some(frame) = assembler.push(&packet.header, &packet.payload) else {
      return out;
    };
    // A key frame names its size itself (an SPS sent in fragments, which
    // the packets alone don't show).
    if frame.key_frame
      && let Some(size) = frame_coded_size(&mime, &frame.data)
      && self.sinks.sizes.get(&key) != Some(&size)
    {
      self.sinks.sizes.insert(key, size);
      out.resize = Some(size);
    }
    out.frame = Some(SinkFrame {
      key_frame: frame.key_frame,
      mime_type: mime,
      rtp_timestamp: frame.info.rtp_timestamp,
      width_height: self.sinks.sizes.get(&key).copied(),
      data: frame.data,
    });
    out
  }

  /// The MIME type of a receiver's payload type, from its negotiated codecs.
  fn receiver_mime_type(&mut self, key: u32, pt: u8) -> Option<String> {
    let index = self.index_of(key).ok()?;
    let mut receiver = self.pc.rtp_receiver(index.into())?;
    receiver
      .get_parameters()
      .rtp_parameters
      .codecs
      .iter()
      .find(|c| c.payload_type == pt)
      .map(|c| c.rtp_codec.mime_type.clone())
  }
}

/// The coded size a video RTP packet's payload names, when it carries the
/// start of a VP8 / VP9 key frame or an H.264 / H.265 SPS.
pub fn packet_coded_size(
  mime_type: &str,
  payload: &Bytes,
) -> Option<(u32, u32)> {
  use rtc::rtp::codec;
  match mime_type.to_ascii_lowercase().as_str() {
    "video/vp8" => {
      let mut p = codec::vp8::Vp8Packet::default();
      let frame = p.depacketize(payload).ok()?;
      if p.s == 0 || p.pid != 0 {
        return None;
      }
      crate::media::frame_size("video/vp8", &frame)
        .map(|(w, h)| (w as u32, h as u32))
    }
    "video/vp9" => {
      let mut p = codec::vp9::Vp9Packet::default();
      let frame = p.depacketize(payload).ok()?;
      if p.b
        && let Some(size) = vp9_frame_size(&frame)
      {
        return Some(size);
      }
      // A scalability structure names each spatial layer's size; the
      // highest layer is the one shown.
      if p.v && p.y {
        let w = *p.width.last()?;
        let h = *p.height.last()?;
        return Some((w as u32, h as u32));
      }
      None
    }
    "video/h264" => {
      let first = *payload.first()?;
      match first & 0x1f {
        7 => h264_sps_size(payload),
        // STAP-A: NAL units, each after its 16-bit size.
        24 => aggregated_nal_units(&payload[1..])
          .find(|n| n.first().is_some_and(|b| b & 0x1f == 7))
          .and_then(h264_sps_size),
        _ => None,
      }
    }
    "video/h265" => {
      let first = *payload.first()?;
      match (first >> 1) & 0x3f {
        33 => h265_sps_size(payload),
        // An aggregation packet: a 2-byte header, then NAL units each
        // after its 16-bit size (no DONL: sprop-max-don-diff is never
        // negotiated here).
        48 => aggregated_nal_units(payload.get(2..)?)
          .find(|n| n.first().is_some_and(|b| (b >> 1) & 0x3f == 33))
          .and_then(h265_sps_size),
        _ => None,
      }
    }
    _ => None,
  }
}

/// The coded size an encoded video frame names (a key frame of VP8 or VP9,
/// or an Annex B access unit with an H.264 / H.265 SPS).
pub fn frame_coded_size(mime_type: &str, data: &[u8]) -> Option<(u32, u32)> {
  match mime_type.to_ascii_lowercase().as_str() {
    "video/vp8" => crate::media::frame_size("video/vp8", data)
      .map(|(w, h)| (w as u32, h as u32)),
    "video/vp9" => vp9_frame_size(data),
    "video/h264" => annex_b_nal_units(data)
      .find(|n| n.first().is_some_and(|b| b & 0x1f == 7))
      .and_then(h264_sps_size),
    "video/h265" => annex_b_nal_units(data)
      .find(|n| n.first().is_some_and(|b| (b >> 1) & 0x3f == 33))
      .and_then(h265_sps_size),
    _ => None,
  }
}

/// The NAL units of an aggregation packet's body: `<u16 size><NAL unit>`...
fn aggregated_nal_units(mut rest: &[u8]) -> impl Iterator<Item = &[u8]> {
  std::iter::from_fn(move || {
    if rest.len() < 2 {
      return None;
    }
    let size = u16::from_be_bytes([rest[0], rest[1]]) as usize;
    if rest.len() < 2 + size {
      return None;
    }
    let nal = &rest[2..2 + size];
    rest = &rest[2 + size..];
    Some(nal)
  })
}

/// The NAL units of an Annex B stream.
fn annex_b_nal_units(data: &[u8]) -> impl Iterator<Item = &[u8]> {
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
  let mut ends: Vec<usize> = starts.iter().skip(1).map(|s| s - 3).collect();
  ends.push(data.len());
  starts.into_iter().zip(ends).map(move |(a, b)| {
    // A 4-byte start code leaves a zero byte before the next one.
    let mut end = b.max(a);
    while end > a && data[end - 1] == 0 {
      end -= 1;
    }
    &data[a..end]
  })
}

/// A NAL unit's payload with the emulation prevention bytes taken out
/// (H.264 7.4.1, H.265 7.4.2): `00 00 03` becomes `00 00`.
fn rbsp(nal: &[u8]) -> Vec<u8> {
  let mut out = Vec::with_capacity(nal.len());
  let mut zeros = 0;
  for &b in nal {
    if zeros >= 2 && b == 3 {
      zeros = 0;
      continue;
    }
    zeros = if b == 0 { zeros + 1 } else { 0 };
    out.push(b);
  }
  out
}

/// An MSB-first bit reader with Exp-Golomb codes.
struct Bits<'a> {
  data: &'a [u8],
  pos: usize,
}

impl<'a> Bits<'a> {
  fn new(data: &'a [u8]) -> Self {
    Self { data, pos: 0 }
  }

  fn bit(&mut self) -> Option<u32> {
    let byte = *self.data.get(self.pos / 8)?;
    let bit = (byte >> (7 - (self.pos % 8))) & 1;
    self.pos += 1;
    Some(bit as u32)
  }

  fn bits(&mut self, n: u32) -> Option<u32> {
    let mut v = 0u32;
    for _ in 0..n {
      v = (v << 1) | self.bit()?;
    }
    Some(v)
  }

  fn skip(&mut self, n: usize) -> Option<()> {
    if self.pos + n > self.data.len() * 8 {
      return None;
    }
    self.pos += n;
    Some(())
  }

  /// ue(v).
  fn ue(&mut self) -> Option<u32> {
    let mut zeros = 0u32;
    while self.bit()? == 0 {
      zeros += 1;
      if zeros > 31 {
        return None;
      }
    }
    if zeros == 0 {
      return Some(0);
    }
    let rest = self.bits(zeros)? as u64;
    u32::try_from((1u64 << zeros) - 1 + rest).ok()
  }

  /// se(v).
  fn se(&mut self) -> Option<i64> {
    let k = self.ue()? as i64;
    Some(if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) })
  }
}

/// An H.264 SPS's picture size after cropping (H.264 7.3.2.1.1, 7.4.2.1.1).
/// `nal` starts with the NAL unit header.
pub fn h264_sps_size(nal: &[u8]) -> Option<(u32, u32)> {
  if nal.first()? & 0x1f != 7 {
    return None;
  }
  let data = rbsp(nal.get(1..)?);
  let mut r = Bits::new(&data);
  let profile_idc = r.bits(8)?;
  r.skip(16)?; // constraint flags, level_idc
  r.ue()?; // seq_parameter_set_id
  let mut chroma_format_idc = 1;
  let mut separate_colour_plane = false;
  if matches!(
    profile_idc,
    100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
  ) {
    chroma_format_idc = r.ue()?;
    if chroma_format_idc == 3 {
      separate_colour_plane = r.bit()? == 1;
    }
    r.ue()?; // bit_depth_luma_minus8
    r.ue()?; // bit_depth_chroma_minus8
    r.bit()?; // qpprime_y_zero_transform_bypass_flag
    if r.bit()? == 1 {
      // seq_scaling_matrix_present_flag
      let lists = if chroma_format_idc == 3 { 12 } else { 8 };
      for i in 0..lists {
        if r.bit()? == 1 {
          let size = if i < 6 { 16 } else { 64 };
          let (mut last, mut next) = (8i64, 8i64);
          for _ in 0..size {
            if next != 0 {
              let delta = r.se()?;
              next = (last + delta + 256).rem_euclid(256);
            }
            if next != 0 {
              last = next;
            }
          }
        }
      }
    }
  }
  r.ue()?; // log2_max_frame_num_minus4
  match r.ue()? {
    0 => {
      r.ue()?; // log2_max_pic_order_cnt_lsb_minus4
    }
    1 => {
      r.bit()?; // delta_pic_order_always_zero_flag
      r.se()?; // offset_for_non_ref_pic
      r.se()?; // offset_for_top_to_bottom_field
      let cycle = r.ue()?;
      if cycle > 255 {
        return None;
      }
      for _ in 0..cycle {
        r.se()?; // offset_for_ref_frame
      }
    }
    _ => {}
  }
  r.ue()?; // max_num_ref_frames
  r.bit()?; // gaps_in_frame_num_value_allowed_flag
  let width_mbs = r.ue()? as u64 + 1;
  let height_map_units = r.ue()? as u64 + 1;
  let frame_mbs_only = r.bit()? as u64;
  if frame_mbs_only == 0 {
    r.bit()?; // mb_adaptive_frame_field_flag
  }
  r.bit()?; // direct_8x8_inference_flag
  let (mut left, mut right, mut top, mut bottom) = (0u64, 0u64, 0u64, 0u64);
  if r.bit()? == 1 {
    left = r.ue()? as u64;
    right = r.ue()? as u64;
    top = r.ue()? as u64;
    bottom = r.ue()? as u64;
  }
  let chroma_array_type = if separate_colour_plane {
    0
  } else {
    chroma_format_idc
  };
  let (crop_x, crop_y) = if chroma_array_type == 0 {
    (1, 2 - frame_mbs_only)
  } else {
    let sub_width = if chroma_format_idc == 3 { 1 } else { 2 };
    let sub_height = if chroma_format_idc == 1 { 2 } else { 1 };
    (sub_width, sub_height * (2 - frame_mbs_only))
  };
  let width = (width_mbs * 16).checked_sub(crop_x * (left + right))?;
  let height = ((2 - frame_mbs_only) * height_map_units * 16)
    .checked_sub(crop_y * (top + bottom))?;
  Some((u32::try_from(width).ok()?, u32::try_from(height).ok()?))
}

/// An H.265 SPS's picture size inside its conformance window (H.265
/// 7.3.2.2.1, 7.3.3). `nal` starts with the 2-byte NAL unit header.
pub fn h265_sps_size(nal: &[u8]) -> Option<(u32, u32)> {
  if (nal.first()? >> 1) & 0x3f != 33 {
    return None;
  }
  let data = rbsp(nal.get(2..)?);
  let mut r = Bits::new(&data);
  r.bits(4)?; // sps_video_parameter_set_id
  let max_sub_layers_minus1 = r.bits(3)? as usize;
  r.bit()?; // sps_temporal_id_nesting_flag
  // profile_tier_level(1, sps_max_sub_layers_minus1): the general profile
  // and level (96 bits), then the sub-layers'.
  r.skip(96)?;
  let mut present = Vec::with_capacity(max_sub_layers_minus1);
  for _ in 0..max_sub_layers_minus1 {
    let profile = r.bit()? == 1;
    let level = r.bit()? == 1;
    present.push((profile, level));
  }
  if max_sub_layers_minus1 > 0 {
    r.skip(2 * (8 - max_sub_layers_minus1))?; // reserved_zero_2bits
  }
  for (profile, level) in present {
    if profile {
      r.skip(88)?;
    }
    if level {
      r.skip(8)?;
    }
  }
  r.ue()?; // sps_seq_parameter_set_id
  let chroma_format_idc = r.ue()?;
  let mut separate_colour_plane = false;
  if chroma_format_idc == 3 {
    separate_colour_plane = r.bit()? == 1;
  }
  let width = r.ue()? as u64;
  let height = r.ue()? as u64;
  let (mut left, mut right, mut top, mut bottom) = (0u64, 0u64, 0u64, 0u64);
  if r.bit()? == 1 {
    left = r.ue()? as u64;
    right = r.ue()? as u64;
    top = r.ue()? as u64;
    bottom = r.ue()? as u64;
  }
  let chroma_array_type = if separate_colour_plane {
    0
  } else {
    chroma_format_idc
  };
  let sub_width = if chroma_array_type == 1 || chroma_array_type == 2 {
    2
  } else {
    1
  };
  let sub_height = if chroma_array_type == 1 { 2 } else { 1 };
  let width = width.checked_sub(sub_width * (left + right))?;
  let height = height.checked_sub(sub_height * (top + bottom))?;
  if width == 0 || height == 0 {
    return None;
  }
  Some((u32::try_from(width).ok()?, u32::try_from(height).ok()?))
}

/// A VP9 key frame's size from its uncompressed header (VP9 bitstream
/// specification 6.2: frame_marker, profile, show_existing_frame,
/// frame_type, show_frame, error_resilient_mode, frame_sync_code,
/// color_config, frame_size).
pub fn vp9_frame_size(frame: &[u8]) -> Option<(u32, u32)> {
  let mut r = Bits::new(frame);
  if r.bits(2)? != 2 {
    return None; // frame_marker
  }
  let low = r.bit()?;
  let high = r.bit()?;
  let profile = (high << 1) | low;
  if profile == 3 {
    r.bit()?; // reserved_zero
  }
  if r.bit()? == 1 {
    return None; // show_existing_frame
  }
  if r.bit()? != 0 {
    return None; // frame_type: not a key frame
  }
  r.bit()?; // show_frame
  r.bit()?; // error_resilient_mode
  if r.bits(24)? != 0x49_83_42 {
    return None; // frame_sync_code
  }
  // color_config
  if profile >= 2 {
    r.bit()?; // ten_or_twelve_bit
  }
  let color_space = r.bits(3)?;
  if color_space != 7 {
    r.bit()?; // color_range
    if profile == 1 || profile == 3 {
      r.skip(3)?; // subsampling_x, subsampling_y, reserved_zero
    }
  } else if profile == 1 || profile == 3 {
    r.bit()?; // reserved_zero
  }
  let width = r.bits(16)? + 1;
  let height = r.bits(16)? + 1;
  Some((width, height))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn exp_golomb() {
    // 1 | 010 | 011 | 00100 -> 0, 1, 2, 3
    let data = [0b1010_0110, 0b0100_0000];
    let mut r = Bits::new(&data);
    assert_eq!(r.ue(), Some(0));
    assert_eq!(r.ue(), Some(1));
    assert_eq!(r.ue(), Some(2));
    assert_eq!(r.ue(), Some(3));
    let data = [0b0100_1100];
    let mut r = Bits::new(&data);
    assert_eq!(r.se(), Some(1));
    assert_eq!(r.se(), Some(-1));
  }

  #[test]
  fn emulation_prevention_is_removed() {
    assert_eq!(rbsp(&[0, 0, 3, 1, 0, 0, 3]), vec![0, 0, 1, 0, 0]);
  }

  #[test]
  fn the_fake_clips_are_sized() {
    // The fake camera's clips are 640x480 in every codec (generate.sh).
    for mime in ["video/vp8", "video/vp9", "video/h264", "video/h265"] {
      let (frames, _) = crate::fake_media::clip(mime, false).unwrap();
      assert_eq!(
        frame_coded_size(mime, frames[0]),
        Some((640, 480)),
        "{mime}"
      );
    }
    // A VP8 / VP9 delta frame names no size.
    for mime in ["video/vp8", "video/vp9"] {
      let (frames, _) = crate::fake_media::clip(mime, false).unwrap();
      assert_eq!(frame_coded_size(mime, frames[1]), None, "{mime}");
    }
  }

  #[test]
  fn sizes_from_packets() {
    // A VP8 key frame's first packet: descriptor 0x10 (S=1, PID=0), then
    // the frame tag and the 640x480 key frame header.
    let vp8 = Bytes::from_static(&[
      0x10, 0x50, 0x01, 0x00, 0x9d, 0x01, 0x2a, 0x80, 0x02, 0xe0, 0x01,
    ]);
    assert_eq!(packet_coded_size("video/VP8", &vp8), Some((640, 480)));
    // A delta frame's packet names none.
    let delta = Bytes::from_static(&[0x10, 0x51, 0x01, 0x00, 0x00, 0x00]);
    assert_eq!(packet_coded_size("video/vp8", &delta), None);
    // H.264: the clip's SPS as a single NAL unit packet, and in a STAP-A.
    let (frames, _) = crate::fake_media::clip("video/h264", false).unwrap();
    let sps = annex_b_nal_units(frames[0])
      .find(|n| n[0] & 0x1f == 7)
      .unwrap()
      .to_vec();
    assert_eq!(
      packet_coded_size("video/h264", &Bytes::from(sps.clone())),
      Some((640, 480))
    );
    let mut stap = vec![24u8];
    stap.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    stap.extend_from_slice(&sps);
    assert_eq!(
      packet_coded_size("video/h264", &Bytes::from(stap)),
      Some((640, 480))
    );
  }
}
