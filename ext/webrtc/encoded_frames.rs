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
use rtc::rtp::packetizer::Depacketizer;
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
  use rtc::rtp::codec;
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
}

impl FrameAssembler {
  pub fn new(payload_type: u8, mime_type: &str) -> Self {
    Self {
      payload_type,
      mime_type: mime_type.to_string(),
      depacketizer: depacketizer_for(mime_type),
      audio: mime_type.to_ascii_lowercase().starts_with("audio/"),
      current: None,
    }
  }

  /// Takes one RTP packet; returns the frame it completes.
  pub fn push(
    &mut self,
    header: &rtc::rtp::Header,
    payload: &Bytes,
  ) -> Option<ReceivedFrame> {
    let mime = self.mime_type.to_ascii_lowercase();
    if self.depacketizer.is_none() && !mime.starts_with("audio/") {
      return None; // AV1 or an unknown video codec.
    }
    let seq = header.sequence_number;
    // A packet of another frame, or a gap: what was being built is lost.
    if let Some((ts, _, next, _)) = &self.current
      && (*ts != header.timestamp || *next != seq)
    {
      self.current = None;
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
    }
    let bytes = match &mut self.depacketizer {
      Some(d) => match d.depacketize(payload) {
        Ok(b) => b,
        Err(_) => {
          self.current = None;
          return None;
        }
      },
      None => payload.clone(),
    };
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
    Some(ReceivedFrame {
      key_frame: crate::media::is_key_frame(&self.mime_type, &data),
      width_height: crate::media::frame_size(&self.mime_type, &data),
      info: ReceivedFrameInfo {
        synchronization_source: header.ssrc,
        contributing_sources: header.csrc.clone(),
        payload_type: self.payload_type,
        mime_type: self.mime_type.clone(),
        rtp_timestamp: ts,
        sequence_number: first,
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

  /// A received RTP packet, for a receiver with a transform: the frame it
  /// completes.
  pub(crate) fn receive_for_transform(
    &mut self,
    key: u32,
    packet: &rtc::rtp::Packet,
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
      let mime = receiver
        .get_parameters()
        .rtp_parameters
        .codecs
        .iter()
        .find(|c| c.payload_type == pt)
        .map(|c| c.rtp_codec.mime_type.clone())?;
      self
        .transforms
        .assemblers
        .insert(key, Some(FrameAssembler::new(pt, &mime)));
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

  fn header(seq: u16, ts: u32, marker: bool) -> rtc::rtp::Header {
    rtc::rtp::Header {
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
      .push(&header(7, 960, true), &Bytes::from_static(&[0xf8, 0xff, 0xfe]))
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
    assert!(a.push(&header(1, 90, false), &Bytes::copy_from_slice(&key)).is_none());
    let f = a
      .push(
        &header(2, 90, true),
        &Bytes::from_static(&[0x00, 0xaa, 0xbb, 0xcc]),
      )
      .unwrap();
    assert_eq!(
      f.data,
      vec![0, 0, 0, 0x9d, 0x01, 0x2a, 0x80, 0x02, 0xe0, 0x01, 0xaa, 0xbb, 0xcc]
    );
    assert!(f.key_frame);
    assert_eq!(f.width_height, Some((640, 480)));
    // Sequence 4 follows 2: 3 was lost, the frame is dropped.
    let start = Bytes::from_static(&[0x10, 0x01, 0x02, 0x03]);
    assert!(a.push(&header(4, 180, false), &start).is_none());
    let end = Bytes::from_static(&[0x00, 0x02, 0x03, 0x04]);
    assert!(a.push(&header(6, 180, true), &end).is_none());
  }
}
