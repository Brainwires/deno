// Copyright 2018-2026 the Deno authors. MIT license.

//! Known-answer ("golden") tests for the `node:crypto` elliptic-curve paths.
//!
//! The expected values were produced independently of this crate, with
//! Python `cryptography` 49 on OpenSSL 3.6: RFC 6979 deterministic ECDSA
//! (the curve's own hash for both the message digest and the nonce, as the
//! RustCrypto `sign_prehash` does; secp256k1 normalized to low-S, as `k256`
//! does), ECDH, X9.62 points, SPKI, PKCS#8 and SEC1 DER. They pin the bytes
//! this crate emits across RustCrypto upgrades (elliptic-curve 0.13 -> 0.14,
//! RUSTSEC-2026-0387): an upgrade must not change any of them.

use data_encoding::BASE64URL_NOPAD;
use data_encoding::HEXLOWER;
use deno_core::serde_json;

use super::*;

struct Vector {
  /// The OpenSSL / `crypto.ECDH` curve name.
  curve: &'static str,
  /// The JWK `crv`, `None` where Node.js refuses JWK export (P-224).
  jwk_crv: Option<&'static str>,
  digest_type: &'static str,
  /// Private scalar of key A.
  d_a: &'static str,
  /// The curve's hash of the ASCII message `sample`.
  digest: &'static str,
  sig_der: &'static str,
  sig_p1363: &'static str,
  pub_uncompressed: &'static str,
  pub_compressed: &'static str,
  spki: &'static str,
  pkcs8: &'static str,
  sec1: &'static str,
  /// Public key of the ECDH peer B.
  pub_b_uncompressed: &'static str,
  pub_b_compressed: &'static str,
  /// x-coordinate of d_a * Q_b.
  ecdh: &'static str,
}

const VECTORS: &[Vector] = &[
  Vector {
    curve: "secp224r1",
    jwk_crv: None,
    digest_type: "sha224",
    d_a: "8caca1e206338231c1d23dc60409937336981ec6bb07d84d14d1d720",
    digest: "9003e374bc726550c2c289447fd0533160f875709386dfa377bfd41c",
    sig_der: concat!(
      "303d021d00e5b57971d0761ab7b2ea42ad9e3d5d3dc131318e5d81bf0be70913",
      "d7021c46844b53495c9f8c8c336530edd39086e0c63448404081fa8168f006",
    ),
    sig_p1363: concat!(
      "e5b57971d0761ab7b2ea42ad9e3d5d3dc131318e5d81bf0be70913d746844b53",
      "495c9f8c8c336530edd39086e0c63448404081fa8168f006",
    ),
    pub_uncompressed: concat!(
      "0465400a9fae2975933df5ba1a40c2c2ec3e592f48b4b927858edaca63b67061",
      "c27e8dc149f4d103c37fba9608d0924535221231f08df57b6d",
    ),
    pub_compressed: "0365400a9fae2975933df5ba1a40c2c2ec3e592f48b4b927858edaca63",
    spki: concat!(
      "304e301006072a8648ce3d020106052b81040021033a000465400a9fae297593",
      "3df5ba1a40c2c2ec3e592f48b4b927858edaca63b67061c27e8dc149f4d103c3",
      "7fba9608d0924535221231f08df57b6d",
    ),
    pkcs8: concat!(
      "3078020100301006072a8648ce3d020106052b810400210461305f020101041c",
      "8caca1e206338231c1d23dc60409937336981ec6bb07d84d14d1d720a13c033a",
      "000465400a9fae2975933df5ba1a40c2c2ec3e592f48b4b927858edaca63b670",
      "61c27e8dc149f4d103c37fba9608d0924535221231f08df57b6d",
    ),
    sec1: concat!(
      "3068020101041c8caca1e206338231c1d23dc60409937336981ec6bb07d84d14",
      "d1d720a00706052b81040021a13c033a000465400a9fae2975933df5ba1a40c2",
      "c2ec3e592f48b4b927858edaca63b67061c27e8dc149f4d103c37fba9608d092",
      "4535221231f08df57b6d",
    ),
    pub_b_uncompressed: concat!(
      "0481addf2c903ce2ce36a85d27397c2c9a8f58dae6b5024b11a566397bec1b83",
      "ea264c381da61004bdf35ead39b63be51651ab67ae0d95d675",
    ),
    pub_b_compressed: "0381addf2c903ce2ce36a85d27397c2c9a8f58dae6b5024b11a566397b",
    ecdh: "ff9a1588de8b57f36f55141146a79cd450eaf76ee040e2d6063945a6",
  },
  Vector {
    curve: "prime256v1",
    jwk_crv: Some("P-256"),
    digest_type: "sha256",
    d_a: "1cce01dc9a14044033a1818f2938edbb8ada4524f356daa858fdd9c16629735e",
    digest: "af2bdbe1aa9b6ec1e2ade1d694f41fc71a831d0268e9891562113d8a62add1bf",
    sig_der: concat!(
      "30460221009e439cedecc2cf66c4f1bb6a1de0a09acc58e4bc995a9f54a6e6b7",
      "9af643d1be022100e6a921e2b29627502bbe7a9a32798932b46f4893ca47aedc",
      "cba108bff5df5918",
    ),
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
    sec1: concat!(
      "307702010104201cce01dc9a14044033a1818f2938edbb8ada4524f356daa858",
      "fdd9c16629735ea00a06082a8648ce3d030107a14403420004b764d9e8b3f4e8",
      "f06b7f7720b83323c3b94d1c61cb182381e6a185584266a51583df7f1ae11522",
      "ac03a7d059ee4d03f7615532a3ec0159e8adf412f03275e859",
    ),
    pub_b_uncompressed: concat!(
      "04f20a286ed0767b04308a138fc4c27e33ae0e5fa713bef4a4bb23e99a531ee9",
      "4dd40ccfcdc804b074f1836a1ee8376066c34cdb555a8ef290304ca3ce943d43",
      "f2",
    ),
    pub_b_compressed: concat!(
      "02f20a286ed0767b04308a138fc4c27e33ae0e5fa713bef4a4bb23e99a531ee9",
      "4d",
    ),
    ecdh: "235dc63ea694d9d118aa2caca3740ea996e4f14721ff1a806f0f98c181fb4d89",
  },
  Vector {
    curve: "secp384r1",
    jwk_crv: Some("P-384"),
    digest_type: "sha384",
    d_a: concat!(
      "e07f77f5437eeb7882ff7cd061cd883e2ed5aec560892a1b7e9ff6fb9646b8c2",
      "1d509f89cf490388193710dfab700102",
    ),
    digest: concat!(
      "9a9083505bc92276aec4be312696ef7bf3bf603f4bbd381196a029f340585312",
      "313bca4a9b5b890efee42c77b1ee25fe",
    ),
    sig_der: concat!(
      "306402306e1c25dfa5f2c4e9b4aefe50c42991325e8a5e879b582285d32b778e",
      "d3379d5afed3d2035dac9994ffff97436a4aa2d8023048fa8c06bd82e4624019",
      "4e74146948d6680ae85d9107b48b54f7429bcf10851c354f052b32d0cca79df7",
      "6eb258ca8353",
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
    sec1: concat!(
      "3081a40201010430e07f77f5437eeb7882ff7cd061cd883e2ed5aec560892a1b",
      "7e9ff6fb9646b8c21d509f89cf490388193710dfab700102a00706052b810400",
      "22a16403620004e1ebd881ea3d71fa1749086ca505f67cddb847a8d9e8fb7328",
      "95a7bf50f5c33dc54f49ce6dc6587fdd45a107f58a7cd95940ac4de9aead1381",
      "5d9eb0aa5312cdb2374d7adac25fe8b7215896fd54c45f533f11d1442bc4fbdb",
      "2f426b7ea55dfe",
    ),
    pub_b_uncompressed: concat!(
      "04a327dc484bae04f9609e57f96f6cbce91e331ace56a86b5bd81e88e954b669",
      "70e1721749e232fb6a1ddac90111d8bfecb2aa3d9ef4c87261be6e17616686f6",
      "d99d03f05bcc9f72fdae505dbf6e5863d2803d0514737ccded38b4b7fa972b58",
      "02",
    ),
    pub_b_compressed: concat!(
      "02a327dc484bae04f9609e57f96f6cbce91e331ace56a86b5bd81e88e954b669",
      "70e1721749e232fb6a1ddac90111d8bfec",
    ),
    ecdh: concat!(
      "f4cceff3386474012f4f1b3bb899badb9491e7a47b5d0e7d3c335a251c71ad86",
      "87de9113dbbe45d6acc99eef4887e158",
    ),
  },
  Vector {
    curve: "secp521r1",
    jwk_crv: Some("P-521"),
    digest_type: "sha512",
    d_a: concat!(
      "0185095656d36b3d8816460cfbebab7b371cf4eb0f7716e446393c571b1acba1",
      "46c1001f5b33e37fb93f7b9a13de2372b42b1b387f8c1d1c88d2a7ca337aa94b",
      "6664",
    ),
    digest: concat!(
      "39a5e04aaff7455d9850c605364f514c11324ce64016960d23d5dc57d3ffd8f4",
      "9a739468ab8049bf18eef820cdb1ad6c9015f838556bc7fad4138b23fdf986c7",
    ),
    sig_der: concat!(
      "30818702420144ec2f7fd9482eee38abd20016afc8ac308e7d186d6bca5a395a",
      "1b043301c988fd43f2297d8a810e1c65667b7f627a4423d65d375840b10071b7",
      "df7ba438c1241202416ca7c9ef52b5e137b52081cb84fd49a0902df5424ef782",
      "8a8f1c7911ad17151a58cdc1d7994ae3e7c343efa17d5136f377dd6808cb9610",
      "da4ccf5d3e3a81832bbb",
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
    sec1: concat!(
      "3081dc02010104420185095656d36b3d8816460cfbebab7b371cf4eb0f7716e4",
      "46393c571b1acba146c1001f5b33e37fb93f7b9a13de2372b42b1b387f8c1d1c",
      "88d2a7ca337aa94b6664a00706052b81040023a181890381860004001a546e3b",
      "51bc197a4d7c0d990db501703bf2d6fc89da8d76389497d50f5e540681d693ba",
      "c8ce46e9c53637cbd0897ced79e6ef83af9f16a395853462dd9d93669c016d81",
      "99f03811407498ac943d5f105450ba0568e87d4739b56a92e1db15b3a8ef1de3",
      "7cebb369061b9feead14b60d610e936a4b2143add4e361b89ba8d63f03c82b",
    ),
    pub_b_uncompressed: concat!(
      "0400fd25411362929a956a59a1ad6bf5c1810405aa01eeba974510a5e63ab75d",
      "339b242ef9c18101dc0c848f3f1a6179ffbc981aa1b83139213609afb702de9c",
      "6d2ae5009ad8e5e3acb30befccb039085c344f8a6109dbcd0f7b9da2a6626581",
      "8e1e225f1ced237b8c3846bfdf514dcc81e022dad8baf856eda2917304d7178a",
      "44a82a2070",
    ),
    pub_b_compressed: concat!(
      "0200fd25411362929a956a59a1ad6bf5c1810405aa01eeba974510a5e63ab75d",
      "339b242ef9c18101dc0c848f3f1a6179ffbc981aa1b83139213609afb702de9c",
      "6d2ae5",
    ),
    ecdh: concat!(
      "0031fb37315737b004cc56d8cdc84537c53465a4a83a6f4810e0dfbbe6ae5d62",
      "5e5df322632f1d28782e52a8855e318d8dc4bfd9c7cd0f0389f47dec3bdfa3d3",
      "d1a8",
    ),
  },
  Vector {
    curve: "secp256k1",
    jwk_crv: Some("secp256k1"),
    digest_type: "sha256",
    d_a: "ed8ec35495cc2ab16535a35db0e7744e124aa673d3fded5c2f4ef3fa0d956402",
    digest: "af2bdbe1aa9b6ec1e2ade1d694f41fc71a831d0268e9891562113d8a62add1bf",
    sig_der: concat!(
      "3044022014d7c9d30e8e5f709d73620061e84cd105fbe7844cbbd2753373699f",
      "c3dd9f09022056cd78e0575f1810e0e72532a8d77e43952590eada1b39b90dca",
      "ecc499944b84",
    ),
    sig_p1363: concat!(
      "14d7c9d30e8e5f709d73620061e84cd105fbe7844cbbd2753373699fc3dd9f09",
      "56cd78e0575f1810e0e72532a8d77e43952590eada1b39b90dcaecc499944b84",
    ),
    pub_uncompressed: concat!(
      "0471de1db6b57f44add9b53c0b7eb0f8ff8cbad7728485896cdb020370783475",
      "e686d7ce77af11763bf5e32aac53a3594feddbabdc384a4ea6464057b5f591ac",
      "7a",
    ),
    pub_compressed: concat!(
      "0271de1db6b57f44add9b53c0b7eb0f8ff8cbad7728485896cdb020370783475",
      "e6",
    ),
    spki: concat!(
      "3056301006072a8648ce3d020106052b8104000a0342000471de1db6b57f44ad",
      "d9b53c0b7eb0f8ff8cbad7728485896cdb020370783475e686d7ce77af11763b",
      "f5e32aac53a3594feddbabdc384a4ea6464057b5f591ac7a",
    ),
    pkcs8: concat!(
      "308184020100301006072a8648ce3d020106052b8104000a046d306b02010104",
      "20ed8ec35495cc2ab16535a35db0e7744e124aa673d3fded5c2f4ef3fa0d9564",
      "02a1440342000471de1db6b57f44add9b53c0b7eb0f8ff8cbad7728485896cdb",
      "020370783475e686d7ce77af11763bf5e32aac53a3594feddbabdc384a4ea646",
      "4057b5f591ac7a",
    ),
    sec1: concat!(
      "30740201010420ed8ec35495cc2ab16535a35db0e7744e124aa673d3fded5c2f",
      "4ef3fa0d956402a00706052b8104000aa1440342000471de1db6b57f44add9b5",
      "3c0b7eb0f8ff8cbad7728485896cdb020370783475e686d7ce77af11763bf5e3",
      "2aac53a3594feddbabdc384a4ea6464057b5f591ac7a",
    ),
    pub_b_uncompressed: concat!(
      "0494d1cabbb83e42acc957b9f07d29207c649b7124e018d6bd2752d46b9a303c",
      "fd59fcb269b4122beac2a52ae7e9e1cf9d31d877f6d52275cc7b0bfbb74c2c67",
      "f0",
    ),
    pub_b_compressed: concat!(
      "0294d1cabbb83e42acc957b9f07d29207c649b7124e018d6bd2752d46b9a303c",
      "fd",
    ),
    ecdh: "c5cf70fed981dfd393d8a82ed07faee9d0e724bc81eee70b9e048db8e8da2f47",
  },
];

fn hex(s: &str) -> Vec<u8> {
  HEXLOWER.decode(s.as_bytes()).unwrap()
}

fn b64(bytes: &[u8]) -> String {
  BASE64URL_NOPAD.encode(bytes)
}

fn private_handle(v: &Vector) -> KeyObjectHandle {
  KeyObjectHandle::new_raw_asymmetric_key(
    "ec",
    Some(v.curve),
    &hex(v.d_a),
    false,
  )
  .unwrap()
}

fn public_handle(v: &Vector) -> KeyObjectHandle {
  KeyObjectHandle::AsymmetricPublic(
    private_handle(v).as_private_key().unwrap().to_public_key(),
  )
}

/// `(x, y)` of key A, base64url (the JWK coordinate encoding).
fn jwk_xy(v: &Vector) -> (String, String) {
  let point = hex(v.pub_uncompressed);
  let len = (point.len() - 1) / 2;
  (b64(&point[1..1 + len]), b64(&point[1 + len..]))
}

#[test]
fn ec_golden_der_and_raw_export() {
  for v in VECTORS {
    let private = private_handle(v);
    let private_key = private.as_private_key().unwrap();
    assert_eq!(
      &*private_key.export_der("pkcs8").unwrap(),
      hex(v.pkcs8),
      "{} pkcs8",
      v.curve
    );
    assert_eq!(
      &*private_key.export_der("sec1").unwrap(),
      hex(v.sec1),
      "{} sec1",
      v.curve
    );
    assert_eq!(
      &*private.export_private_key_raw().unwrap(),
      hex(v.d_a),
      "{} raw private",
      v.curve
    );

    let public = public_handle(v);
    let public_key = public.as_public_key().unwrap();
    assert_eq!(
      &*public_key.export_der("spki").unwrap(),
      hex(v.spki),
      "{} spki",
      v.curve
    );
    assert_eq!(
      &*public.export_public_key_raw(false).unwrap(),
      hex(v.pub_uncompressed),
      "{} uncompressed point",
      v.curve
    );
    assert_eq!(
      &*public.export_public_key_raw(true).unwrap(),
      hex(v.pub_compressed),
      "{} compressed point",
      v.curve
    );
    // The public key derived from the private handle is the same key.
    assert!(
      KeyObjectHandle::AsymmetricPublic(private_key.to_public_key()) == public
    );
  }
}

#[test]
fn ec_golden_jwk_export() {
  for v in VECTORS {
    let private = private_handle(v);
    let private_key = ec_private(&private);
    let public_key = private_key.to_public_key();
    let Some(crv) = v.jwk_crv else {
      assert!(private_key.to_jwk().is_err(), "{} private jwk", v.curve);
      assert!(public_key.to_jwk().is_err(), "{} public jwk", v.curve);
      continue;
    };
    let (x, y) = jwk_xy(v);
    let d = b64(&hex(v.d_a));
    // Byte-exact serialization, member order included.
    assert_eq!(
      serde_json::to_string(&private_key.to_jwk().unwrap()).unwrap(),
      format!(r#"{{"kty":"EC","crv":"{crv}","x":"{x}","y":"{y}","d":"{d}"}}"#),
      "{} private jwk",
      v.curve
    );
    assert_eq!(
      serde_json::to_string(&public_key.to_jwk().unwrap()).unwrap(),
      format!(r#"{{"kty":"EC","crv":"{crv}","x":"{x}","y":"{y}"}}"#),
      "{} public jwk",
      v.curve
    );
  }
}

fn ec_private(handle: &KeyObjectHandle) -> &EcPrivateKey {
  match handle.as_private_key() {
    Some(AsymmetricPrivateKey::Ec(key)) => key,
    _ => panic!("not an EC private key"),
  }
}

#[test]
fn ec_golden_import() {
  for v in VECTORS {
    let private = private_handle(v);
    let public = public_handle(v);

    let from_pkcs8 = KeyObjectHandle::new_asymmetric_private_key_from_js(
      &hex(v.pkcs8),
      "der",
      "pkcs8",
      None,
    )
    .unwrap();
    assert!(from_pkcs8 == private, "{} pkcs8 import", v.curve);

    let from_sec1 = KeyObjectHandle::new_asymmetric_private_key_from_js(
      &hex(v.sec1),
      "der",
      "sec1",
      None,
    )
    .unwrap();
    assert!(from_sec1 == private, "{} sec1 import", v.curve);

    let pem = der_to_pem("EC PRIVATE KEY", &hex(v.sec1));
    let from_pem = KeyObjectHandle::new_asymmetric_private_key_from_js(
      pem.as_bytes(),
      "pem",
      "",
      None,
    )
    .unwrap();
    assert!(from_pem == private, "{} sec1 pem import", v.curve);

    let from_spki = KeyObjectHandle::new_asymmetric_public_key_from_js(
      &hex(v.spki),
      "der",
      "spki",
      None,
    )
    .unwrap();
    assert!(from_spki == public, "{} spki import", v.curve);

    for point in [v.pub_uncompressed, v.pub_compressed] {
      let from_raw = KeyObjectHandle::new_raw_asymmetric_key(
        "ec",
        Some(v.curve),
        &hex(point),
        true,
      )
      .unwrap();
      assert!(from_raw == public, "{} raw point import", v.curve);
    }

    let Some(crv) = v.jwk_crv else {
      continue;
    };
    let (x, y) = jwk_xy(v);
    let d = b64(&hex(v.d_a));
    let jwk: JwkEcKey = serde_json::from_str(&format!(
      r#"{{"kty":"EC","crv":"{crv}","x":"{x}","y":"{y}","d":"{d}"}}"#
    ))
    .unwrap();
    assert!(
      KeyObjectHandle::new_ec_jwk(&jwk, false).unwrap() == private,
      "{} private jwk import",
      v.curve
    );
    let jwk: JwkEcKey = serde_json::from_str(&format!(
      r#"{{"kty":"EC","crv":"{crv}","x":"{x}","y":"{y}"}}"#
    ))
    .unwrap();
    assert!(
      KeyObjectHandle::new_ec_jwk(&jwk, true).unwrap() == public,
      "{} public jwk import",
      v.curve
    );

    // A `d` that does not belong to (x, y) is refused.
    let mut wrong_d = hex(v.d_a);
    *wrong_d.last_mut().unwrap() ^= 1;
    let jwk: JwkEcKey = serde_json::from_str(&format!(
      r#"{{"kty":"EC","crv":"{crv}","x":"{x}","y":"{y}","d":"{}"}}"#,
      b64(&wrong_d)
    ))
    .unwrap();
    assert!(KeyObjectHandle::new_ec_jwk(&jwk, false).is_err());
    // As are a foreign `kty` and members a JWK EC key does not have.
    assert!(
      serde_json::from_str::<JwkEcKey>(&format!(
        r#"{{"kty":"RSA","crv":"{crv}","x":"{x}","y":"{y}"}}"#
      ))
      .is_err()
    );
    assert!(
      serde_json::from_str::<JwkEcKey>(&format!(
        r#"{{"kty":"EC","crv":"{crv}","x":"{x}","y":"{y}","alg":"ES256"}}"#
      ))
      .is_err()
    );
  }
}

/// Before the migration P-521 signed with a random nonce (`p521` 0.13's
/// `sign_prehash` used `OsRng`), so its signatures are checked by
/// verification only; every other curve is RFC 6979 and pinned to the byte.
fn is_randomized(v: &Vector) -> bool {
  v.curve == "secp521r1"
}

#[test]
fn ec_golden_ecdsa_rfc6979() {
  for v in VECTORS {
    let private = private_handle(v);
    let public = public_handle(v);
    let digest = hex(v.digest);
    for (encoding, expected) in [(0, v.sig_der), (1, v.sig_p1363)] {
      let signature = private
        .sign_prehashed(v.digest_type, &digest, None, None, encoding)
        .unwrap();
      if !is_randomized(v) {
        assert_eq!(
          &*signature,
          hex(expected),
          "{} signature (encoding {encoding})",
          v.curve
        );
      }
      for signature in [&*signature, &hex(expected)] {
        assert!(
          public
            .verify_prehashed(
              v.digest_type,
              &digest,
              signature,
              None,
              None,
              encoding
            )
            .unwrap(),
          "{} verify (encoding {encoding})",
          v.curve
        );
      }
      let mut tampered = digest.clone();
      tampered[0] ^= 1;
      assert!(
        !public
          .verify_prehashed(
            v.digest_type,
            &tampered,
            &signature,
            None,
            None,
            encoding
          )
          .unwrap(),
        "{} verify tampered (encoding {encoding})",
        v.curve
      );
    }

    // A DER signature whose `r` carries a redundant leading zero is not DER
    // and does not verify.
    let der = hex(v.sig_der);
    if der[1] < 0x7f {
      let r_len = der[3] as usize;
      let mut bad = vec![0x30, der[1] + 1, 0x02, (r_len + 1) as u8, 0x00];
      bad.extend_from_slice(&der[4..]);
      assert!(
        !public
          .verify_prehashed(v.digest_type, &hex(v.digest), &bad, None, None, 0)
          .unwrap(),
        "{} non-minimal DER",
        v.curve
      );
    }
  }
}

/// Signatures (IEEE P1363) of the curve's hash of `sample` under every
/// digest Node.js accepts, as this crate produced them before the
/// elliptic-curve 0.14 migration. `None`: refused (the digest is shorter
/// than half the field). P-521 is absent: its signatures were randomized.
const CROSS_HASH: &[(&str, &str, Option<&str>)] = &[
  (
    "secp224r1",
    "md5",
    Some(concat!(
      "43a315d080f4fb5ea823b4561a94734e068e83eb2db6edfa6628f72d086d4359",
      "b1b5a2df9a7a19df03d00f132c73cd7a66d3a6bf77c67433"
    )),
  ),
  (
    "secp224r1",
    "sha1",
    Some(concat!(
      "b7c53dc5a000f69cad261d40d404fb24f43d6b43c4fa860e8fbb82a3ff342106",
      "83fe917a3f8046536126f74b05621b7da8861b311f179dea"
    )),
  ),
  (
    "secp224r1",
    "sha224",
    Some(concat!(
      "e5b57971d0761ab7b2ea42ad9e3d5d3dc131318e5d81bf0be70913d746844b53",
      "495c9f8c8c336530edd39086e0c63448404081fa8168f006"
    )),
  ),
  (
    "secp224r1",
    "sha256",
    Some(concat!(
      "52ee4c5cbd0332cf0d2b5179874a181dd567da9b6e64d62e978cd445a258d3be",
      "cb91688098c25adb2133eec30ac9ccc6578e90b934a4fd08"
    )),
  ),
  (
    "secp224r1",
    "sha384",
    Some(concat!(
      "168e23a0397fe211c96520a8c8d1cd561f8785a78936e3f27ffad34e533b4182",
      "936306ea23f5fa6790f7b4e22df5fbeb714e56cbc12ca480"
    )),
  ),
  (
    "secp224r1",
    "sha512",
    Some(concat!(
      "df9d64139ed078ce8d5c9fc2e7963a2576c557cac464bf2116613d6db0d28024",
      "5af8966b7d2756a856e33b3c50ffc95a3c43ad2f386d8eb7"
    )),
  ),
  (
    "prime256v1",
    "md5",
    Some(concat!(
      "94f7839a00b4ccf3ae4550dea7f97c6eff66b19125a5c567264f9841411849dd",
      "c9f65c123bf5b75900539a605ab6203f8f376308cc54900291511f45472f48f7"
    )),
  ),
  (
    "prime256v1",
    "sha1",
    Some(concat!(
      "7dda5b7c6a3c71d7ab301038f21564e5cf84f4e5f45411060ec9d168922ca810",
      "3fc8ca3e25af2fc3425cbd1fd9a1c7756da6fdff36fb5c4b628cdcd85994f339"
    )),
  ),
  (
    "prime256v1",
    "sha224",
    Some(concat!(
      "7ab76647cbeb79c793a2ae19e638b9d71db8fcbe4a47b94165880b15697764b2",
      "e2f6f17aa4e6c5443cfb360c802698453ed4828a47d226f930d610619b5be55f"
    )),
  ),
  (
    "prime256v1",
    "sha256",
    Some(concat!(
      "9e439cedecc2cf66c4f1bb6a1de0a09acc58e4bc995a9f54a6e6b79af643d1be",
      "e6a921e2b29627502bbe7a9a32798932b46f4893ca47aedccba108bff5df5918"
    )),
  ),
  (
    "prime256v1",
    "sha384",
    Some(concat!(
      "0f93da613a9e4103bd4fb10ed1f29c02aa64e9837104917ae6ff9ba8203614d9",
      "6d29fe78a45955d1450c8fa1ec5f8137459cedd40e5529ba07286a6f14ce1ad2"
    )),
  ),
  (
    "prime256v1",
    "sha512",
    Some(concat!(
      "24787ec52475034407175eb6d1e235cf055a20e708a650f839b09dfc192e32ec",
      "35024c8ac29b88f5a22637d32444ce0ff685dab12876cf72bacd6ede1af3382d"
    )),
  ),
  ("secp384r1", "md5", None),
  ("secp384r1", "sha1", None),
  (
    "secp384r1",
    "sha224",
    Some(concat!(
      "e9ce6a4472a43fa0b21a76351a78d243b92a6f30286dd3f8dfe854646da4dd64",
      "b5e2d1245b9d5df4754daa0bcce9c5e6a1801968c5cf4b5b9b768fda28545b29",
      "4a6ff13fe874c748d0091dd8beec13598986ace80e5890e1254ade1dadccbb01"
    )),
  ),
  (
    "secp384r1",
    "sha256",
    Some(concat!(
      "b8a05c71699365d6f1f7021b3e93b3a256f4f8bd5d0d48beba41b103c772baa3",
      "ac70673d4e2f3cffbae229dc43f9db9f0e5a563c17a4a1c4d9510cb2c815c5d9",
      "b812b8184ab9924472b419d53a96025eda7982d55d8fb9bca83a191b9f4509a2"
    )),
  ),
  (
    "secp384r1",
    "sha384",
    Some(concat!(
      "6e1c25dfa5f2c4e9b4aefe50c42991325e8a5e879b582285d32b778ed3379d5a",
      "fed3d2035dac9994ffff97436a4aa2d848fa8c06bd82e46240194e74146948d6",
      "680ae85d9107b48b54f7429bcf10851c354f052b32d0cca79df76eb258ca8353"
    )),
  ),
  (
    "secp384r1",
    "sha512",
    Some(concat!(
      "9badf52378de0a58941a3a65c0608f4dc4bd56adcafae86cbd2377564cc8179c",
      "acb0acee59fe1cdf12d2eda772ccb3871bb586da75b507cee01145818ba915d6",
      "02562beef5cb8b6dc16b7caaa51438fc2ff2aeb53314c1444651093ea457f4f8"
    )),
  ),
  ("secp521r1", "md5", None),
  ("secp521r1", "sha1", None),
  ("secp521r1", "sha224", None),
  ("secp521r1", "sha256", None),
  (
    "secp256k1",
    "md5",
    Some(concat!(
      "37066da52e019ee0a50060f1176d4f05ab999b73a100b5034e6c9ea57a227da8",
      "0b786ce9a36da92b60bcd00f09464a1b60ad8cb54a441c5bef892c19c9402c53"
    )),
  ),
  (
    "secp256k1",
    "sha1",
    Some(concat!(
      "f4d8442db32f1020a61a1836950b9495f335db08eea05f349c4fc26c2f8ec2c5",
      "1634c5c0938373239b523de6962ef23aaebb8aec5ea3e78b683a245e70b4f9e2"
    )),
  ),
  (
    "secp256k1",
    "sha224",
    Some(concat!(
      "1442bf8fdb26410227e94eaeab16538b1c458ec9081f92223eb1153f05f0e984",
      "41796ab8fa5f71022374ea0e48100906baa52e924c1674dabc7be545bcc2a76d"
    )),
  ),
  (
    "secp256k1",
    "sha256",
    Some(concat!(
      "14d7c9d30e8e5f709d73620061e84cd105fbe7844cbbd2753373699fc3dd9f09",
      "56cd78e0575f1810e0e72532a8d77e43952590eada1b39b90dcaecc499944b84"
    )),
  ),
  (
    "secp256k1",
    "sha384",
    Some(concat!(
      "f7e65da9394b214261da69c395d1745483e22020d35ceb653d6baf291abd7756",
      "38f7875bb7e25edd6b01bd2387577fb5600bacc36d23710d794e95f46736c078"
    )),
  ),
  (
    "secp256k1",
    "sha512",
    Some(concat!(
      "2f5e25d276e6e1461815c6f353649358672075094cba291ad8bd87804b6d98c0",
      "7ba8044b1904041cae248913ab9c59892d5a6fcb13cd41f64d1746c24a3e0370"
    )),
  ),
];

fn sample_digest(name: &str) -> Vec<u8> {
  use digest::Digest as _;
  match name {
    "md5" => md5::Md5::digest(b"sample").to_vec(),
    "sha1" => sha1::Sha1::digest(b"sample").to_vec(),
    "sha224" => sha2::Sha224::digest(b"sample").to_vec(),
    "sha256" => sha2::Sha256::digest(b"sample").to_vec(),
    "sha384" => sha2::Sha384::digest(b"sample").to_vec(),
    "sha512" => sha2::Sha512::digest(b"sample").to_vec(),
    _ => unreachable!(),
  }
}

/// Every digest with every curve: the same signatures, and the same
/// refusals for a digest shorter than half the field (ecdsa 0.17 would
/// accept those; the crate keeps refusing them).
#[test]
fn ec_golden_ecdsa_cross_hash() {
  for v in VECTORS {
    let private = private_handle(v);
    let public = public_handle(v);
    for name in ["md5", "sha1", "sha224", "sha256", "sha384", "sha512"] {
      let digest = sample_digest(name);
      let expected = CROSS_HASH
        .iter()
        .find(|(curve, digest, _)| *curve == v.curve && *digest == name);
      let signature = private.sign_prehashed(name, &digest, None, None, 1);
      match expected {
        Some((_, _, None)) => {
          assert!(
            matches!(
              signature,
              Err(crate::sign::KeyObjectHandlePrehashedSignAndVerifyError::FailedToSignDigest)
            ),
            "{} {name} must be refused",
            v.curve
          );
          // ...and a signature over it never verifies.
          assert!(
            !public
              .verify_prehashed(name, &digest, &hex(v.sig_p1363), None, None, 1)
              .unwrap()
          );
        }
        Some((_, _, Some(expected))) => {
          assert_eq!(&*signature.unwrap(), hex(expected), "{} {name}", v.curve);
        }
        // P-521 above half the field: randomized before, verify only.
        None => {
          let signature = signature.unwrap();
          assert!(
            public
              .verify_prehashed(name, &digest, &signature, None, None, 1)
              .unwrap(),
            "{} {name} verify",
            v.curve
          );
        }
      }
    }
  }
}

/// RFC 6979, A.2.5: P-256, SHA-256, message "sample".
#[test]
fn ec_golden_rfc6979_p256_published_vector() {
  let private = KeyObjectHandle::new_raw_asymmetric_key(
    "ec",
    Some("prime256v1"),
    &hex("c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721"),
    false,
  )
  .unwrap();
  let digest =
    hex("af2bdbe1aa9b6ec1e2ade1d694f41fc71a831d0268e9891562113d8a62add1bf");
  let signature = private
    .sign_prehashed("sha256", &digest, None, None, 1)
    .unwrap();
  assert_eq!(
    &*signature,
    hex(concat!(
      "efd48b2aacb6a8fd1140dd9cd45e81d69d2c877b56aaf991c34d0ea84eaf3716",
      "f7cb1c942d657c41d436c7a1b6e29f65f3e900dbb9aff4064dc4ab2f843acda8",
    ))
  );
}

#[test]
fn ec_golden_ecdh() {
  for v in VECTORS {
    let d_a = hex(v.d_a);
    let pub_a = hex(v.pub_uncompressed);

    let mut computed = vec![0; pub_a.len()];
    crate::ecdh_compute_public_key(v.curve, &d_a, &mut computed).unwrap();
    assert_eq!(computed, pub_a, "{} ecdh public key", v.curve);

    assert_eq!(
      &*crate::ecdh_encode_pubkey(v.curve, &pub_a, true).unwrap(),
      hex(v.pub_compressed),
      "{} ecdh encode compressed",
      v.curve
    );
    assert_eq!(
      &*crate::ecdh_encode_pubkey(v.curve, &hex(v.pub_compressed), false)
        .unwrap(),
      pub_a,
      "{} ecdh encode uncompressed",
      v.curve
    );

    for their_pub in [v.pub_b_uncompressed, v.pub_b_compressed] {
      let mut secret = vec![0; hex(v.ecdh).len()];
      crate::ecdh_compute_secret(
        v.curve,
        &d_a,
        Some(&pub_a),
        &hex(their_pub),
        &mut secret,
      )
      .unwrap();
      assert_eq!(secret, hex(v.ecdh), "{} ecdh secret", v.curve);
    }

    // A stored public key that is not d_a's is refused.
    let mut secret = vec![0; hex(v.ecdh).len()];
    assert!(matches!(
      crate::ecdh_compute_secret(
        v.curve,
        &d_a,
        Some(&hex(v.pub_b_uncompressed)),
        &hex(v.pub_b_uncompressed),
        &mut secret,
      ),
      Err(crate::EcdhError::InvalidKeyPair)
    ));
  }
}
