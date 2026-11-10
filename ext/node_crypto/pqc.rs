// Copyright 2018-2026 the Deno authors. MIT license.

//! ML-DSA (FIPS 204) and ML-KEM (FIPS 203) keys for `node:crypto`'s
//! `KeyObject` layer: PKCS#8 / SPKI / JWK / raw import and export,
//! `generateKeyPair`, `sign` / `verify` and `encapsulate` / `decapsulate`.
//!
//! The runtime reports `process.features.openssl_is_boringssl`, so this layer
//! follows Node's BoringSSL build: private keys are held as their seed (the
//! only PKCS#8 encoding BoringSSL reads and writes), and ML-KEM-512, which
//! BoringSSL lacks, is refused with `ERR_OSSL_EVP_UNSUPPORTED_ALGORITHM`.
//!
//! ML-DSA comes from aws-lc-rs; ML-KEM key expansion from a seed comes from
//! the pure-Rust `fips203` crate (aws-lc-rs has no seed-based ML-KEM key
//! generation) and encapsulation / decapsulation from aws-lc-rs, the same
//! split `ext/crypto/mlkem.rs` uses for WebCrypto.

use aws_lc_rs::kem;
use aws_lc_rs::signature::KeyPair as _;
use aws_lc_rs::signature::PqdsaKeyPair;
use aws_lc_rs::signature::UnparsedPublicKey;
use base64::Engine as _;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use fips203::traits::KeyGen as _;
use fips203::traits::SerDes as _;
use rand::RngCore as _;
use spki::der::Encode as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PqcAlg {
  MlDsa44,
  MlDsa65,
  MlDsa87,
  MlKem768,
  MlKem1024,
}

const ML_DSA_44_OID: const_oid::ObjectIdentifier =
  const_oid::ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.3.17");
const ML_DSA_65_OID: const_oid::ObjectIdentifier =
  const_oid::ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.3.18");
const ML_DSA_87_OID: const_oid::ObjectIdentifier =
  const_oid::ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.3.19");
const ML_KEM_512_OID: const_oid::ObjectIdentifier =
  const_oid::ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.4.1");
const ML_KEM_768_OID: const_oid::ObjectIdentifier =
  const_oid::ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.4.2");
const ML_KEM_1024_OID: const_oid::ObjectIdentifier =
  const_oid::ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.4.3");

#[derive(Debug, thiserror::Error, deno_error::JsError)]
pub enum PqcError {
  /// ML-KEM-512: Node's BoringSSL build has no ML-KEM-512.
  #[class(generic)]
  #[error("unsupported algorithm")]
  #[property("code" = "ERR_OSSL_EVP_UNSUPPORTED_ALGORITHM")]
  UnsupportedAlgorithm,
  /// A PKCS#8 private key without its seed (expanded key only).
  #[class(generic)]
  #[error("unsupported")]
  #[property("code" = "ERR_OSSL_UNSUPPORTED")]
  UnsupportedPrivateKeyFormat,
  #[class(generic)]
  #[error("decode error")]
  #[property("code" = "ERR_OSSL_EVP_DECODE_ERROR")]
  DecodeError,
  #[class(type)]
  #[error("Invalid JWK data")]
  #[property("code" = "ERR_CRYPTO_INVALID_JWK")]
  InvalidJwk,
  #[class(type)]
  #[error("JWK does not contain private key material")]
  #[property("code" = "ERR_CRYPTO_INVALID_JWK")]
  JwkMissingPrivateKey,
  #[class(generic)]
  #[error("Decapsulation failed")]
  #[property("code" = "ERR_CRYPTO_OPERATION_FAILED")]
  DecapsulationFailed,
  #[class(generic)]
  #[error("Encapsulation failed")]
  #[property("code" = "ERR_CRYPTO_OPERATION_FAILED")]
  EncapsulationFailed,
  #[class(generic)]
  #[error("Signing failed")]
  #[property("code" = "ERR_CRYPTO_OPERATION_FAILED")]
  SigningFailed,
  #[class(generic)]
  #[error("operation not supported for this keytype")]
  #[property("code" = "ERR_CRYPTO_OPERATION_FAILED")]
  NotSupportedForKeyType,
}

impl PqcAlg {
  /// The `asymmetricKeyType` name.
  pub fn name(self) -> &'static str {
    match self {
      PqcAlg::MlDsa44 => "ml-dsa-44",
      PqcAlg::MlDsa65 => "ml-dsa-65",
      PqcAlg::MlDsa87 => "ml-dsa-87",
      PqcAlg::MlKem768 => "ml-kem-768",
      PqcAlg::MlKem1024 => "ml-kem-1024",
    }
  }

  /// The JWK `alg` member.
  pub fn jwk_alg(self) -> &'static str {
    match self {
      PqcAlg::MlDsa44 => "ML-DSA-44",
      PqcAlg::MlDsa65 => "ML-DSA-65",
      PqcAlg::MlDsa87 => "ML-DSA-87",
      PqcAlg::MlKem768 => "ML-KEM-768",
      PqcAlg::MlKem1024 => "ML-KEM-1024",
    }
  }

  /// `Ok(None)` for a name that is not ML-DSA / ML-KEM at all, so callers
  /// can fall through to their other key types.
  pub fn from_name(name: &str) -> Result<Option<Self>, PqcError> {
    Ok(Some(match name {
      "ml-dsa-44" => PqcAlg::MlDsa44,
      "ml-dsa-65" => PqcAlg::MlDsa65,
      "ml-dsa-87" => PqcAlg::MlDsa87,
      "ml-kem-768" => PqcAlg::MlKem768,
      "ml-kem-1024" => PqcAlg::MlKem1024,
      "ml-kem-512" => return Err(PqcError::UnsupportedAlgorithm),
      _ => return Ok(None),
    }))
  }

  pub fn from_jwk_alg(alg: &str) -> Option<Self> {
    Some(match alg {
      "ML-DSA-44" => PqcAlg::MlDsa44,
      "ML-DSA-65" => PqcAlg::MlDsa65,
      "ML-DSA-87" => PqcAlg::MlDsa87,
      "ML-KEM-768" => PqcAlg::MlKem768,
      "ML-KEM-1024" => PqcAlg::MlKem1024,
      _ => return None,
    })
  }

  /// `Ok(None)` for an OID that is not ML-DSA / ML-KEM.
  pub fn from_oid(
    oid: &const_oid::ObjectIdentifier,
  ) -> Result<Option<Self>, PqcError> {
    Ok(Some(match *oid {
      ML_DSA_44_OID => PqcAlg::MlDsa44,
      ML_DSA_65_OID => PqcAlg::MlDsa65,
      ML_DSA_87_OID => PqcAlg::MlDsa87,
      ML_KEM_768_OID => PqcAlg::MlKem768,
      ML_KEM_1024_OID => PqcAlg::MlKem1024,
      ML_KEM_512_OID => return Err(PqcError::UnsupportedAlgorithm),
      _ => return Ok(None),
    }))
  }

  pub fn oid(self) -> const_oid::ObjectIdentifier {
    match self {
      PqcAlg::MlDsa44 => ML_DSA_44_OID,
      PqcAlg::MlDsa65 => ML_DSA_65_OID,
      PqcAlg::MlDsa87 => ML_DSA_87_OID,
      PqcAlg::MlKem768 => ML_KEM_768_OID,
      PqcAlg::MlKem1024 => ML_KEM_1024_OID,
    }
  }

  pub fn is_kem(self) -> bool {
    matches!(self, PqcAlg::MlKem768 | PqcAlg::MlKem1024)
  }

  pub fn seed_len(self) -> usize {
    if self.is_kem() { 64 } else { 32 }
  }

  pub fn public_len(self) -> usize {
    match self {
      PqcAlg::MlDsa44 => 1312,
      PqcAlg::MlDsa65 => 1952,
      PqcAlg::MlDsa87 => 2592,
      PqcAlg::MlKem768 => 1184,
      PqcAlg::MlKem1024 => 1568,
    }
  }

  fn dsa_signing(
    self,
  ) -> Option<&'static aws_lc_rs::signature::PqdsaSigningAlgorithm> {
    use aws_lc_rs::signature::*;
    match self {
      PqcAlg::MlDsa44 => Some(&ML_DSA_44_SIGNING),
      PqcAlg::MlDsa65 => Some(&ML_DSA_65_SIGNING),
      PqcAlg::MlDsa87 => Some(&ML_DSA_87_SIGNING),
      _ => None,
    }
  }

  fn dsa_verifying(
    self,
  ) -> Option<&'static aws_lc_rs::signature::PqdsaVerificationAlgorithm> {
    use aws_lc_rs::signature::*;
    match self {
      PqcAlg::MlDsa44 => Some(&ML_DSA_44),
      PqcAlg::MlDsa65 => Some(&ML_DSA_65),
      PqcAlg::MlDsa87 => Some(&ML_DSA_87),
      _ => None,
    }
  }

  fn kem(self) -> Option<&'static kem::Algorithm<kem::AlgorithmId>> {
    match self {
      PqcAlg::MlKem768 => Some(&kem::ML_KEM_768),
      PqcAlg::MlKem1024 => Some(&kem::ML_KEM_1024),
      _ => None,
    }
  }

  fn sig_len(self) -> usize {
    match self {
      PqcAlg::MlDsa44 => 2420,
      PqcAlg::MlDsa65 => 3309,
      PqcAlg::MlDsa87 => 4627,
      _ => 0,
    }
  }
}

/// An ML-DSA or ML-KEM private key, held as its seed with the derived keys.
#[derive(Clone)]
pub struct PqcPrivateKey {
  pub alg: PqcAlg,
  pub seed: Vec<u8>,
  pub public: Vec<u8>,
  /// ML-KEM: the FIPS 203 expanded decapsulation key. Empty for ML-DSA.
  expanded: Vec<u8>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct PqcPublicKey {
  pub alg: PqcAlg,
  pub public: Vec<u8>,
}

impl PartialEq for PqcPrivateKey {
  fn eq(&self, other: &Self) -> bool {
    self.alg == other.alg && self.seed == other.seed
  }
}

impl PqcPrivateKey {
  pub fn from_seed(alg: PqcAlg, seed: &[u8]) -> Result<Self, PqcError> {
    if seed.len() != alg.seed_len() {
      return Err(PqcError::DecodeError);
    }
    if let Some(signing) = alg.dsa_signing() {
      let key_pair = PqdsaKeyPair::from_seed(signing, seed)
        .map_err(|_| PqcError::DecodeError)?;
      return Ok(PqcPrivateKey {
        alg,
        seed: seed.to_vec(),
        public: key_pair.public_key().as_ref().to_vec(),
        expanded: Vec::new(),
      });
    }
    let mut d = [0u8; 32];
    let mut z = [0u8; 32];
    d.copy_from_slice(&seed[..32]);
    z.copy_from_slice(&seed[32..]);
    let (expanded, public) = match alg {
      PqcAlg::MlKem768 => {
        let (ek, dk) = fips203::ml_kem_768::KG::keygen_from_seed(d, z);
        (dk.into_bytes().to_vec(), ek.into_bytes().to_vec())
      }
      _ => {
        let (ek, dk) = fips203::ml_kem_1024::KG::keygen_from_seed(d, z);
        (dk.into_bytes().to_vec(), ek.into_bytes().to_vec())
      }
    };
    Ok(PqcPrivateKey {
      alg,
      seed: seed.to_vec(),
      public,
      expanded,
    })
  }

  pub fn generate(alg: PqcAlg) -> Self {
    let mut seed = vec![0u8; alg.seed_len()];
    rand::thread_rng().fill_bytes(&mut seed);
    Self::from_seed(alg, &seed).expect("a fresh seed has the right length")
  }

  pub fn to_public_key(&self) -> PqcPublicKey {
    PqcPublicKey {
      alg: self.alg,
      public: self.public.clone(),
    }
  }

  /// PKCS#8 `PrivateKeyInfo` in the seed-only form
  /// (`privateKey` = `[0] IMPLICIT OCTET STRING seed`).
  pub fn to_pkcs8_der(&self) -> Vec<u8> {
    let mut inner = Vec::with_capacity(2 + self.seed.len());
    inner.push(0x80);
    inner.push(self.seed.len() as u8);
    inner.extend_from_slice(&self.seed);
    let info = pkcs8::PrivateKeyInfo {
      algorithm: pkcs8::AlgorithmIdentifierRef {
        oid: self.alg.oid(),
        parameters: None,
      },
      private_key: &inner,
      public_key: None,
    };
    info.to_der().expect("PrivateKeyInfo encodes")
  }

  /// Reads the `privateKey` OCTET STRING contents of a PKCS#8 key.
  pub fn from_pkcs8_inner(alg: PqcAlg, inner: &[u8]) -> Result<Self, PqcError> {
    let (tag, body, rest) =
      read_tlv(inner).ok_or(PqcError::UnsupportedPrivateKeyFormat)?;
    if !rest.is_empty() {
      return Err(PqcError::DecodeError);
    }
    match tag {
      // seed [0] IMPLICIT OCTET STRING (BoringSSL's and OpenSSL's seed-only
      // form; tolerate the constructed tag too).
      0x80 | 0xa0 => Self::from_seed(alg, body),
      // both SEQUENCE { seed OCTET STRING, expandedKey OCTET STRING }:
      // BoringSSL refuses it, as it does the expanded-only form.
      _ => Err(PqcError::UnsupportedPrivateKeyFormat),
    }
  }

  pub fn sign(&self, data: &[u8]) -> Result<Vec<u8>, PqcError> {
    let signing = self
      .alg
      .dsa_signing()
      .ok_or(PqcError::NotSupportedForKeyType)?;
    let key_pair = PqdsaKeyPair::from_seed(signing, &self.seed)
      .map_err(|_| PqcError::SigningFailed)?;
    let mut signature = vec![0u8; self.alg.sig_len()];
    let len = key_pair
      .sign(data, &mut signature)
      .map_err(|_| PqcError::SigningFailed)?;
    signature.truncate(len);
    Ok(signature)
  }

  pub fn decapsulate(&self, ciphertext: &[u8]) -> Result<Vec<u8>, PqcError> {
    let alg = self.alg.kem().ok_or(PqcError::DecapsulationFailed)?;
    let key = kem::DecapsulationKey::new(alg, &self.expanded)
      .map_err(|_| PqcError::DecapsulationFailed)?;
    let secret = key
      .decapsulate(kem::Ciphertext::from(ciphertext))
      .map_err(|_| PqcError::DecapsulationFailed)?;
    Ok(secret.as_ref().to_vec())
  }

  pub fn to_jwk(&self) -> deno_core::serde_json::Value {
    deno_core::serde_json::json!({
      "kty": "AKP",
      "alg": self.alg.jwk_alg(),
      "pub": BASE64_URL_SAFE_NO_PAD.encode(&self.public),
      "priv": BASE64_URL_SAFE_NO_PAD.encode(&self.seed),
    })
  }
}

impl PqcPublicKey {
  pub fn from_raw(alg: PqcAlg, public: &[u8]) -> Result<Self, PqcError> {
    if public.len() != alg.public_len() {
      return Err(PqcError::DecodeError);
    }
    if let Some(kem_alg) = alg.kem() {
      kem::EncapsulationKey::new(kem_alg, public)
        .map_err(|_| PqcError::DecodeError)?;
    }
    Ok(PqcPublicKey {
      alg,
      public: public.to_vec(),
    })
  }

  pub fn to_spki_der(&self) -> Vec<u8> {
    let info = spki::SubjectPublicKeyInfoRef {
      algorithm: spki::AlgorithmIdentifierRef {
        oid: self.alg.oid(),
        parameters: None,
      },
      subject_public_key: spki::der::asn1::BitStringRef::from_bytes(
        &self.public,
      )
      .expect("public key fits a BIT STRING"),
    };
    info.to_der().expect("SubjectPublicKeyInfo encodes")
  }

  pub fn verify(
    &self,
    data: &[u8],
    signature: &[u8],
  ) -> Result<bool, PqcError> {
    let verifying = self
      .alg
      .dsa_verifying()
      .ok_or(PqcError::NotSupportedForKeyType)?;
    Ok(
      UnparsedPublicKey::new(verifying, &self.public)
        .verify(data, signature)
        .is_ok(),
    )
  }

  /// Returns `(shared_key, ciphertext)`.
  pub fn encapsulate(&self) -> Result<(Vec<u8>, Vec<u8>), PqcError> {
    let alg = self.alg.kem().ok_or(PqcError::EncapsulationFailed)?;
    let key = kem::EncapsulationKey::new(alg, &self.public)
      .map_err(|_| PqcError::EncapsulationFailed)?;
    let (ciphertext, secret) = key
      .encapsulate()
      .map_err(|_| PqcError::EncapsulationFailed)?;
    Ok((secret.as_ref().to_vec(), ciphertext.as_ref().to_vec()))
  }

  pub fn to_jwk(&self) -> deno_core::serde_json::Value {
    deno_core::serde_json::json!({
      "kty": "AKP",
      "alg": self.alg.jwk_alg(),
      "pub": BASE64_URL_SAFE_NO_PAD.encode(&self.public),
    })
  }
}

/// An `AKP` JWK (`alg`, `pub`, optional `priv`), as `node:crypto` imports it.
pub enum PqcJwkKey {
  Private(PqcPrivateKey),
  Public(PqcPublicKey),
}

pub fn import_jwk(
  alg: &str,
  public: Option<&str>,
  private: Option<&str>,
  is_public: bool,
) -> Result<PqcJwkKey, PqcError> {
  let alg = PqcAlg::from_jwk_alg(alg).ok_or(PqcError::InvalidJwk)?;
  let decode = |s: &str| {
    BASE64_URL_SAFE_NO_PAD
      .decode(s.trim_end_matches('='))
      .map_err(|_| PqcError::InvalidJwk)
  };
  let public = decode(public.ok_or(PqcError::InvalidJwk)?)?;
  if public.len() != alg.public_len() {
    return Err(PqcError::InvalidJwk);
  }
  if is_public {
    return PqcPublicKey::from_raw(alg, &public)
      .map(PqcJwkKey::Public)
      .map_err(|_| PqcError::InvalidJwk);
  }
  let seed = decode(private.ok_or(PqcError::JwkMissingPrivateKey)?)?;
  let key =
    PqcPrivateKey::from_seed(alg, &seed).map_err(|_| PqcError::InvalidJwk)?;
  if key.public != public {
    return Err(PqcError::InvalidJwk);
  }
  Ok(PqcJwkKey::Private(key))
}

/// One DER TLV with a short- or long-form length: `(tag, body, rest)`.
fn read_tlv(der: &[u8]) -> Option<(u8, &[u8], &[u8])> {
  let tag = *der.first()?;
  let first = *der.get(1)? as usize;
  let (len, start) = if first < 0x80 {
    (first, 2)
  } else {
    let n = first & 0x7f;
    if n == 0 || n > 3 {
      return None;
    }
    let mut len = 0usize;
    for i in 0..n {
      len = (len << 8) | *der.get(2 + i)? as usize;
    }
    (len, 2 + n)
  };
  let end = start.checked_add(len)?;
  Some((tag, der.get(start..end)?, der.get(end..)?))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn ml_dsa_seed_round_trip_and_sign() {
    for alg in [PqcAlg::MlDsa44, PqcAlg::MlDsa65, PqcAlg::MlDsa87] {
      let key = PqcPrivateKey::generate(alg);
      let der = key.to_pkcs8_der();
      let info = pkcs8::PrivateKeyInfo::try_from(der.as_slice()).unwrap();
      assert_eq!(info.algorithm.oid, alg.oid());
      let back =
        PqcPrivateKey::from_pkcs8_inner(alg, info.private_key).unwrap();
      assert!(back == key);
      assert_eq!(back.public, key.public);
      let sig = key.sign(b"data").unwrap();
      assert_eq!(sig.len(), alg.sig_len());
      let public = key.to_public_key();
      assert!(public.verify(b"data", &sig).unwrap());
      assert!(!public.verify(b"other", &sig).unwrap());
    }
  }

  #[test]
  fn ml_kem_encapsulate_decapsulate() {
    for alg in [PqcAlg::MlKem768, PqcAlg::MlKem1024] {
      let key = PqcPrivateKey::generate(alg);
      let (shared, ct) = key.to_public_key().encapsulate().unwrap();
      assert_eq!(shared.len(), 32);
      assert_eq!(key.decapsulate(&ct).unwrap(), shared);
      let der = key.to_pkcs8_der();
      let info = pkcs8::PrivateKeyInfo::try_from(der.as_slice()).unwrap();
      let back =
        PqcPrivateKey::from_pkcs8_inner(alg, info.private_key).unwrap();
      assert_eq!(back.decapsulate(&ct).unwrap(), shared);
    }
  }

  #[test]
  fn ml_kem_512_is_refused() {
    assert!(matches!(
      PqcAlg::from_name("ml-kem-512"),
      Err(PqcError::UnsupportedAlgorithm)
    ));
    assert!(matches!(
      PqcAlg::from_oid(&ML_KEM_512_OID),
      Err(PqcError::UnsupportedAlgorithm)
    ));
  }
}
