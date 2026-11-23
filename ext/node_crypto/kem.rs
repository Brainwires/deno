// Copyright 2018-2026 the Deno authors. MIT license.

//! `crypto.encapsulate` / `crypto.decapsulate` and ML-DSA one-shot
//! `sign` / `verify`.
//!
//! The KEMs are the ones Node's BoringSSL build (whose identity the runtime
//! reports) serves with an OpenSSL 3.0 version number: RSASVE (NIST SP
//! 800-56B) on `rsa` keys and ML-KEM-768 / ML-KEM-1024. DHKEM on EC / X25519
//! / X448 keys needs OpenSSL 3.2 in Node and fails here as it does there.

use deno_core::ToV8;
use deno_core::convert::Uint8Array;
use deno_core::op2;
use rand::RngCore as _;
use rsa::BoxedUint;
use rsa::hazmat::rsa_decrypt_and_check;
use rsa::hazmat::rsa_encrypt;
use rsa::traits::PublicKeyParts as _;

use crate::keys::AsymmetricPrivateKey;
use crate::keys::AsymmetricPublicKey;
use crate::keys::KeyObjectHandle;
use crate::pqc::PqcError;
use crate::rsa_util::uint_to_be_bytes_padded;

#[derive(ToV8)]
pub struct Encapsulated {
  shared_key: Uint8Array,
  ciphertext: Uint8Array,
}

/// RSASVE-GENERATE: a random `1 < z < n - 1`, `c = z^e mod n`.
fn rsasve_encapsulate(key: &rsa::PublicKey) -> Option<(Vec<u8>, Vec<u8>)> {
  let precision = key.n_bits_precision();
  let n_len = key.size();
  let one = BoxedUint::one_with_precision(precision);
  let n_minus_1 = key.n().as_ref().wrapping_sub(&one);
  let bits = key.n().bit_width() as usize;
  let mut rng = rand::thread_rng();
  let mut buf = vec![0u8; n_len];
  for _ in 0..128 {
    rng.fill_bytes(&mut buf);
    let excess = n_len * 8 - bits;
    if excess > 0 {
      buf[0] &= 0xff >> excess;
    }
    let z = BoxedUint::from_be_slice(&buf, precision).ok()?;
    if z <= one || z >= n_minus_1 {
      continue;
    }
    let c = rsa_encrypt(key, &z).ok()?;
    return Some((
      uint_to_be_bytes_padded(&z, n_len),
      uint_to_be_bytes_padded(&c, n_len),
    ));
  }
  None
}

/// RSASVE-RECOVER: `z = c^d mod n`, checked to be in `(1, n - 1)`.
fn rsasve_decapsulate(
  key: &rsa::PrivateKey,
  ciphertext: &[u8],
) -> Option<Vec<u8>> {
  let n_len = key.size();
  if ciphertext.len() != n_len {
    return None;
  }
  let precision = key.n_bits_precision();
  let c = BoxedUint::from_be_slice(ciphertext, precision).ok()?;
  if c >= key.n().as_ref() {
    return None;
  }
  let z =
    rsa_decrypt_and_check(key, Some(&mut rsa_getrandom::SysRng), &c).ok()?;
  let one = BoxedUint::one_with_precision(precision);
  let n_minus_1 = key.n().as_ref().wrapping_sub(&one);
  if z <= one || z >= n_minus_1 {
    return None;
  }
  Some(uint_to_be_bytes_padded(&z, n_len))
}

pub fn encapsulate(
  handle: &KeyObjectHandle,
) -> Result<(Vec<u8>, Vec<u8>), PqcError> {
  let public = handle
    .as_public_key()
    .ok_or(PqcError::NotSupportedForKeyType)?;
  match &*public {
    AsymmetricPublicKey::Pqc(key) => key.encapsulate(),
    AsymmetricPublicKey::Rsa(key) => {
      rsasve_encapsulate(key).ok_or(PqcError::EncapsulationFailed)
    }
    _ => Err(PqcError::NotSupportedForKeyType),
  }
}

pub fn decapsulate(
  handle: &KeyObjectHandle,
  ciphertext: &[u8],
) -> Result<Vec<u8>, PqcError> {
  match handle.as_private_key() {
    Some(AsymmetricPrivateKey::Pqc(key)) => key.decapsulate(ciphertext),
    Some(AsymmetricPrivateKey::Rsa(key)) => {
      rsasve_decapsulate(key, ciphertext).ok_or(PqcError::DecapsulationFailed)
    }
    _ => Err(PqcError::DecapsulationFailed),
  }
}

#[op2]
pub fn op_node_encapsulate(
  #[cppgc] handle: &KeyObjectHandle,
) -> Result<Encapsulated, PqcError> {
  let (shared_key, ciphertext) = encapsulate(handle)?;
  Ok(Encapsulated {
    shared_key: shared_key.into(),
    ciphertext: ciphertext.into(),
  })
}

#[op2]
pub fn op_node_decapsulate(
  #[cppgc] handle: &KeyObjectHandle,
  #[buffer] ciphertext: &[u8],
) -> Result<Uint8Array, PqcError> {
  Ok(decapsulate(handle, ciphertext)?.into())
}

/// One-shot ML-DSA signing (`crypto.sign(null, data, key)`).
#[op2]
pub fn op_node_sign_pqc(
  #[cppgc] handle: &KeyObjectHandle,
  #[buffer] data: &[u8],
) -> Result<Uint8Array, PqcError> {
  match handle.as_private_key() {
    Some(AsymmetricPrivateKey::Pqc(key)) => Ok(key.sign(data)?.into()),
    _ => Err(PqcError::NotSupportedForKeyType),
  }
}

/// One-shot ML-DSA verification.
#[op2(fast)]
pub fn op_node_verify_pqc(
  #[cppgc] handle: &KeyObjectHandle,
  #[buffer] data: &[u8],
  #[buffer] signature: &[u8],
) -> Result<bool, PqcError> {
  let public = handle
    .as_public_key()
    .ok_or(PqcError::NotSupportedForKeyType)?;
  match &*public {
    AsymmetricPublicKey::Pqc(key) => key.verify(data, signature),
    _ => Err(PqcError::NotSupportedForKeyType),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn rsasve_round_trip() {
    let mut rng = rsa_getrandom::SysRng;
    let private = rsa::PrivateKey::new(&mut rng, 1024).unwrap();
    let public = private.to_public_key();
    let (z, c) = rsasve_encapsulate(&public).unwrap();
    assert_eq!(z.len(), 128);
    assert_eq!(c.len(), 128);
    assert_eq!(rsasve_decapsulate(&private, &c).unwrap(), z);
    let other = rsa::PrivateKey::new(&mut rng, 1024).unwrap();
    assert_ne!(rsasve_decapsulate(&other, &c), Some(z));
  }
}
