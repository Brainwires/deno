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
use num_bigint_dig::BigUint;
use rand::RngCore as _;
use rsa::hazmat::rsa_decrypt_and_check;
use rsa::hazmat::rsa_encrypt;
use rsa::traits::PublicKeyParts as _;

use crate::keys::AsymmetricPrivateKey;
use crate::keys::AsymmetricPublicKey;
use crate::keys::KeyObjectHandle;
use crate::pqc::PqcError;

#[derive(ToV8)]
pub struct Encapsulated {
  shared_key: Uint8Array,
  ciphertext: Uint8Array,
}

fn left_pad(mut bytes: Vec<u8>, len: usize) -> Vec<u8> {
  if bytes.len() < len {
    let mut padded = vec![0u8; len - bytes.len()];
    padded.append(&mut bytes);
    bytes = padded;
  }
  bytes
}

/// RSASVE-GENERATE: a random `1 < z < n - 1`, `c = z^e mod n`.
fn rsasve_encapsulate(key: &rsa::RsaPublicKey) -> Option<(Vec<u8>, Vec<u8>)> {
  let n = key.n();
  let n_len = key.size();
  let n_minus_1 = n - BigUint::from(1u8);
  let bits = n.bits();
  let mut rng = rand::thread_rng();
  let mut buf = vec![0u8; n_len];
  for _ in 0..128 {
    rng.fill_bytes(&mut buf);
    let excess = n_len * 8 - bits;
    if excess > 0 {
      buf[0] &= 0xff >> excess;
    }
    let z = BigUint::from_bytes_be(&buf);
    if z <= BigUint::from(1u8) || z >= n_minus_1 {
      continue;
    }
    let c = rsa_encrypt(key, &z).ok()?;
    return Some((
      left_pad(z.to_bytes_be(), n_len),
      left_pad(c.to_bytes_be(), n_len),
    ));
  }
  None
}

/// RSASVE-RECOVER: `z = c^d mod n`, checked to be in `(1, n - 1)`.
fn rsasve_decapsulate(
  key: &rsa::RsaPrivateKey,
  ciphertext: &[u8],
) -> Option<Vec<u8>> {
  let n_len = key.size();
  if ciphertext.len() != n_len {
    return None;
  }
  let c = BigUint::from_bytes_be(ciphertext);
  if &c >= key.n() {
    return None;
  }
  let mut rng = rand::thread_rng();
  let z = rsa_decrypt_and_check(key, Some(&mut rng), &c).ok()?;
  let n_minus_1 = key.n() - BigUint::from(1u8);
  if z <= BigUint::from(1u8) || z >= n_minus_1 {
    return None;
  }
  Some(left_pad(z.to_bytes_be(), n_len))
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
    let mut rng = rand::thread_rng();
    let private = rsa::RsaPrivateKey::new(&mut rng, 1024).unwrap();
    let public = private.to_public_key();
    let (z, c) = rsasve_encapsulate(&public).unwrap();
    assert_eq!(z.len(), 128);
    assert_eq!(c.len(), 128);
    assert_eq!(rsasve_decapsulate(&private, &c).unwrap(), z);
    let other = rsa::RsaPrivateKey::new(&mut rng, 1024).unwrap();
    assert_ne!(rsasve_decapsulate(&other, &c), Some(z));
  }
}
