// Copyright 2018-2026 the Deno authors. MIT license.

//! Cryptographically secure random bytes for `crypto.getRandomValues`,
//! `crypto.randomUUID`, `node:crypto`'s random functions and WASI's
//! `random_get`.
//!
//! The bytes come from aws-lc's `RAND_bytes`: a per-thread AES-256 CTR-DRBG
//! (SP 800-90A) seeded from the operating system, reseeded on a fixed request
//! budget, on every call where fork detection is unavailable, and after a
//! fork. It runs at full speed in debug builds, where `rand`'s generic ChaCha
//! code is compiled without optimizations.

use std::fmt;

/// The secure random source could not produce bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecureRandomError;

impl fmt::Display for SecureRandomError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("the secure random number generator failed")
  }
}

impl std::error::Error for SecureRandomError {}

/// Fills all of `buf` with cryptographically secure random bytes in one call.
///
/// On failure the buffer is zeroed and an error is returned, so a caller never
/// hands back a partly filled buffer as random.
pub fn fill_secure_random(buf: &mut [u8]) -> Result<(), SecureRandomError> {
  if aws_lc_rs::rand::fill(buf).is_ok() {
    return Ok(());
  }
  buf.fill(0);
  Err(SecureRandomError)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn empty_buffer_is_ok() {
    let mut buf = [0u8; 0];
    assert_eq!(fill_secure_random(&mut buf), Ok(()));
  }

  #[test]
  fn fills_every_byte_of_a_large_buffer() {
    // Larger than both WebCrypto's 65,536-byte limit and the DRBG's
    // per-generate maximum, and not a multiple of either.
    let len = 3 * 65536 + 17;
    let mut buf = vec![0u8; len];
    fill_secure_random(&mut buf).unwrap();
    // A random 4 KiB window being all zero has probability 2^-32768.
    for chunk in buf.chunks(4096) {
      assert!(chunk.iter().any(|b| *b != 0), "an unfilled window");
    }
    // Every byte value shows up in 196 KiB of random data.
    let mut seen = [false; 256];
    for b in &buf {
      seen[*b as usize] = true;
    }
    assert!(seen.iter().all(|s| *s));
  }

  #[test]
  fn successive_fills_differ() {
    let mut a = [0u8; 32];
    let mut b = [0u8; 32];
    fill_secure_random(&mut a).unwrap();
    fill_secure_random(&mut b).unwrap();
    assert_ne!(a, b);
  }

  #[test]
  fn threads_get_distinct_bytes() {
    let handles: Vec<_> = (0..4)
      .map(|_| {
        std::thread::spawn(|| {
          let mut buf = [0u8; 32];
          fill_secure_random(&mut buf).unwrap();
          buf
        })
      })
      .collect();
    let mut outputs: Vec<[u8; 32]> =
      handles.into_iter().map(|h| h.join().unwrap()).collect();
    outputs.sort();
    outputs.dedup();
    assert_eq!(outputs.len(), 4);
  }

  #[test]
  fn error_displays() {
    assert_eq!(
      SecureRandomError.to_string(),
      "the secure random number generator failed"
    );
  }
}
