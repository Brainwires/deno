// Copyright 2018-2026 the Deno authors. MIT license.

//! Known-answer ("golden") tests for the WebCrypto elliptic-curve paths
//! (`importKey`/`exportKey`, ECDSA `sign`/`verify`, ECDH `deriveBits`).
//!
//! The expected values were produced independently of this crate, with
//! Python `cryptography` 49 on OpenSSL 3.6 (RFC 6979 deterministic ECDSA with
//! the curve's own hash, ECDH, X9.62 points, SPKI and PKCS#8 DER). They pin
//! the bytes this crate emits across RustCrypto upgrades (elliptic-curve
//! 0.13 -> 0.14, RUSTSEC-2026-0387): an upgrade must not change any of them.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::Algorithm;
use crate::CryptoHash;
use crate::CryptoNamedCurve;
use crate::KeyData;
use crate::KeyType;
use crate::SignArg;
use crate::VerifyArg;
use crate::export_key::ExportKeyAlgorithm;
use crate::export_key::ExportKeyFormat;
use crate::export_key::ExportKeyOptions;
use crate::export_key::ExportKeyResult;
use crate::export_key::export_key_with_raw;
use crate::import_key::ImportKeyOptions;
use crate::import_key::ImportKeyResult;
use crate::import_key::op_crypto_import_key_inner;
use crate::shared::EcNamedCurve;
use crate::shared::RawKeyData;
use crate::shared::RustRawKeyData;

struct Vector {
  curve: EcNamedCurve,
  crypto_curve: CryptoNamedCurve,
  hash: CryptoHash,
  /// Private scalar of key A.
  d_a: &'static str,
  /// RFC 6979 signature of the ASCII message `sample`, IEEE P1363 (r || s).
  sig_p1363: &'static str,
  pub_uncompressed: &'static str,
  pub_compressed: &'static str,
  spki: &'static str,
  pkcs8: &'static str,
  /// Public key of the ECDH peer B.
  pub_b_uncompressed: &'static str,
  /// x-coordinate of d_a * Q_b.
  ecdh: &'static str,
}

const VECTORS: &[Vector] = &[
  Vector {
    curve: EcNamedCurve::P256,
    crypto_curve: CryptoNamedCurve::P256,
    hash: CryptoHash::Sha256,
    d_a: "1cce01dc9a14044033a1818f2938edbb8ada4524f356daa858fdd9c16629735e",
    sig_p1363: concat!(
      "9e439cedecc2cf66c4f1bb6a1de0a09acc58e4bc995a9f54a6e6b79af643d1be",
      "e6a921e2b29627502bbe7a9a32798932b46f4893ca47aedccba108bff5df5918",
    ),
    pub_uncompressed: concat!(
      "04b764d9e8b3f4e8f06b7f7720b83323c3b94d1c61cb182381e6a185584266a5",
      "1583df7f1ae11522ac03a7d059ee4d03f7615532a3ec0159e8adf412f03275e8",
      "59",
    ),
    pub_compressed: concat!(
      "03b764d9e8b3f4e8f06b7f7720b83323c3b94d1c61cb182381e6a185584266a5",
      "15",
    ),
    spki: concat!(
      "3059301306072a8648ce3d020106082a8648ce3d03010703420004b764d9e8b3",
      "f4e8f06b7f7720b83323c3b94d1c61cb182381e6a185584266a51583df7f1ae1",
      "1522ac03a7d059ee4d03f7615532a3ec0159e8adf412f03275e859",
    ),
    pkcs8: concat!(
      "308187020100301306072a8648ce3d020106082a8648ce3d030107046d306b02",
      "010104201cce01dc9a14044033a1818f2938edbb8ada4524f356daa858fdd9c1",
      "6629735ea14403420004b764d9e8b3f4e8f06b7f7720b83323c3b94d1c61cb18",
      "2381e6a185584266a51583df7f1ae11522ac03a7d059ee4d03f7615532a3ec01",
      "59e8adf412f03275e859",
    ),
    pub_b_uncompressed: concat!(
      "04f20a286ed0767b04308a138fc4c27e33ae0e5fa713bef4a4bb23e99a531ee9",
      "4dd40ccfcdc804b074f1836a1ee8376066c34cdb555a8ef290304ca3ce943d43",
      "f2",
    ),
    ecdh: "235dc63ea694d9d118aa2caca3740ea996e4f14721ff1a806f0f98c181fb4d89",
  },
  Vector {
    curve: EcNamedCurve::P384,
    crypto_curve: CryptoNamedCurve::P384,
    hash: CryptoHash::Sha384,
    d_a: concat!(
      "e07f77f5437eeb7882ff7cd061cd883e2ed5aec560892a1b7e9ff6fb9646b8c2",
      "1d509f89cf490388193710dfab700102",
    ),
    sig_p1363: concat!(
      "6e1c25dfa5f2c4e9b4aefe50c42991325e8a5e879b582285d32b778ed3379d5a",
      "fed3d2035dac9994ffff97436a4aa2d848fa8c06bd82e46240194e74146948d6",
      "680ae85d9107b48b54f7429bcf10851c354f052b32d0cca79df76eb258ca8353",
    ),
    pub_uncompressed: concat!(
      "04e1ebd881ea3d71fa1749086ca505f67cddb847a8d9e8fb732895a7bf50f5c3",
      "3dc54f49ce6dc6587fdd45a107f58a7cd95940ac4de9aead13815d9eb0aa5312",
      "cdb2374d7adac25fe8b7215896fd54c45f533f11d1442bc4fbdb2f426b7ea55d",
      "fe",
    ),
    pub_compressed: concat!(
      "02e1ebd881ea3d71fa1749086ca505f67cddb847a8d9e8fb732895a7bf50f5c3",
      "3dc54f49ce6dc6587fdd45a107f58a7cd9",
    ),
    spki: concat!(
      "3076301006072a8648ce3d020106052b8104002203620004e1ebd881ea3d71fa",
      "1749086ca505f67cddb847a8d9e8fb732895a7bf50f5c33dc54f49ce6dc6587f",
      "dd45a107f58a7cd95940ac4de9aead13815d9eb0aa5312cdb2374d7adac25fe8",
      "b7215896fd54c45f533f11d1442bc4fbdb2f426b7ea55dfe",
    ),
    pkcs8: concat!(
      "3081b6020100301006072a8648ce3d020106052b8104002204819e30819b0201",
      "010430e07f77f5437eeb7882ff7cd061cd883e2ed5aec560892a1b7e9ff6fb96",
      "46b8c21d509f89cf490388193710dfab700102a16403620004e1ebd881ea3d71",
      "fa1749086ca505f67cddb847a8d9e8fb732895a7bf50f5c33dc54f49ce6dc658",
      "7fdd45a107f58a7cd95940ac4de9aead13815d9eb0aa5312cdb2374d7adac25f",
      "e8b7215896fd54c45f533f11d1442bc4fbdb2f426b7ea55dfe",
    ),
    pub_b_uncompressed: concat!(
      "04a327dc484bae04f9609e57f96f6cbce91e331ace56a86b5bd81e88e954b669",
      "70e1721749e232fb6a1ddac90111d8bfecb2aa3d9ef4c87261be6e17616686f6",
      "d99d03f05bcc9f72fdae505dbf6e5863d2803d0514737ccded38b4b7fa972b58",
      "02",
    ),
    ecdh: concat!(
      "f4cceff3386474012f4f1b3bb899badb9491e7a47b5d0e7d3c335a251c71ad86",
      "87de9113dbbe45d6acc99eef4887e158",
    ),
  },
  Vector {
    curve: EcNamedCurve::P521,
    crypto_curve: CryptoNamedCurve::P521,
    hash: CryptoHash::Sha512,
    d_a: concat!(
      "0185095656d36b3d8816460cfbebab7b371cf4eb0f7716e446393c571b1acba1",
      "46c1001f5b33e37fb93f7b9a13de2372b42b1b387f8c1d1c88d2a7ca337aa94b",
      "6664",
    ),
    sig_p1363: concat!(
      "0144ec2f7fd9482eee38abd20016afc8ac308e7d186d6bca5a395a1b043301c9",
      "88fd43f2297d8a810e1c65667b7f627a4423d65d375840b10071b7df7ba438c1",
      "2412006ca7c9ef52b5e137b52081cb84fd49a0902df5424ef7828a8f1c7911ad",
      "17151a58cdc1d7994ae3e7c343efa17d5136f377dd6808cb9610da4ccf5d3e3a",
      "81832bbb",
    ),
    pub_uncompressed: concat!(
      "04001a546e3b51bc197a4d7c0d990db501703bf2d6fc89da8d76389497d50f5e",
      "540681d693bac8ce46e9c53637cbd0897ced79e6ef83af9f16a395853462dd9d",
      "93669c016d8199f03811407498ac943d5f105450ba0568e87d4739b56a92e1db",
      "15b3a8ef1de37cebb369061b9feead14b60d610e936a4b2143add4e361b89ba8",
      "d63f03c82b",
    ),
    pub_compressed: concat!(
      "03001a546e3b51bc197a4d7c0d990db501703bf2d6fc89da8d76389497d50f5e",
      "540681d693bac8ce46e9c53637cbd0897ced79e6ef83af9f16a395853462dd9d",
      "93669c",
    ),
    spki: concat!(
      "30819b301006072a8648ce3d020106052b810400230381860004001a546e3b51",
      "bc197a4d7c0d990db501703bf2d6fc89da8d76389497d50f5e540681d693bac8",
      "ce46e9c53637cbd0897ced79e6ef83af9f16a395853462dd9d93669c016d8199",
      "f03811407498ac943d5f105450ba0568e87d4739b56a92e1db15b3a8ef1de37c",
      "ebb369061b9feead14b60d610e936a4b2143add4e361b89ba8d63f03c82b",
    ),
    pkcs8: concat!(
      "3081ee020100301006072a8648ce3d020106052b810400230481d63081d30201",
      "0104420185095656d36b3d8816460cfbebab7b371cf4eb0f7716e446393c571b",
      "1acba146c1001f5b33e37fb93f7b9a13de2372b42b1b387f8c1d1c88d2a7ca33",
      "7aa94b6664a181890381860004001a546e3b51bc197a4d7c0d990db501703bf2",
      "d6fc89da8d76389497d50f5e540681d693bac8ce46e9c53637cbd0897ced79e6",
      "ef83af9f16a395853462dd9d93669c016d8199f03811407498ac943d5f105450",
      "ba0568e87d4739b56a92e1db15b3a8ef1de37cebb369061b9feead14b60d610e",
      "936a4b2143add4e361b89ba8d63f03c82b",
    ),
    pub_b_uncompressed: concat!(
      "0400fd25411362929a956a59a1ad6bf5c1810405aa01eeba974510a5e63ab75d",
      "339b242ef9c18101dc0c848f3f1a6179ffbc981aa1b83139213609afb702de9c",
      "6d2ae5009ad8e5e3acb30befccb039085c344f8a6109dbcd0f7b9da2a6626581",
      "8e1e225f1ced237b8c3846bfdf514dcc81e022dad8baf856eda2917304d7178a",
      "44a82a2070",
    ),
    ecdh: concat!(
      "0031fb37315737b004cc56d8cdc84537c53465a4a83a6f4810e0dfbbe6ae5d62",
      "5e5df322632f1d28782e52a8855e318d8dc4bfd9c7cd0f0389f47dec3bdfa3d3",
      "d1a8",
    ),
  },
];

fn hex(s: &str) -> Vec<u8> {
  (0..s.len())
    .step_by(2)
    .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
    .collect()
}

fn b64(bytes: &[u8]) -> String {
  URL_SAFE_NO_PAD.encode(bytes)
}

/// `(x, y)` of key A, base64url (the JWK coordinate encoding).
fn jwk_xy(v: &Vector) -> (String, String) {
  let point = hex(v.pub_uncompressed);
  let len = (point.len() - 1) / 2;
  (b64(&point[1..1 + len]), b64(&point[1 + len..]))
}

fn import(v: &Vector, key_data: crate::import_key::KeyData) -> RustRawKeyData {
  match op_crypto_import_key_inner(
    ImportKeyOptions::Ecdsa {
      named_curve: v.curve,
    },
    key_data,
  ) {
    Ok(ImportKeyResult::Ec { raw_data }) => raw_data,
    _ => panic!("EC import failed"),
  }
}

fn private_bytes(raw: RustRawKeyData) -> Vec<u8> {
  match raw {
    RustRawKeyData::Private(bytes) => bytes.to_vec(),
    _ => panic!("expected a private key"),
  }
}

fn public_bytes(raw: RustRawKeyData) -> Vec<u8> {
  match raw {
    RustRawKeyData::Public(bytes) => bytes.to_vec(),
    _ => panic!("expected a public key"),
  }
}

fn export(
  format: ExportKeyFormat,
  algorithm: ExportKeyAlgorithm,
  key: &RawKeyData,
) -> ExportKeyResult {
  match export_key_with_raw(ExportKeyOptions::new(format, algorithm), key) {
    Ok(result) => result,
    Err(_) => panic!("EC export failed"),
  }
}

fn ecdsa(v: &Vector) -> ExportKeyAlgorithm {
  ExportKeyAlgorithm::Ecdsa {
    named_curve: v.curve,
  }
}

fn ecdh(v: &Vector) -> ExportKeyAlgorithm {
  ExportKeyAlgorithm::Ecdh {
    named_curve: v.curve,
  }
}

fn bytes_of(result: ExportKeyResult) -> Vec<u8> {
  match result {
    ExportKeyResult::Raw(b)
    | ExportKeyResult::Spki(b)
    | ExportKeyResult::Pkcs8(b) => b.to_vec(),
    _ => panic!("expected a byte export"),
  }
}

fn private_key(v: &Vector) -> RawKeyData {
  RawKeyData::Private(hex(v.pkcs8).into())
}

#[test]
fn ec_golden_import() {
  for v in VECTORS {
    let (x, y) = jwk_xy(v);
    let d = b64(&hex(v.d_a));

    assert_eq!(
      private_bytes(import(v, crate::import_key::KeyData::Pkcs8(hex(v.pkcs8)))),
      hex(v.pkcs8)
    );
    // A JWK private key is re-encoded as PKCS#8.
    assert_eq!(
      private_bytes(import(
        v,
        crate::import_key::KeyData::JwkPrivateEc {
          x: x.clone(),
          y: y.clone(),
          d,
        }
      )),
      hex(v.pkcs8)
    );
    assert_eq!(
      public_bytes(import(v, crate::import_key::KeyData::JwkPublicEc { x, y })),
      hex(v.pub_uncompressed)
    );
    assert_eq!(
      public_bytes(import(v, crate::import_key::KeyData::Spki(hex(v.spki)))),
      hex(v.pub_uncompressed)
    );
    // A raw point is validated, then kept as given.
    for point in [v.pub_uncompressed, v.pub_compressed] {
      assert_eq!(
        public_bytes(import(v, crate::import_key::KeyData::Raw(hex(point)))),
        hex(point)
      );
    }
    // Off-curve points are refused.
    let mut off_curve = hex(v.pub_uncompressed);
    *off_curve.last_mut().unwrap() ^= 1;
    assert!(
      op_crypto_import_key_inner(
        ImportKeyOptions::Ecdh {
          named_curve: v.curve
        },
        crate::import_key::KeyData::Raw(off_curve),
      )
      .is_err()
    );
  }
}

#[test]
fn ec_golden_export() {
  for v in VECTORS {
    let (x, y) = jwk_xy(v);
    let private = private_key(v);
    let compressed = RawKeyData::Public(hex(v.pub_compressed).into());

    for key in [&private, &compressed] {
      assert_eq!(
        bytes_of(export(ExportKeyFormat::Raw, ecdsa(v), key)),
        hex(v.pub_uncompressed)
      );
      for algorithm in [ecdsa(v), ecdh(v)] {
        assert_eq!(
          bytes_of(export(ExportKeyFormat::Spki, algorithm, key)),
          hex(v.spki)
        );
      }
      match export(ExportKeyFormat::JwkPublic, ecdsa(v), key) {
        ExportKeyResult::JwkPublicEc { x: jx, y: jy } => {
          assert_eq!((jx, jy), (x.clone(), y.clone()));
        }
        _ => panic!("expected a public JWK"),
      }
    }

    assert_eq!(
      bytes_of(export(ExportKeyFormat::Pkcs8, ecdsa(v), &private)),
      hex(v.pkcs8)
    );
    match export(ExportKeyFormat::JwkPrivate, ecdsa(v), &private) {
      ExportKeyResult::JwkPrivateEc {
        x: jx,
        y: jy,
        d: jd,
      } => {
        assert_eq!((jx, jy, jd), (x, y, b64(&hex(v.d_a))));
      }
      _ => panic!("expected a private JWK"),
    }
  }
}

fn sign(v: &Vector, hash: CryptoHash) -> Result<Vec<u8>, crate::CryptoError> {
  crate::sign_key_sync(
    KeyData::from(&private_key(v)),
    SignArg::new(Algorithm::Ecdsa, None, Some(hash), Some(v.crypto_curve)),
    b"sample",
  )
}

fn verify(
  v: &Vector,
  key: &RawKeyData,
  hash: CryptoHash,
  signature: &[u8],
  message: &[u8],
) -> bool {
  crate::verify_key_sync(
    KeyData::from(key),
    VerifyArg::new(
      Algorithm::Ecdsa,
      None,
      Some(hash),
      signature.to_vec(),
      Some(v.crypto_curve),
    ),
    message,
  )
  .unwrap()
}

/// Before the migration P-521 signed with a random nonce (`p521` 0.13's
/// `sign_prehash` used `OsRng`), so its signatures are checked by
/// verification only; P-256 and P-384 are RFC 6979 and pinned to the byte.
fn is_randomized(v: &Vector) -> bool {
  v.curve == EcNamedCurve::P521
}

#[test]
fn ec_golden_ecdsa_rfc6979() {
  for v in VECTORS {
    let signature = sign(v, v.hash).unwrap();
    if !is_randomized(v) {
      assert_eq!(signature, hex(v.sig_p1363));
    }

    let public = RawKeyData::Public(hex(v.pub_uncompressed).into());
    for key in [&public, &private_key(v)] {
      for sig in [&signature, &hex(v.sig_p1363)] {
        assert!(verify(v, key, v.hash, sig, b"sample"));
        assert!(!verify(v, key, v.hash, sig, b"sampl3"));
      }
    }
  }
}

/// `sign` with every hash, as this crate produced it before the
/// elliptic-curve 0.14 migration. `None`: refused (SHA-1 is shorter than
/// half the P-384 field). P-521 is absent: its signatures were randomized.
const CROSS_HASH: &[(EcNamedCurve, CryptoHash, Option<&str>)] = &[
  (
    EcNamedCurve::P256,
    CryptoHash::Sha1,
    Some(concat!(
      "7dda5b7c6a3c71d7ab301038f21564e5cf84f4e5f45411060ec9d168922ca810",
      "3fc8ca3e25af2fc3425cbd1fd9a1c7756da6fdff36fb5c4b628cdcd85994f339"
    )),
  ),
  (
    EcNamedCurve::P256,
    CryptoHash::Sha256,
    Some(concat!(
      "9e439cedecc2cf66c4f1bb6a1de0a09acc58e4bc995a9f54a6e6b79af643d1be",
      "e6a921e2b29627502bbe7a9a32798932b46f4893ca47aedccba108bff5df5918"
    )),
  ),
  (
    EcNamedCurve::P256,
    CryptoHash::Sha384,
    Some(concat!(
      "0f93da613a9e4103bd4fb10ed1f29c02aa64e9837104917ae6ff9ba8203614d9",
      "6d29fe78a45955d1450c8fa1ec5f8137459cedd40e5529ba07286a6f14ce1ad2"
    )),
  ),
  (
    EcNamedCurve::P256,
    CryptoHash::Sha512,
    Some(concat!(
      "24787ec52475034407175eb6d1e235cf055a20e708a650f839b09dfc192e32ec",
      "35024c8ac29b88f5a22637d32444ce0ff685dab12876cf72bacd6ede1af3382d"
    )),
  ),
  (EcNamedCurve::P384, CryptoHash::Sha1, None),
  (
    EcNamedCurve::P384,
    CryptoHash::Sha256,
    Some(concat!(
      "b8a05c71699365d6f1f7021b3e93b3a256f4f8bd5d0d48beba41b103c772baa3",
      "ac70673d4e2f3cffbae229dc43f9db9f0e5a563c17a4a1c4d9510cb2c815c5d9",
      "b812b8184ab9924472b419d53a96025eda7982d55d8fb9bca83a191b9f4509a2"
    )),
  ),
  (
    EcNamedCurve::P384,
    CryptoHash::Sha384,
    Some(concat!(
      "6e1c25dfa5f2c4e9b4aefe50c42991325e8a5e879b582285d32b778ed3379d5a",
      "fed3d2035dac9994ffff97436a4aa2d848fa8c06bd82e46240194e74146948d6",
      "680ae85d9107b48b54f7429bcf10851c354f052b32d0cca79df76eb258ca8353"
    )),
  ),
  (
    EcNamedCurve::P384,
    CryptoHash::Sha512,
    Some(concat!(
      "9badf52378de0a58941a3a65c0608f4dc4bd56adcafae86cbd2377564cc8179c",
      "acb0acee59fe1cdf12d2eda772ccb3871bb586da75b507cee01145818ba915d6",
      "02562beef5cb8b6dc16b7caaa51438fc2ff2aeb53314c1444651093ea457f4f8"
    )),
  ),
];

/// Every hash with every curve: the same signatures, and the same refusal
/// of SHA-1 with P-384 (ecdsa 0.17 would accept it; the crate keeps
/// refusing it).
#[test]
fn ec_golden_ecdsa_cross_hash() {
  let public = |v: &Vector| RawKeyData::Public(hex(v.pub_uncompressed).into());
  for v in VECTORS {
    for hash in [
      CryptoHash::Sha1,
      CryptoHash::Sha256,
      CryptoHash::Sha384,
      CryptoHash::Sha512,
    ] {
      let expected = CROSS_HASH
        .iter()
        .find(|(curve, h, _)| *curve == v.curve && *h == hash);
      match expected {
        Some((_, _, None)) => {
          assert!(sign(v, hash).is_err());
          assert!(!verify(v, &public(v), hash, &hex(v.sig_p1363), b"sample"));
        }
        Some((_, _, Some(expected))) => {
          assert_eq!(sign(v, hash).unwrap(), hex(expected));
        }
        None => {
          let signature = sign(v, hash).unwrap();
          assert!(verify(v, &public(v), hash, &signature, b"sample"));
        }
      }
    }
  }
}

#[test]
fn ec_golden_ecdh_derive_bits() {
  for v in VECTORS {
    let secret = hex(v.ecdh);
    let peer = KeyData {
      r#type: KeyType::Public,
      data: hex(v.pub_b_uncompressed).into(),
    };
    let bits = crate::derive_bits_sync(
      KeyData::from(&private_key(v)),
      Some(peer),
      Algorithm::Ecdh,
      None,
      secret.len() * 8,
      None,
      Some(v.crypto_curve),
      None,
      None,
    )
    .unwrap();
    assert_eq!(bits, secret);
  }
}

#[test]
fn ec_golden_generated_pkcs8_shape() {
  for v in VECTORS {
    let generated = crate::generate_key::generate_ec(v.curve).unwrap();
    // The generated key is a PKCS#8 document this crate reads back...
    let key = RawKeyData::Private(generated.clone().into());
    let public = bytes_of(export(ExportKeyFormat::Raw, ecdsa(v), &key));
    assert_eq!(public.len(), hex(v.pub_uncompressed).len());
    // ...and re-encoding it through JWK reproduces it byte for byte, so it
    // has the same layout as the golden PKCS#8 above.
    let ExportKeyResult::JwkPrivateEc { x, y, d } =
      export(ExportKeyFormat::JwkPrivate, ecdsa(v), &key)
    else {
      panic!("expected a private JWK");
    };
    assert_eq!(
      private_bytes(import(
        v,
        crate::import_key::KeyData::JwkPrivateEc { x, y, d }
      )),
      generated
    );
  }
}
