// Copyright 2018-2026 the Deno authors. MIT license.

//! `crypto.argon2()` / `crypto.argon2Sync()` (Node 24.7+), backed by the
//! RustCrypto `argon2` crate. The JS layer (`internal/crypto/argon2.ts`)
//! validates the parameters the way Node's `lib/internal/crypto/argon2.js`
//! does; these ops are the native half of Node's `Argon2Job`, and refuse
//! whatever the KDF itself rejects with Node's `Argon2 derivation failed`.

use argon2::Algorithm;
use argon2::Argon2;
use argon2::AssociatedData;
use argon2::ParamsBuilder;
use argon2::Version;
use deno_core::convert::Uint8Array;
use deno_core::op2;
use deno_core::unsync::spawn_blocking;
use deno_error::JsErrorBox;

/// `kTypeArgon2d` / `kTypeArgon2i` / `kTypeArgon2id` in Node's
/// `internalBinding('crypto')` (ncrypto's `Argon2Type`).
const TYPE_ARGON2D: u32 = 0;
const TYPE_ARGON2I: u32 = 1;
const TYPE_ARGON2ID: u32 = 2;

fn derivation_failed() -> JsErrorBox {
  JsErrorBox::generic("Argon2 derivation failed")
}

/// One Argon2 derivation (RFC 9106, version 0x13).
///
/// Memory is bounded the way Node bounds it: `memcost` is at most
/// 2^32 - 1 KiB and `lanes` at most 2^24 - 1 (the JS validators enforce
/// both). Both the working memory and the output buffer are allocated
/// fallibly, so a request the machine cannot satisfy is an
/// `Argon2 derivation failed` error, as OpenSSL's failed allocation is in
/// Node, never an abort of the process.
#[allow(clippy::too_many_arguments, reason = "mirrors Node's Argon2Job")]
pub fn argon2_derive(
  pass: &[u8],
  salt: &[u8],
  lanes: u32,
  keylen: u32,
  memcost: u32,
  iter: u32,
  secret: &[u8],
  ad: &[u8],
  kind: u32,
) -> Result<Vec<u8>, JsErrorBox> {
  // Node's Argon2Traits::DeriveBits answers an empty key before it asks
  // the KDF anything.
  if keylen == 0 {
    return Ok(Vec::new());
  }
  let algorithm = match kind {
    TYPE_ARGON2D => Algorithm::Argon2d,
    TYPE_ARGON2I => Algorithm::Argon2i,
    TYPE_ARGON2ID => Algorithm::Argon2id,
    _ => return Err(derivation_failed()),
  };
  let keylen = usize::try_from(keylen).map_err(|_| derivation_failed())?;

  let mut builder = ParamsBuilder::new();
  builder
    .m_cost(memcost)
    .t_cost(iter)
    .p_cost(lanes)
    .output_len(keylen);
  if !ad.is_empty() {
    // The `argon2` crate keeps associated data inline, at most
    // `Params::MAX_DATA_LEN` (32) bytes; Node takes up to 2^32 - 1. Say so
    // rather than derive a different key.
    let data = AssociatedData::new(ad).map_err(|_| {
      JsErrorBox::generic(format!(
        "Argon2 derivation failed: associatedData longer than {} bytes is not supported",
        AssociatedData::MAX_LEN
      ))
    })?;
    builder.data(data);
  }
  let params = builder.build().map_err(|_| derivation_failed())?;
  let context = if secret.is_empty() {
    Argon2::new(algorithm, Version::V0x13, params)
  } else {
    Argon2::new_with_secret(secret, algorithm, Version::V0x13, params)
      .map_err(|_| derivation_failed())?
  };

  let mut out = Vec::new();
  out
    .try_reserve_exact(keylen)
    .map_err(|_| derivation_failed())?;
  out.resize(keylen, 0);
  // `hash_password_into` allocates its blocks with `alloc_zeroed` and
  // reports a failed allocation as an error.
  context
    .hash_password_into(pass, salt, &mut out)
    .map_err(|_| derivation_failed())?;
  Ok(out)
}

#[allow(clippy::too_many_arguments, reason = "mirrors Node's Argon2Job")]
#[op2]
pub fn op_node_argon2_sync(
  #[anybuffer] pass: &[u8],
  #[anybuffer] salt: &[u8],
  lanes: u32,
  keylen: u32,
  memcost: u32,
  iter: u32,
  #[anybuffer] secret: &[u8],
  #[anybuffer] ad: &[u8],
  kind: u32,
) -> Result<Uint8Array, JsErrorBox> {
  argon2_derive(pass, salt, lanes, keylen, memcost, iter, secret, ad, kind)
    .map(Into::into)
}

#[derive(Debug, thiserror::Error, deno_error::JsError)]
pub enum Argon2AsyncError {
  #[class(inherit)]
  #[error(transparent)]
  Join(#[from] tokio::task::JoinError),
  #[class(inherit)]
  #[error(transparent)]
  Other(JsErrorBox),
}

/// The async form copies its inputs (as Node's async job does) and runs on
/// the blocking pool, off the event loop.
#[allow(clippy::too_many_arguments, reason = "mirrors Node's Argon2Job")]
#[op2]
pub async fn op_node_argon2_async(
  #[buffer(copy)] pass: Vec<u8>,
  #[buffer(copy)] salt: Vec<u8>,
  lanes: u32,
  keylen: u32,
  memcost: u32,
  iter: u32,
  #[buffer(copy)] secret: Vec<u8>,
  #[buffer(copy)] ad: Vec<u8>,
  kind: u32,
) -> Result<Uint8Array, Argon2AsyncError> {
  spawn_blocking(move || {
    argon2_derive(
      &pass, &salt, lanes, keylen, memcost, iter, &secret, &ad, kind,
    )
    .map(Into::into)
    .map_err(Argon2AsyncError::Other)
  })
  .await?
}

#[cfg(test)]
mod tests {
  use super::*;

  fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
  }

  // RFC 9106 section 5 test vectors.
  #[test]
  fn rfc9106_vectors() {
    let pass = [0x01u8; 32];
    let salt = [0x02u8; 16];
    let secret = [0x03u8; 8];
    let ad = [0x04u8; 12];
    for (kind, expected) in [
      (
        TYPE_ARGON2D,
        "512b391b6f1162975371d30919734294f868e3be3984f3c1a13a4db9fabe4acb",
      ),
      (
        TYPE_ARGON2I,
        "c814d9d1dc7f37aa13f0d77f2494bda1c8de6b016dd388d29952a4c4672b6ce8",
      ),
      (
        TYPE_ARGON2ID,
        "0d640df58d78766c08c037a34a8b53c9d01ef0452d75b65eb52520e96b01e659",
      ),
    ] {
      let out =
        argon2_derive(&pass, &salt, 4, 32, 32, 3, &secret, &ad, kind).unwrap();
      assert_eq!(hex(&out), expected);
    }
  }

  #[test]
  fn kdf_rejections_are_errors() {
    let salt = [0x02u8; 16];
    for (lanes, memcost, iter) in [(0, 16, 1), (1, 0, 1), (1, 16, 0)] {
      let err = argon2_derive(
        b"password",
        &salt,
        lanes,
        32,
        memcost,
        iter,
        &[],
        &[],
        TYPE_ARGON2ID,
      )
      .unwrap_err();
      assert_eq!(err.to_string(), "Argon2 derivation failed");
    }
    // An unknown type and a short salt.
    assert!(argon2_derive(b"p", &salt, 1, 32, 8, 1, &[], &[], 3).is_err());
    assert!(
      argon2_derive(b"p", &[0; 7], 1, 32, 8, 1, &[], &[], TYPE_ARGON2ID)
        .is_err()
    );
  }

  #[test]
  fn empty_key_short_circuits() {
    assert!(
      argon2_derive(b"p", &[], 0, 0, 0, 0, &[], &[], 9)
        .unwrap()
        .is_empty()
    );
  }

  #[test]
  fn oversized_associated_data_is_refused() {
    let err =
      argon2_derive(b"p", &[0; 16], 1, 32, 8, 1, &[], &[0; 33], TYPE_ARGON2ID)
        .unwrap_err();
    assert!(err.to_string().contains("associatedData longer than 32"));
  }
}
