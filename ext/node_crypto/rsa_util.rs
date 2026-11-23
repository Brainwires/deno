// Copyright 2018-2026 the Deno authors. MIT license.

//! Glue between node:crypto and the `rsa` 0.10 crate.
//!
//! rsa 0.10 (crypto-bigint, constant-time arithmetic) changed three things
//! node:crypto depends on, and this module keeps node:crypto's behaviour as it
//! was on rsa 0.9:
//!
//! - Key sizes. rsa 0.10 refuses a public modulus under 1024 bits when it
//!   decodes or builds a key. Node (OpenSSL) accepts 512-bit keys and larger
//!   ones up to OpenSSL's own limit, so the key constructors here bound the
//!   modulus from above only.
//! - Key codecs. rsa 0.10 speaks pkcs1 0.8 / pkcs8 0.11 / spki 0.8 (`der`
//!   0.8), while the rest of node:crypto parses keys with pkcs8 0.10 / spki
//!   0.7 (`der` 0.7). The decoders here take and return the 0.7-era types and
//!   errors the callers already handle, and accept the same inputs rsa 0.9
//!   did (the `rsaEncryption` algorithm with NULL parameters).
//! - Digests. rsa 0.10's PSS and OAEP schemes take digest 0.11 hashers;
//!   node:crypto hashes with the digest 0.10 crates. [`Digest011`] lets a
//!   digest 0.10 hasher stand in, so every digest Node accepts keeps working
//!   with the same implementation, and [`pkcs1v15_sign_scheme`] builds the
//!   PKCS#1 v1.5 scheme from a digest 0.10 hasher's OID.

use der::Decode as _;
use der::pem::PemLabel as _;
use pkcs8::Document;
use pkcs8::PrivateKeyInfo;
use pkcs8::SecretDocument;
use rsa::BoxedUint;
use rsa::PrivateKey as RsaPrivateKey;
use rsa::PublicKey as RsaPublicKey;
use rsa::pkcs1::DecodeRsaPrivateKey as _;
use rsa::pkcs1::der::Decode as _;
use spki::SubjectPublicKeyInfoRef;

/// The largest RSA modulus node:crypto takes, in bits: OpenSSL's
/// `OPENSSL_RSA_MAX_MODULUS_BITS`.
pub(crate) const RSA_MAX_MODULUS_BITS: usize = 16384;

pub(crate) const RSA_PRIVATE_KEY_PEM_LABEL: &str = "RSA PRIVATE KEY";
pub(crate) const RSA_PUBLIC_KEY_PEM_LABEL: &str = "RSA PUBLIC KEY";

const RSA_ENCRYPTION_OID: const_oid::ObjectIdentifier =
  const_oid::ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1");

/// The `rsaEncryption` AlgorithmIdentifier with its NULL parameters.
const RSA_ALGORITHM_ID: pkcs8::AlgorithmIdentifierRef<'static> =
  pkcs8::AlgorithmIdentifierRef {
    oid: RSA_ENCRYPTION_OID,
    parameters: Some(der::asn1::AnyRef::NULL),
  };

/// An unsigned big-endian integer with no leading zero bytes removed
/// (`BoxedUint` sizes itself to the value).
pub(crate) fn uint(bytes: &[u8]) -> BoxedUint {
  BoxedUint::from_be_slice_vartime(bytes)
}

/// `value` big-endian with leading zero bytes removed, as `BigUint::to_bytes_be`
/// gave it (a zero is one zero byte).
pub(crate) fn uint_to_be_bytes(value: &BoxedUint) -> Vec<u8> {
  let bytes = value.to_be_bytes_trimmed_vartime().into_vec();
  if bytes.is_empty() { vec![0] } else { bytes }
}

/// `value` big-endian, left-padded with zeros to `len` bytes.
pub(crate) fn uint_to_be_bytes_padded(
  value: &BoxedUint,
  len: usize,
) -> Vec<u8> {
  let mut bytes = value.to_be_bytes_trimmed_vartime().into_vec();
  if bytes.len() < len {
    let mut padded = vec![0u8; len - bytes.len()];
    padded.append(&mut bytes);
    bytes = padded;
  }
  bytes
}

/// A public key from its modulus and exponent, with any modulus up to
/// [`RSA_MAX_MODULUS_BITS`] (rsa 0.10's `PublicKey::new` refuses one under
/// 1024 bits). The crate's other checks (an odd modulus above the exponent,
/// the exponent's range) still apply.
pub(crate) fn public_key_new(
  n: BoxedUint,
  e: BoxedUint,
) -> rsa::Result<RsaPublicKey> {
  RsaPublicKey::new_with_size_limits(n, e, 0..=RSA_MAX_MODULUS_BITS)
}

/// A PKCS#1 `RSAPublicKey` (DER).
pub(crate) fn public_key_from_pkcs1_der(
  der: &[u8],
) -> rsa::pkcs1::Result<RsaPublicKey> {
  let key = rsa::pkcs1::RsaPublicKeyRef::from_der(der)?;
  public_key_new(
    uint(key.modulus.as_bytes()),
    uint(key.public_exponent.as_bytes()),
  )
  .map_err(|_| rsa::pkcs1::Error::KeyMalformed)
}

/// A PKCS#1 `RSAPublicKey` in an `RSA PUBLIC KEY` PEM block.
pub(crate) fn public_key_from_pkcs1_pem(
  pem: &str,
) -> spki::Result<RsaPublicKey> {
  let (label, doc) = Document::from_pem(pem)?;
  if label != RSA_PUBLIC_KEY_PEM_LABEL {
    return Err(
      der::Error::from(der::pem::Error::UnexpectedTypeLabel {
        expected: RSA_PUBLIC_KEY_PEM_LABEL,
      })
      .into(),
    );
  }
  public_key_from_pkcs1_der(doc.as_bytes())
    .map_err(|_| spki::Error::KeyMalformed)
}

/// An `rsaEncryption` SubjectPublicKeyInfo (DER).
pub(crate) fn public_key_from_spki_der(
  der: &[u8],
) -> spki::Result<RsaPublicKey> {
  let spki = SubjectPublicKeyInfoRef::try_from(der)?;
  verify_algorithm_id(&spki.algorithm)?;
  public_key_from_pkcs1_der(spki.subject_public_key.raw_bytes())
    .map_err(|_| spki::Error::KeyMalformed)
}

/// An `rsaEncryption` SubjectPublicKeyInfo in a `PUBLIC KEY` PEM block.
pub(crate) fn public_key_from_spki_pem(
  pem: &str,
) -> spki::Result<RsaPublicKey> {
  let (label, doc) = Document::from_pem(pem)?;
  SubjectPublicKeyInfoRef::validate_pem_label(label)?;
  public_key_from_spki_der(doc.as_bytes())
}

/// A PKCS#1 `RSAPrivateKey` (DER). rsa 0.10 sets no lower bound on a private
/// key's modulus, so its own decoder is used.
pub(crate) fn private_key_from_pkcs1_der(
  der: &[u8],
) -> pkcs8::Result<RsaPrivateKey> {
  RsaPrivateKey::from_pkcs1_der(der).map_err(|_| pkcs8::Error::KeyMalformed)
}

/// A PKCS#1 `RSAPrivateKey` in an `RSA PRIVATE KEY` PEM block.
pub(crate) fn private_key_from_pkcs1_pem(
  pem: &str,
) -> pkcs8::Result<RsaPrivateKey> {
  let (label, doc) = SecretDocument::from_pem(pem)?;
  if label != RSA_PRIVATE_KEY_PEM_LABEL {
    return Err(
      der::Error::from(der::pem::Error::UnexpectedTypeLabel {
        expected: RSA_PRIVATE_KEY_PEM_LABEL,
      })
      .into(),
    );
  }
  private_key_from_pkcs1_der(doc.as_bytes())
}

/// An `rsaEncryption` PKCS#8 PrivateKeyInfo (DER).
pub(crate) fn private_key_from_pkcs8_der(
  der: &[u8],
) -> pkcs8::Result<RsaPrivateKey> {
  let info = PrivateKeyInfo::from_der(der)?;
  verify_algorithm_id(&info.algorithm)?;
  private_key_from_pkcs1_der(info.private_key)
}

/// An `rsaEncryption` PKCS#8 PrivateKeyInfo in a `PRIVATE KEY` PEM block.
pub(crate) fn private_key_from_pkcs8_pem(
  pem: &str,
) -> pkcs8::Result<RsaPrivateKey> {
  let (label, doc) = SecretDocument::from_pem(pem)?;
  PrivateKeyInfo::validate_pem_label(label)?;
  private_key_from_pkcs8_der(doc.as_bytes())
}

/// A PKCS#1 `RSAPrivateKey` wrapped in an `rsaEncryption` PKCS#8
/// PrivateKeyInfo, as pkcs1 0.7's `SecretDocument::from_pkcs1_der` gave it.
/// The PKCS#1 structure itself is checked when the key is decoded.
pub(crate) fn pkcs1_private_key_document(
  der: &[u8],
) -> pkcs8::Result<SecretDocument> {
  SecretDocument::try_from(PrivateKeyInfo {
    algorithm: RSA_ALGORITHM_ID,
    private_key: der,
    public_key: None,
  })
}

/// A PKCS#1 `RSAPublicKey` wrapped in an `rsaEncryption`
/// SubjectPublicKeyInfo, as pkcs1 0.7's `Document::from_pkcs1_der` gave it.
pub(crate) fn pkcs1_public_key_document(der: &[u8]) -> spki::Result<Document> {
  Ok(Document::try_from(SubjectPublicKeyInfoRef {
    algorithm: RSA_ALGORITHM_ID,
    subject_public_key: der::asn1::BitStringRef::from_bytes(der)?,
  })?)
}

/// rsa 0.9's check of a key's AlgorithmIdentifier: `rsaEncryption` with NULL
/// parameters.
fn verify_algorithm_id(
  algorithm: &pkcs8::AlgorithmIdentifierRef<'_>,
) -> spki::Result<()> {
  algorithm.assert_algorithm_oid(RSA_ENCRYPTION_OID)?;
  if algorithm.parameters_any()? != der::asn1::Null.into() {
    return Err(spki::Error::KeyMalformed);
  }
  Ok(())
}

/// The PKCS#1 v1.5 signature scheme for `D`: the DigestInfo prefix for its OID
/// and its output length (what `rsa::Pkcs1v15Sign::new` builds for a digest
/// 0.11 hasher).
pub(crate) fn pkcs1v15_sign_scheme<D>() -> rsa::Pkcs1v15Sign
where
  D: digest::Digest + const_oid::AssociatedOid,
{
  let oid = D::OID;
  let oid = oid.as_bytes();
  let hash_len = <D as digest::Digest>::output_size();
  // DigestInfo ::= SEQUENCE {
  //   digestAlgorithm SEQUENCE { OBJECT IDENTIFIER, NULL },
  //   digest          OCTET STRING }
  // The prefix is everything before the digest bytes. Every digest with an
  // OID here is short enough for single-byte DER lengths.
  let mut prefix = Vec::with_capacity(oid.len() + 10);
  prefix.extend_from_slice(&[
    0x30,
    (oid.len() + 8 + hash_len) as u8,
    0x30,
    (oid.len() + 4) as u8,
    0x06,
    oid.len() as u8,
  ]);
  prefix.extend_from_slice(oid);
  prefix.extend_from_slice(&[0x05, 0x00, 0x04, hash_len as u8]);
  rsa::Pkcs1v15Sign {
    hash_len: Some(hash_len),
    prefix: prefix.into_boxed_slice(),
  }
}

/// A digest 0.10 hasher behind digest 0.11's traits, for rsa 0.10's PSS and
/// OAEP schemes.
#[derive(Clone, Default)]
pub(crate) struct Digest011<D>(D);

impl<D> rsa_digest::HashMarker for Digest011<D> {}

impl<D: digest::Update> rsa_digest::Update for Digest011<D> {
  fn update(&mut self, data: &[u8]) {
    digest::Update::update(&mut self.0, data);
  }
}

impl<D> rsa_digest::OutputSizeUser for Digest011<D>
where
  D: digest::OutputSizeUser,
  D::OutputSize: rsa_digest::array::ArraySize,
{
  type OutputSize = D::OutputSize;
}

impl<D> rsa_digest::FixedOutput for Digest011<D>
where
  D: digest::FixedOutput,
  D::OutputSize: rsa_digest::array::ArraySize,
{
  fn finalize_into(self, out: &mut rsa_digest::Output<Self>) {
    out.copy_from_slice(&digest::FixedOutput::finalize_fixed(self.0));
  }
}

impl<D: digest::Reset> rsa_digest::Reset for Digest011<D> {
  fn reset(&mut self) {
    digest::Reset::reset(&mut self.0);
  }
}

impl<D> rsa_digest::FixedOutputReset for Digest011<D>
where
  D: digest::FixedOutputReset,
  D::OutputSize: rsa_digest::array::ArraySize,
{
  fn finalize_into_reset(&mut self, out: &mut rsa_digest::Output<Self>) {
    out.copy_from_slice(&digest::FixedOutputReset::finalize_fixed_reset(
      &mut self.0,
    ));
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn pkcs1v15_prefix_matches_rsa_crate() {
    // rsa 0.10 builds the same scheme for its digest 0.11 SHA-256.
    let ours = pkcs1v15_sign_scheme::<sha2::Sha256>();
    let theirs = rsa::Pkcs1v15Sign::new::<rsa::sha2::Sha256>();
    assert_eq!(ours.prefix, theirs.prefix);
    assert_eq!(ours.hash_len, theirs.hash_len);
    let ours = pkcs1v15_sign_scheme::<sha2::Sha512>();
    let theirs = rsa::Pkcs1v15Sign::new::<rsa::sha2::Sha512>();
    assert_eq!(ours.prefix, theirs.prefix);
  }

  #[test]
  fn digest011_matches_digest010() {
    use rsa_digest::Digest as _;
    let ours = Digest011::<sha2::Sha256>::digest(b"abc");
    let theirs = <sha2::Sha256 as digest::Digest>::digest(b"abc");
    assert_eq!(ours.as_slice(), theirs.as_slice());
  }

  #[test]
  fn small_keys_still_decode() {
    // A 512-bit key, which rsa 0.10's own decoders refuse.
    let key =
      RsaPrivateKey::new_unchecked(&mut rsa_getrandom::SysRng, 512).unwrap();
    use rsa::pkcs1::EncodeRsaPrivateKey as _;
    use rsa::pkcs1::EncodeRsaPublicKey as _;
    let private_der = key.to_pkcs1_der().unwrap();
    let decoded = private_key_from_pkcs1_der(private_der.as_bytes()).unwrap();
    assert_eq!(decoded, key);
    let public_der = key.to_public_key().to_pkcs1_der().unwrap();
    let public = public_key_from_pkcs1_der(public_der.as_bytes()).unwrap();
    assert_eq!(public, key.to_public_key());
    let doc = pkcs1_public_key_document(public_der.as_bytes()).unwrap();
    let public = public_key_from_spki_der(doc.as_bytes()).unwrap();
    assert_eq!(public, key.to_public_key());
    let doc = pkcs1_private_key_document(private_der.as_bytes()).unwrap();
    let decoded = private_key_from_pkcs8_der(doc.as_bytes()).unwrap();
    assert_eq!(decoded, key);
  }
}
