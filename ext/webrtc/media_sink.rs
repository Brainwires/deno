// Copyright 2018-2026 the Deno authors. MIT license.

//! What a remote track really receives, for the track's own state:
//!
//! - silence: a receiver whose RTP stops for [`SILENCE`] mutes its track
//!   (mediacapture-main 4.3.1.1: "muted ... the source is temporarily unable
//!   to provide the track with data"), and its next packet unmutes it again.

use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;

use crate::peer::Core;
use crate::peer::JsEvent;

/// How long a receiver may go without RTP before its track mutes. The fake
/// devices and every real encoder send at least every 400 ms (Opus DTX's
/// comfort noise is the sparsest), so a gap this long means the remote side
/// stopped sending: a stopped transceiver, a closed connection, an inactive
/// encoding or a direction without `send`.
pub const SILENCE: Duration = Duration::from_millis(2000);

/// The receivers' sinks, by transceiver key.
#[derive(Default)]
pub struct MediaSinks {
  /// When each flowing receiver's last RTP packet arrived.
  last_packet: HashMap<u32, Instant>,
}

impl MediaSinks {
  /// The next time a flowing receiver goes silent.
  pub fn silence_deadline(&self) -> Option<Instant> {
    self.last_packet.values().min().map(|t| *t + SILENCE)
  }

  /// A receiver's RTP packet arrived.
  pub fn saw_packet(&mut self, key: u32, now: Instant) {
    self.last_packet.insert(key, now);
  }
}

impl Core {
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
}
