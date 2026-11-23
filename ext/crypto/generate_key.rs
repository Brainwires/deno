// Copyright 2018-2026 the Deno authors. MIT license.

use aws_lc_rs::rand::SecureRandom;
use aws_lc_rs::signature::EcdsaKeyPair;
use elliptic_curve::Generate as _;
use elliptic_curve::pkcs8::EncodePrivateKey;
use once_cell::sync::Lazy;
use rsa::BoxedUint;
use rsa::PrivateKey as RsaPrivateKey;
use rsa::pkcs1::EncodeRsaPrivateKey;

use crate::shared::*;

#[derive(Debug, thiserror::Error, deno_error::JsError)]
#[class("DOMExceptionOperationError")]
pub enum GenerateKeyError {
  #[class(inherit)]
  #[error(transparent)]
  General(
    #[from]
    #[inherit]
    SharedError,
  ),
  #[error("Bad public exponent")]
  BadPublicExponent,
  #[error("Invalid HMAC key length")]
  InvalidHMACKeyLength,
  #[error("Failed to serialize RSA key")]
  FailedRSAKeySerialization,
  #[error("Invalid AES key length")]
  InvalidAESKeyLength,
  #[error("Failed to generate RSA key")]
  FailedRSAKeyGeneration,
  #[error("Failed to generate EC key")]
  FailedECKeyGeneration,
  #[error("Failed to generate key")]
  FailedKeyGeneration,
  #[error("Unsupported algorithm")]
  UnsupportedAlgorithm,
}

// Allowlist for RSA public exponents.
static PUB_EXPONENT_1: Lazy<BoxedUint> = Lazy::new(|| BoxedUint::from(3u64));
static PUB_EXPONENT_2: Lazy<BoxedUint> =
  Lazy::new(|| BoxedUint::from(65537u64));

/// Rust-callable view of the RSA keygen path. Returns raw PKCS#1 DER
/// bytes ready to feed [`crate::shared::RawKeyData::Private`].
pub fn generate_rsa(
  modulus_length: u32,
  public_exponent: &[u8],
) -> Result<Vec<u8>, GenerateKeyError> {
  generate_key_rsa(modulus_length, public_exponent)
}

/// Rust-callable EC keygen.
pub fn generate_ec(
  named_curve: EcNamedCurve,
) -> Result<Vec<u8>, GenerateKeyError> {
  generate_key_ec(named_curve)
}

/// Rust-callable AES keygen.
pub fn generate_aes(length: usize) -> Result<Vec<u8>, GenerateKeyError> {
  generate_key_aes(length)
}

/// Rust-callable HMAC keygen.
pub fn generate_hmac(
  hash: ShaHash,
  length: Option<usize>,
) -> Result<Vec<u8>, GenerateKeyError> {
  generate_key_hmac(hash, length)
}

fn generate_key_rsa(
  modulus_length: u32,
  public_exponent: &[u8],
) -> Result<Vec<u8>, GenerateKeyError> {
  let exponent = BoxedUint::from_be_slice_vartime(public_exponent);
  if exponent != *PUB_EXPONENT_1 && exponent != *PUB_EXPONENT_2 {
    return Err(GenerateKeyError::BadPublicExponent);
  }

  let private_key = match generate_rsa_aws_lc(modulus_length, &exponent) {
    Some(key) => key,
    None => {
      // The unchecked constructor: rsa 0.10 refuses to generate a modulus
      // under 1024 bits, which WebCrypto allowed on rsa 0.9.
      RsaPrivateKey::new_with_exp_unchecked(
        &mut rsa_getrandom::SysRng,
        modulus_length as usize,
        exponent,
      )
      .map_err(|_| GenerateKeyError::FailedRSAKeyGeneration)?
    }
  };

  let private_key = private_key
    .to_pkcs1_der()
    .map_err(|_| GenerateKeyError::FailedRSAKeySerialization)?;

  Ok(private_key.as_bytes().to_vec())
}

/// Generates the key with aws-lc (OpenSSL-speed prime search) when it
/// supports the request: 2048, 3072, 4096 or 8192 bits with e = 65537.
/// `None` sends every other size and exponent to the pure-Rust `rsa` path.
/// The key comes back as PKCS#8 and is parsed into the `rsa` crate's type,
/// so export, sign, encrypt and JWK use it as before.
fn generate_rsa_aws_lc(
  modulus_length: u32,
  exponent: &BoxedUint,
) -> Option<RsaPrivateKey> {
  use aws_lc_rs::encoding::AsDer;
  use aws_lc_rs::rsa::KeySize;
  use rsa::pkcs8::DecodePrivateKey;
  use rsa::traits::PublicKeyParts;

  if *exponent != *PUB_EXPONENT_2 {
    return None;
  }
  let size = match modulus_length {
    2048 => KeySize::Rsa2048,
    3072 => KeySize::Rsa3072,
    4096 => KeySize::Rsa4096,
    8192 => KeySize::Rsa8192,
    _ => return None,
  };
  let pair = aws_lc_rs::rsa::KeyPair::generate(size).ok()?;
  let der = pair.as_der().ok()?;
  let key = RsaPrivateKey::from_pkcs8_der(der.as_ref()).ok()?;
  (key.e() == exponent && key.n().bit_width() == modulus_length).then_some(key)
}

fn generate_key_ec_p521() -> Result<Vec<u8>, GenerateKeyError> {
  // The OS RNG, as before (elliptic-curve 0.14 takes rand_core 0.10).
  let key = p521::SecretKey::generate();
  let pkcs8 = key
    .to_pkcs8_der()
    .map_err(|_| GenerateKeyError::FailedECKeyGeneration)?;
  Ok(pkcs8.as_bytes().to_vec())
}

fn generate_key_ec(
  named_curve: EcNamedCurve,
) -> Result<Vec<u8>, GenerateKeyError> {
  let curve = match named_curve {
    EcNamedCurve::P256 => {
      &aws_lc_rs::signature::ECDSA_P256_SHA256_FIXED_SIGNING
    }
    EcNamedCurve::P384 => {
      &aws_lc_rs::signature::ECDSA_P384_SHA384_FIXED_SIGNING
    }
    EcNamedCurve::P521 => return generate_key_ec_p521(),
  };

  let rng = aws_lc_rs::rand::SystemRandom::new();

  let pkcs8 = EcdsaKeyPair::generate_pkcs8(curve, &rng)
    .map_err(|_| GenerateKeyError::FailedECKeyGeneration)?;

  Ok(pkcs8.as_ref().to_vec())
}

fn generate_key_aes(length: usize) -> Result<Vec<u8>, GenerateKeyError> {
  if !length.is_multiple_of(8) || length > 256 {
    return Err(GenerateKeyError::InvalidAESKeyLength);
  }

  let mut key = vec![0u8; length / 8];
  let rng = aws_lc_rs::rand::SystemRandom::new();
  rng
    .fill(&mut key)
    .map_err(|_| GenerateKeyError::FailedKeyGeneration)?;

  Ok(key)
}

fn generate_key_hmac(
  hash: ShaHash,
  length: Option<usize>,
) -> Result<Vec<u8>, GenerateKeyError> {
  // Default key length (in bytes) is the hash's block size.
  // SHA-3 is not supported by aws-lc-rs for HMAC, so the block sizes are
  // hard-coded here per FIPS 202.
  let default_block_len = match hash {
    ShaHash::Sha1 => aws_lc_rs::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY
      .digest_algorithm()
      .block_len(),
    ShaHash::Sha256 => {
      aws_lc_rs::hmac::HMAC_SHA256.digest_algorithm().block_len()
    }
    ShaHash::Sha384 => {
      aws_lc_rs::hmac::HMAC_SHA384.digest_algorithm().block_len()
    }
    ShaHash::Sha512 => {
      aws_lc_rs::hmac::HMAC_SHA512.digest_algorithm().block_len()
    }
    // FIPS 202: rate (r) in bytes for SHA3-N is (1600 - 2N) / 8.
    ShaHash::Sha3_256 => 136,
    ShaHash::Sha3_384 => 104,
    ShaHash::Sha3_512 => 72,
  };

  let length = if let Some(length) = length {
    if length % 8 != 0 {
      return Err(GenerateKeyError::InvalidHMACKeyLength);
    }

    let length = length / 8;
    if length > aws_lc_rs::digest::MAX_BLOCK_LEN {
      return Err(GenerateKeyError::InvalidHMACKeyLength);
    }

    length
  } else {
    default_block_len
  };

  let rng = aws_lc_rs::rand::SystemRandom::new();
  let mut key = vec![0u8; length];
  rng
    .fill(&mut key)
    .map_err(|_| GenerateKeyError::FailedKeyGeneration)?;

  Ok(key)
}
