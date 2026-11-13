// Copyright 2018-2026 the Deno authors. MIT license.

//! MIDI 1.0 byte-stream framing.
//!
//! The OS APIs hand over bytes in whatever chunks their drivers produce: a
//! packet with several messages, a system exclusive message split across
//! packets, running status, a real-time byte in the middle of another
//! message. Web MIDI delivers each `midimessage` event as exactly one complete
//! message with its status byte, so every byte an input receives goes through
//! a [`MessageParser`], which:
//!
//! - expands running status (a data byte after a complete channel message
//!   repeats that message's status);
//! - delivers a real-time byte (`0xF8`-`0xFF`) at once as its own message,
//!   wherever it arrives, without disturbing the message around it;
//! - collects a system exclusive message from `0xF0` to `0xF7`, and drops one
//!   that another status byte cuts short;
//! - drops the undefined status bytes (`0xF4`, `0xF5`, `0xF9`, `0xFD`), a
//!   stray `0xF7` and data bytes with no status to belong to.
//!
//! An output uses the same parser to split the bytes of one `send()` (already
//! validated by the JavaScript side) into single messages, since WinMM takes
//! one short message or one system exclusive message per call.

/// The longest system exclusive message an input collects (16 MiB); a longer
/// one is dropped rather than buffered without bound.
pub const MAX_SYSEX_LEN: usize = 16 * 1024 * 1024;

/// The length of a message with this status byte, or `None` for a status
/// byte that starts no fixed-length message (`0xF0`, `0xF7`, the undefined
/// ones).
pub fn message_len(status: u8) -> Option<usize> {
  match status {
    0x80..=0xBF => Some(3),
    0xC0..=0xDF => Some(2),
    0xE0..=0xEF => Some(3),
    0xF1 | 0xF3 => Some(2),
    0xF2 => Some(3),
    0xF6 => Some(1),
    0xF8 | 0xFA..=0xFC | 0xFE | 0xFF => Some(1),
    _ => None,
  }
}

#[derive(Default)]
pub struct MessageParser {
  buf: Vec<u8>,
  /// The status byte running status repeats (a channel message's).
  running: Option<u8>,
  /// The length of the message in `buf`.
  expected: usize,
  in_sysex: bool,
  /// The system exclusive message in `buf` grew past [`MAX_SYSEX_LEN`]; its
  /// remaining bytes are skipped.
  sysex_overflow: bool,
}

impl MessageParser {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn feed(&mut self, bytes: &[u8], mut emit: impl FnMut(&[u8])) {
    for &byte in bytes {
      self.push(byte, &mut emit);
    }
  }

  fn push(&mut self, byte: u8, emit: &mut impl FnMut(&[u8])) {
    if byte >= 0xF8 {
      // real-time: its own message, wherever it arrives
      if message_len(byte).is_some() {
        emit(&[byte]);
      }
      return;
    }
    if byte & 0x80 != 0 {
      self.status(byte, emit);
      return;
    }
    if self.in_sysex {
      if self.sysex_overflow {
        return;
      }
      if self.buf.len() >= MAX_SYSEX_LEN {
        self.sysex_overflow = true;
        self.buf = Vec::new();
        return;
      }
      self.buf.push(byte);
      return;
    }
    if self.buf.is_empty() {
      match self.running {
        Some(status) => {
          self.buf.push(status);
          self.expected = message_len(status).unwrap_or(0);
        }
        // a data byte that belongs to no message
        None => return,
      }
    }
    self.buf.push(byte);
    if self.buf.len() == self.expected {
      emit(&self.buf);
      self.buf.clear();
    }
  }

  fn status(&mut self, byte: u8, emit: &mut impl FnMut(&[u8])) {
    if byte == 0xF7 {
      if self.in_sysex && !self.sysex_overflow {
        self.buf.push(0xF7);
        emit(&self.buf);
      }
      self.reset();
      return;
    }
    // any other status byte ends an unfinished message (a system exclusive
    // message cut short is dropped)
    self.reset();
    if byte == 0xF0 {
      self.in_sysex = true;
      self.buf.push(0xF0);
      return;
    }
    let Some(len) = message_len(byte) else {
      // 0xF4 / 0xF5: undefined
      return;
    };
    // channel messages set running status; system common messages clear it
    self.running = (byte < 0xF0).then_some(byte);
    if len == 1 {
      emit(&[byte]);
      return;
    }
    self.buf.push(byte);
    self.expected = len;
  }

  fn reset(&mut self) {
    self.buf.clear();
    self.in_sysex = false;
    self.sysex_overflow = false;
    self.expected = 0;
    // a status byte other than real-time always replaces running status
    // (`status` sets it again for a channel message)
    self.running = None;
  }
}

/// Splits bytes the JavaScript side validated into single messages.
pub fn split_messages(bytes: &[u8]) -> Vec<Vec<u8>> {
  let mut out = Vec::new();
  let mut parser = MessageParser::new();
  parser.feed(bytes, |message| out.push(message.to_vec()));
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  fn parse(chunks: &[&[u8]]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut parser = MessageParser::new();
    for chunk in chunks {
      parser.feed(chunk, |m| out.push(m.to_vec()));
    }
    out
  }

  #[test]
  fn complete_messages() {
    assert_eq!(
      parse(&[&[0x90, 60, 100, 0x80, 60, 0, 0xC0, 5]]),
      vec![vec![0x90, 60, 100], vec![0x80, 60, 0], vec![0xC0, 5]]
    );
  }

  #[test]
  fn running_status_is_expanded() {
    assert_eq!(
      parse(&[&[0x90, 60, 100, 62, 100], &[64, 0]]),
      vec![vec![0x90, 60, 100], vec![0x90, 62, 100], vec![0x90, 64, 0]]
    );
  }

  #[test]
  fn split_across_chunks() {
    assert_eq!(parse(&[&[0xB0], &[7], &[127]]), vec![vec![0xB0, 7, 127]]);
  }

  #[test]
  fn sysex_across_chunks() {
    assert_eq!(
      parse(&[&[0xF0, 0x7E, 0x7F], &[0x06, 0x01], &[0xF7]]),
      vec![vec![0xF0, 0x7E, 0x7F, 0x06, 0x01, 0xF7]]
    );
  }

  #[test]
  fn realtime_inside_messages() {
    assert_eq!(
      parse(&[&[0x90, 60, 0xF8, 100, 0xF0, 1, 0xFE, 2, 0xF7]]),
      vec![
        vec![0xF8],
        vec![0x90, 60, 100],
        vec![0xFE],
        vec![0xF0, 1, 2, 0xF7]
      ]
    );
  }

  #[test]
  fn truncated_sysex_is_dropped() {
    assert_eq!(
      parse(&[&[0xF0, 1, 2, 0x90, 60, 100]]),
      vec![vec![0x90, 60, 100]]
    );
  }

  #[test]
  fn system_common_clears_running_status() {
    assert_eq!(
      parse(&[&[0x90, 60, 100, 0xF1, 0x20, 62, 100]]),
      vec![vec![0x90, 60, 100], vec![0xF1, 0x20]]
    );
    // a real-time byte does not
    assert_eq!(
      parse(&[&[0x90, 60, 100, 0xF8, 62, 100]]),
      vec![vec![0x90, 60, 100], vec![0xF8], vec![0x90, 62, 100]]
    );
  }

  #[test]
  fn undefined_and_stray_bytes_are_dropped() {
    assert_eq!(
      parse(&[&[0x01, 0xF4, 0xF5, 0xF9, 0xFD, 0xF7, 0x40, 0xF6]]),
      vec![vec![0xF6]]
    );
  }

  #[test]
  fn oversized_sysex_is_dropped() {
    let mut big = vec![0xF0];
    big.resize(MAX_SYSEX_LEN + 10, 0x01);
    big.push(0xF7);
    assert_eq!(parse(&[&big, &[0xC0, 1]]), vec![vec![0xC0, 1]]);
  }

  #[test]
  fn split_messages_splits() {
    assert_eq!(
      split_messages(&[0x90, 60, 100, 0xF0, 1, 0xF7, 0xFF]),
      vec![vec![0x90, 60, 100], vec![0xF0, 1, 0xF7], vec![0xFF]]
    );
  }
}
