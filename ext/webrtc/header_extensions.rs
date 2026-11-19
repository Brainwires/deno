// Copyright 2018-2026 the Deno authors. MIT license.

//! What a sender writes in the RTP header extensions it negotiated, for the
//! ones whose value comes from the media rather than from the transport:
//!
//! - the client-to-mixer audio level (RFC 6464), measured from the PCM of
//!   each frame. The runtime has no audio decoder, so a frame has a level
//!   only where its PCM is at hand: G.711 (PCMU / PCMA) frames are decoded
//!   here, and the fake microphone's mono Opus frames have the same tone in
//!   the PCMU clip `fake_media/generate.sh` made from the same source, frame
//!   for frame. Any other frame (Opus the program encoded, G.722) goes out
//!   without one: the extension is optional per packet;
//! - absolute capture time (abs-capture-time, the 16-byte form): the wall
//!   clock when the frame was handed to the sender, as an NTP timestamp, with
//!   an estimated capture clock offset of 0 (the capture clock is this
//!   machine's).

use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use bytes::Bytes;

/// `http://www.webrtc.org/experiments/rtp-hdrext/abs-capture-time`.
pub const ABS_CAPTURE_TIME_URI: &str =
  "http://www.webrtc.org/experiments/rtp-hdrext/abs-capture-time";

/// Seconds from the NTP epoch (1900) to the Unix epoch (1970).
const NTP_UNIX_OFFSET_SECS: u64 = 2_208_988_800;

/// G.711 mu-law to linear PCM (ITU-T G.711, 14-bit magnitude scaled to 16).
fn ulaw_to_linear(byte: u8) -> i16 {
  let u = !byte;
  let sign = u & 0x80;
  let exponent = (u >> 4) & 0x07;
  let mantissa = (u & 0x0F) as i32;
  let magnitude = (((mantissa << 3) + 0x84) << exponent) - 0x84;
  (if sign != 0 { -magnitude } else { magnitude }) as i16
}

/// G.711 A-law to linear PCM.
fn alaw_to_linear(byte: u8) -> i16 {
  let a = byte ^ 0x55;
  let segment = (a >> 4) & 0x07;
  let mut t = ((a & 0x0F) as i32) << 4;
  match segment {
    0 => t += 8,
    1 => t += 0x108,
    s => {
      t += 0x108;
      t <<= s - 1;
    }
  }
  (if a & 0x80 != 0 { t } else { -t }) as i16
}

/// RFC 6464 section 3: the level in -dBov (0 loudest, 127 silence) of 16-bit
/// PCM, from its RMS against the full scale.
fn level_of(samples: impl Iterator<Item = i16>) -> Option<u8> {
  let mut count = 0usize;
  let mut sum = 0f64;
  for s in samples {
    let v = s as f64 / 32768.0;
    sum += v * v;
    count += 1;
  }
  if count == 0 {
    return None;
  }
  let rms = (sum / count as f64).sqrt();
  if rms <= 0.0 {
    return Some(127);
  }
  Some((-20.0 * rms.log10()).round().clamp(0.0, 127.0) as u8)
}

/// The audio level of an encoded frame, where its PCM is at hand (see the
/// module documentation).
pub fn audio_level(mime_type: &str, frame: &[u8]) -> Option<u8> {
  match mime_type.to_ascii_lowercase().as_str() {
    "audio/pcmu" => level_of(frame.iter().map(|b| ulaw_to_linear(*b))),
    "audio/pcma" => level_of(frame.iter().map(|b| alaw_to_linear(*b))),
    "audio/opus" => {
      let (opus, _) = crate::fake_media::clip("audio/opus", false)?;
      let index = opus.iter().position(|f| *f == frame)?;
      let (pcmu, _) = crate::fake_media::clip("audio/pcmu", false)?;
      let twin = pcmu.get(index)?;
      level_of(twin.iter().map(|b| ulaw_to_linear(*b)))
    }
    _ => None,
  }
}

/// The RFC 6464 one-byte payload: the V bit clear (no voice activity
/// detection runs here) and the level.
pub fn audio_level_payload(level: u8) -> Bytes {
  Bytes::copy_from_slice(&[level & 0x7F])
}

/// The 16-byte abs-capture-time payload for a frame captured now: the NTP
/// timestamp (UQ32.32) and an estimated capture clock offset (Q32.32) of 0.
pub fn abs_capture_time_payload(now: SystemTime) -> Bytes {
  let since = now.duration_since(UNIX_EPOCH).unwrap_or_default();
  let seconds = since.as_secs().wrapping_add(NTP_UNIX_OFFSET_SECS);
  let fraction = ((since.subsec_nanos() as u64) << 32) / 1_000_000_000;
  let ntp = (seconds << 32) | (fraction & 0xFFFF_FFFF);
  let mut out = [0u8; 16];
  out[..8].copy_from_slice(&ntp.to_be_bytes());
  Bytes::copy_from_slice(&out)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn g711_decodes_to_the_expected_extremes() {
    assert_eq!(ulaw_to_linear(0xFF), 0);
    assert_eq!(ulaw_to_linear(0x00), -32124);
    assert_eq!(ulaw_to_linear(0x80), 32124);
    assert_eq!(alaw_to_linear(0xD5), 8);
    assert_eq!(alaw_to_linear(0x55), -8);
    assert_eq!(alaw_to_linear(0xAA), 32256);
  }

  #[test]
  fn the_fake_microphone_tone_is_about_21_dbov() {
    // A sine of amplitude 1/8 has an RMS of 0.0884: 21 dBov.
    let (pcmu, _) = crate::fake_media::clip("audio/pcmu", false).unwrap();
    let level = audio_level("audio/PCMU", pcmu[10]).unwrap();
    assert!((20..=22).contains(&level), "{level}");
    let (opus, _) = crate::fake_media::clip("audio/opus", false).unwrap();
    assert_eq!(audio_level("audio/opus", opus[10]), Some(level));
    assert_eq!(audio_level("audio/opus", &[0x78, 1, 2, 3]), None);
    assert_eq!(level_of([0i16; 160].into_iter()), Some(127));
  }

  #[test]
  fn abs_capture_time_is_ntp_with_a_zero_offset() {
    let t = UNIX_EPOCH + std::time::Duration::from_millis(1500);
    let payload = abs_capture_time_payload(t);
    assert_eq!(payload.len(), 16);
    let ntp = u64::from_be_bytes(payload[..8].try_into().unwrap());
    assert_eq!(ntp >> 32, NTP_UNIX_OFFSET_SECS + 1);
    assert_eq!(ntp & 0xFFFF_FFFF, 1 << 31);
    assert_eq!(&payload[8..], &[0u8; 8]);
  }
}
