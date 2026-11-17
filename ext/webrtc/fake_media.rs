// Copyright 2018-2026 the Deno authors. MIT license.

//! The fake capture devices of `navigator.mediaDevices`, there only under
//! `DENO_FAKE_MEDIA_DEVICES=1` (Chrome's `--use-fake-device-for-media-stream`
//! for a runtime with no camera or microphone): a camera that shows a moving
//! test card and a microphone that plays a tone.
//!
//! The runtime has no encoder, so what they send is pre-encoded: one second
//! of each, in every codec the engine packetizes (`fake_media/generate.sh`
//! makes the clips), looped. A sender takes the clip of the codec it
//! negotiated, and restarts the video on its key frame when the remote asks
//! for one.

use std::sync::OnceLock;

/// Whether `DENO_FAKE_MEDIA_DEVICES=1` is set.
pub fn enabled() -> bool {
  std::env::var_os("DENO_FAKE_MEDIA_DEVICES").is_some_and(|v| v == "1")
}

/// A video frame lasts 1/30 s, an audio frame 20 ms.
pub const VIDEO_FRAME_MS: f64 = 1000.0 / 30.0;
pub const AUDIO_FRAME_MS: f64 = 20.0;

/// `<u32 little-endian length><frame>` records, as `pack.py` writes them.
fn frames(blob: &'static [u8]) -> Vec<&'static [u8]> {
  let mut out = Vec::new();
  let mut rest = blob;
  while rest.len() >= 4 {
    let len = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
    if rest.len() < 4 + len {
      break;
    }
    out.push(&rest[4..4 + len]);
    rest = &rest[4 + len..];
  }
  out
}

macro_rules! clip {
  ($file:literal) => {{
    static CLIP: OnceLock<Vec<&'static [u8]>> = OnceLock::new();
    CLIP
      .get_or_init(|| frames(include_bytes!(concat!("fake_media/", $file))))
      .as_slice()
  }};
}

/// The clip for a negotiated codec (`stereo` picks Opus' two-channel one),
/// and how long each of its frames lasts; `None` for a codec there is no
/// clip of.
pub fn clip(
  mime_type: &str,
  stereo: bool,
) -> Option<(&'static [&'static [u8]], f64)> {
  let mime = mime_type.to_ascii_lowercase();
  Some(match mime.as_str() {
    "audio/opus" if stereo => (clip!("opus-stereo.bin"), AUDIO_FRAME_MS),
    "audio/opus" => (clip!("opus.bin"), AUDIO_FRAME_MS),
    "audio/pcmu" => (clip!("pcmu.bin"), AUDIO_FRAME_MS),
    "audio/pcma" => (clip!("pcma.bin"), AUDIO_FRAME_MS),
    "audio/g722" => (clip!("g722.bin"), AUDIO_FRAME_MS),
    "video/vp8" => (clip!("vp8.bin"), VIDEO_FRAME_MS),
    "video/vp9" => (clip!("vp9.bin"), VIDEO_FRAME_MS),
    "video/av1" => (clip!("av1.bin"), VIDEO_FRAME_MS),
    "video/h264" => (clip!("h264.bin"), VIDEO_FRAME_MS),
    "video/h265" => (clip!("h265.bin"), VIDEO_FRAME_MS),
    _ => return None,
  })
}

/// Where a sender is in its clip.
#[derive(Debug, Default)]
pub struct Cursor {
  pub mime_type: String,
  pub stereo: bool,
  pub next: usize,
}

impl Cursor {
  /// The next frame for `mime_type` and its duration, starting the clip over
  /// when the codec changed.
  pub fn advance(
    &mut self,
    mime_type: &str,
    stereo: bool,
  ) -> Option<(&'static [u8], f64)> {
    let (frames, duration) = clip(mime_type, stereo)?;
    if !self.mime_type.eq_ignore_ascii_case(mime_type) || self.stereo != stereo
    {
      self.mime_type = mime_type.to_string();
      self.stereo = stereo;
      self.next = 0;
    }
    let frame = frames.get(self.next % frames.len())?;
    self.next = (self.next + 1) % frames.len();
    Some((frame, duration))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn every_clip_is_one_second() {
    for (mime, count) in [
      ("audio/opus", 50),
      ("audio/PCMU", 50),
      ("audio/pcma", 50),
      ("audio/g722", 50),
      ("video/VP8", 30),
      ("video/vp9", 30),
      ("video/av1", 30),
      ("video/h264", 30),
      ("video/h265", 30),
    ] {
      let (frames, ms) = clip(mime, false).unwrap();
      assert_eq!(frames.len(), count, "{mime}");
      assert!((frames.len() as f64 * ms - 1000.0).abs() < 0.01, "{mime}");
      assert!(frames.iter().all(|f| !f.is_empty()), "{mime}");
    }
    assert_eq!(clip("audio/opus", true).unwrap().0.len(), 50);
    assert!(clip("video/rtx", false).is_none());
  }

  #[test]
  fn video_clips_start_on_a_key_frame() {
    // VP8: bit 0 of the first byte clear marks a key frame, followed by the
    // start code 9d 01 2a.
    let vp8 = clip("video/vp8", false).unwrap().0;
    assert_eq!(vp8[0][0] & 1, 0);
    assert_eq!(&vp8[0][3..6], &[0x9d, 0x01, 0x2a]);
    assert!(vp8[1..].iter().all(|f| f[0] & 1 == 1));
    // H.264: the first access unit carries an SPS (7), a PPS (8) and an IDR
    // slice (5).
    let h264 = clip("video/h264", false).unwrap().0;
    let types: Vec<u8> = h264[0]
      .windows(4)
      .filter(|w| w[..3] == [0, 0, 1])
      .map(|w| w[3] & 0x1f)
      .collect();
    for t in [7, 8, 5] {
      assert!(types.contains(&t), "{types:?}");
    }
  }

  #[test]
  fn opus_clips_are_mono_and_stereo() {
    // The TOC byte's `s` bit (0x04) marks a stereo frame.
    assert!(
      clip("audio/opus", false)
        .unwrap()
        .0
        .iter()
        .all(|f| f[0] & 4 == 0)
    );
    assert!(
      clip("audio/opus", true)
        .unwrap()
        .0
        .iter()
        .all(|f| f[0] & 4 != 0)
    );
  }

  #[test]
  fn the_cursor_loops_and_restarts_on_a_codec_change() {
    let mut cursor = Cursor::default();
    let first = clip("video/vp8", false).unwrap().0[0];
    assert_eq!(cursor.advance("video/vp8", false).unwrap().0, first);
    for _ in 1..30 {
      cursor.advance("video/vp8", false).unwrap();
    }
    assert_eq!(cursor.advance("video/vp8", false).unwrap().0, first);
    let h264 = clip("video/h264", false).unwrap().0[0];
    assert_eq!(cursor.advance("video/H264", false).unwrap().0, h264);
    assert!(cursor.advance("video/rtx", false).is_none());
  }
}
