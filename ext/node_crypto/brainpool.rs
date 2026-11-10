// Copyright 2018-2026 the Deno authors. MIT license.

//! The brainpool curves (RFC 5639) `node:crypto` serves: brainpoolP256r1,
//! brainpoolP384r1 and brainpoolP512r1, over AWS-LC's built-in brainpool
//! groups (`EC_group_brainpoolP*r1`). RustCrypto has no brainpoolP512r1, and
//! routing all three through one implementation gives `KeyObject`s, ECDH,
//! ECDSA and key generation the same behaviour on every brainpool curve.
//!
//! Keys are plain bytes: the private scalar as a big-endian integer of the
//! field size, and the public point in uncompressed SEC1 form.

use std::os::raw::c_uint;
use std::ptr;

use aws_lc_sys as ffi;
use aws_lc_sys::point_conversion_form_t;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrainpoolCurve {
  P256r1,
  P384r1,
  P512r1,
}

pub const BRAINPOOL_P256R1_OID: const_oid::ObjectIdentifier =
  const_oid::ObjectIdentifier::new_unwrap("1.3.36.3.3.2.8.1.1.7");
pub const BRAINPOOL_P384R1_OID: const_oid::ObjectIdentifier =
  const_oid::ObjectIdentifier::new_unwrap("1.3.36.3.3.2.8.1.1.11");
pub const BRAINPOOL_P512R1_OID: const_oid::ObjectIdentifier =
  const_oid::ObjectIdentifier::new_unwrap("1.3.36.3.3.2.8.1.1.13");

impl BrainpoolCurve {
  pub fn from_name(name: &str) -> Option<Self> {
    Some(match name {
      "brainpoolP256r1" => BrainpoolCurve::P256r1,
      "brainpoolP384r1" => BrainpoolCurve::P384r1,
      "brainpoolP512r1" => BrainpoolCurve::P512r1,
      _ => return None,
    })
  }

  pub fn from_oid(oid: &const_oid::ObjectIdentifier) -> Option<Self> {
    Some(match *oid {
      BRAINPOOL_P256R1_OID => BrainpoolCurve::P256r1,
      BRAINPOOL_P384R1_OID => BrainpoolCurve::P384r1,
      BRAINPOOL_P512R1_OID => BrainpoolCurve::P512r1,
      _ => return None,
    })
  }

  pub fn name(self) -> &'static str {
    match self {
      BrainpoolCurve::P256r1 => "brainpoolP256r1",
      BrainpoolCurve::P384r1 => "brainpoolP384r1",
      BrainpoolCurve::P512r1 => "brainpoolP512r1",
    }
  }

  pub fn oid(self) -> const_oid::ObjectIdentifier {
    match self {
      BrainpoolCurve::P256r1 => BRAINPOOL_P256R1_OID,
      BrainpoolCurve::P384r1 => BRAINPOOL_P384R1_OID,
      BrainpoolCurve::P512r1 => BRAINPOOL_P512R1_OID,
    }
  }

  /// Field (and scalar) size in bytes.
  pub fn field_len(self) -> usize {
    match self {
      BrainpoolCurve::P256r1 => 32,
      BrainpoolCurve::P384r1 => 48,
      BrainpoolCurve::P512r1 => 64,
    }
  }

  fn group(self) -> *const ffi::EC_GROUP {
    // SAFETY: the brainpool group constructors return static, lazily
    // initialised groups that are never freed.
    unsafe {
      match self {
        BrainpoolCurve::P256r1 => ffi::EC_group_brainpoolP256r1(),
        BrainpoolCurve::P384r1 => ffi::EC_group_brainpoolP384r1(),
        BrainpoolCurve::P512r1 => ffi::EC_group_brainpoolP512r1(),
      }
    }
  }
}

/// Owns an `EC_POINT`.
struct Point(*mut ffi::EC_POINT);
impl Drop for Point {
  fn drop(&mut self) {
    // SAFETY: the pointer came from EC_POINT_new and is freed once.
    unsafe { ffi::EC_POINT_free(self.0) }
  }
}

/// Owns a `BIGNUM`.
struct Bn(*mut ffi::BIGNUM);
impl Drop for Bn {
  fn drop(&mut self) {
    // SAFETY: the pointer came from BN_new / BN_bin2bn and is freed once.
    unsafe { ffi::BN_free(self.0) }
  }
}

/// Owns an `EC_KEY`.
struct Key(*mut ffi::EC_KEY);
impl Drop for Key {
  fn drop(&mut self) {
    // SAFETY: the pointer came from EC_KEY_new and is freed once.
    unsafe { ffi::EC_KEY_free(self.0) }
  }
}

fn new_point(curve: BrainpoolCurve) -> Option<Point> {
  // SAFETY: the group is valid; a null result is checked.
  let p = unsafe { ffi::EC_POINT_new(curve.group()) };
  (!p.is_null()).then_some(Point(p))
}

fn decode_point(curve: BrainpoolCurve, bytes: &[u8]) -> Option<Point> {
  let point = new_point(curve)?;
  // SAFETY: valid group, point and input slice. oct2point rejects encodings
  // that are not on the curve.
  let ok = unsafe {
    ffi::EC_POINT_oct2point(
      curve.group(),
      point.0,
      bytes.as_ptr(),
      bytes.len(),
      ptr::null_mut(),
    )
  };
  (ok == 1).then_some(point)
}

fn encode_point(
  curve: BrainpoolCurve,
  point: &Point,
  compressed: bool,
) -> Option<Vec<u8>> {
  let form = if compressed {
    point_conversion_form_t::POINT_CONVERSION_COMPRESSED
  } else {
    point_conversion_form_t::POINT_CONVERSION_UNCOMPRESSED
  };
  let len = if compressed {
    1 + curve.field_len()
  } else {
    1 + 2 * curve.field_len()
  };
  let mut out = vec![0u8; len];
  // SAFETY: valid group and point; `out` has room for the encoding.
  let written = unsafe {
    ffi::EC_POINT_point2oct(
      curve.group(),
      point.0,
      form,
      out.as_mut_ptr(),
      out.len(),
      ptr::null_mut(),
    )
  };
  (written == len).then_some(out)
}

/// A scalar in `[1, n-1]`, or `None`.
fn decode_scalar(curve: BrainpoolCurve, d: &[u8]) -> Option<Bn> {
  // SAFETY: valid input slice; a null result is checked.
  let bn = unsafe { ffi::BN_bin2bn(d.as_ptr(), d.len(), ptr::null_mut()) };
  if bn.is_null() {
    return None;
  }
  let bn = Bn(bn);
  // SAFETY: valid group and BIGNUMs.
  let in_range = unsafe {
    ffi::BN_is_zero(bn.0) == 0
      && ffi::BN_cmp(bn.0, ffi::EC_GROUP_get0_order(curve.group())) < 0
  };
  in_range.then_some(bn)
}

fn scalar_bytes(
  curve: BrainpoolCurve,
  bn: *const ffi::BIGNUM,
) -> Option<Vec<u8>> {
  let mut out = vec![0u8; curve.field_len()];
  // SAFETY: valid BIGNUM; `out` is the field size.
  let ok = unsafe { ffi::BN_bn2bin_padded(out.as_mut_ptr(), out.len(), bn) };
  (ok == 1).then_some(out)
}

#[derive(Clone, PartialEq, Eq)]
pub struct BrainpoolPrivateKey {
  pub curve: BrainpoolCurve,
  /// The private scalar, big-endian, `field_len` bytes.
  pub d: Vec<u8>,
  /// The public point, uncompressed SEC1.
  pub public: Vec<u8>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct BrainpoolPublicKey {
  pub curve: BrainpoolCurve,
  /// Uncompressed SEC1.
  pub point: Vec<u8>,
}

impl BrainpoolPrivateKey {
  pub fn from_scalar(curve: BrainpoolCurve, d: &[u8]) -> Option<Self> {
    if d.is_empty() || d.len() > curve.field_len() {
      return None;
    }
    let bn = decode_scalar(curve, d)?;
    let point = new_point(curve)?;
    // SAFETY: valid group, point and scalar.
    let ok = unsafe {
      ffi::EC_POINT_mul(
        curve.group(),
        point.0,
        bn.0,
        ptr::null(),
        ptr::null(),
        ptr::null_mut(),
      )
    };
    if ok != 1 {
      return None;
    }
    Some(BrainpoolPrivateKey {
      curve,
      d: scalar_bytes(curve, bn.0)?,
      public: encode_point(curve, &point, false)?,
    })
  }

  pub fn generate(curve: BrainpoolCurve) -> Option<Self> {
    // SAFETY: a fresh EC_KEY on a valid group; every result is checked.
    unsafe {
      let key = Key(ffi::EC_KEY_new());
      if key.0.is_null()
        || ffi::EC_KEY_set_group(key.0, curve.group()) != 1
        || ffi::EC_KEY_generate_key(key.0) != 1
      {
        return None;
      }
      let d = scalar_bytes(curve, ffi::EC_KEY_get0_private_key(key.0))?;
      Self::from_scalar(curve, &d)
    }
  }

  pub fn to_public_key(&self) -> BrainpoolPublicKey {
    BrainpoolPublicKey {
      curve: self.curve,
      point: self.public.clone(),
    }
  }

  fn ec_key(&self) -> Option<Key> {
    let bn = decode_scalar(self.curve, &self.d)?;
    let point = decode_point(self.curve, &self.public)?;
    // SAFETY: valid EC_KEY, group, scalar and point; results checked.
    unsafe {
      let key = Key(ffi::EC_KEY_new());
      if key.0.is_null()
        || ffi::EC_KEY_set_group(key.0, self.curve.group()) != 1
        || ffi::EC_KEY_set_private_key(key.0, bn.0) != 1
        || ffi::EC_KEY_set_public_key(key.0, point.0) != 1
      {
        return None;
      }
      Some(key)
    }
  }

  /// ECDH: the x coordinate of `d * peer`, `field_len` bytes.
  pub fn diffie_hellman(&self, peer: &[u8]) -> Option<Vec<u8>> {
    ecdh(self.curve, &self.d, peer)
  }

  /// ECDSA over a precomputed digest; a DER `ECDSA-Sig-Value`.
  pub fn sign_digest(&self, digest: &[u8]) -> Option<Vec<u8>> {
    let key = self.ec_key()?;
    // SAFETY: valid EC_KEY; the buffer is ECDSA_size bytes.
    unsafe {
      let mut sig = vec![0u8; ffi::ECDSA_size(key.0)];
      let mut sig_len: c_uint = 0;
      if ffi::ECDSA_sign(
        0,
        digest.as_ptr(),
        digest.len(),
        sig.as_mut_ptr(),
        &mut sig_len,
        key.0,
      ) != 1
      {
        return None;
      }
      sig.truncate(sig_len as usize);
      Some(sig)
    }
  }
}

impl BrainpoolPublicKey {
  /// Accepts compressed, uncompressed or hybrid SEC1 points on the curve.
  pub fn from_sec1(curve: BrainpoolCurve, bytes: &[u8]) -> Option<Self> {
    let point = decode_point(curve, bytes)?;
    Some(BrainpoolPublicKey {
      curve,
      point: encode_point(curve, &point, false)?,
    })
  }

  pub fn to_sec1(&self, compressed: bool) -> Vec<u8> {
    if !compressed {
      return self.point.clone();
    }
    decode_point(self.curve, &self.point)
      .and_then(|p| encode_point(self.curve, &p, true))
      .unwrap_or_else(|| self.point.clone())
  }

  /// ECDSA verification of a DER signature over a precomputed digest.
  pub fn verify_digest(&self, digest: &[u8], signature: &[u8]) -> bool {
    let Some(point) = decode_point(self.curve, &self.point) else {
      return false;
    };
    // SAFETY: valid EC_KEY, group and point; results checked.
    unsafe {
      let key = Key(ffi::EC_KEY_new());
      if key.0.is_null()
        || ffi::EC_KEY_set_group(key.0, self.curve.group()) != 1
        || ffi::EC_KEY_set_public_key(key.0, point.0) != 1
      {
        return false;
      }
      ffi::ECDSA_verify(
        0,
        digest.as_ptr(),
        digest.len(),
        signature.as_ptr(),
        signature.len(),
        key.0,
      ) == 1
    }
  }
}

/// ECDH on a brainpool curve: the x coordinate of `d * peer`.
pub fn ecdh(curve: BrainpoolCurve, d: &[u8], peer: &[u8]) -> Option<Vec<u8>> {
  let bn = decode_scalar(curve, d)?;
  let peer = decode_point(curve, peer)?;
  let out = new_point(curve)?;
  // SAFETY: valid group, points and scalar; results checked.
  unsafe {
    if ffi::EC_POINT_mul(
      curve.group(),
      out.0,
      ptr::null(),
      peer.0,
      bn.0,
      ptr::null_mut(),
    ) != 1
    {
      return None;
    }
    let x = Bn(ffi::BN_new());
    if x.0.is_null()
      || ffi::EC_POINT_get_affine_coordinates_GFp(
        curve.group(),
        out.0,
        x.0,
        ptr::null_mut(),
        ptr::null_mut(),
      ) != 1
    {
      return None;
    }
    scalar_bytes(curve, x.0)
  }
}

/// Re-encodes a SEC1 point (`ECDH.convertKey`).
pub fn convert_point(
  curve: BrainpoolCurve,
  bytes: &[u8],
  compressed: bool,
) -> Option<Vec<u8>> {
  let point = decode_point(curve, bytes)?;
  encode_point(curve, &point, compressed)
}

/// DER `ECDSA-Sig-Value` to IEEE P1363 (`r || s`, each `n` bytes).
pub fn der_signature_to_p1363(der: &[u8], n: usize) -> Option<Vec<u8>> {
  fn read(der: &[u8], tag: u8) -> Option<(&[u8], &[u8])> {
    if *der.first()? != tag {
      return None;
    }
    let first = *der.get(1)? as usize;
    let (len, start): (usize, usize) = if first < 0x80 {
      (first, 2)
    } else if first == 0x81 {
      (*der.get(2)? as usize, 3)
    } else if first == 0x82 {
      ((*der.get(2)? as usize) << 8 | *der.get(3)? as usize, 4)
    } else {
      return None;
    };
    let end = start.checked_add(len)?;
    Some((der.get(start..end)?, der.get(end..)?))
  }
  let (seq, rest) = read(der, 0x30)?;
  if !rest.is_empty() {
    return None;
  }
  let (r, seq) = read(seq, 0x02)?;
  let (s, seq) = read(seq, 0x02)?;
  if !seq.is_empty() {
    return None;
  }
  let mut out = vec![0u8; 2 * n];
  for (i, int) in [r, s].into_iter().enumerate() {
    let int = match int.iter().position(|&b| b != 0) {
      Some(p) => &int[p..],
      None => &[],
    };
    if int.len() > n {
      return None;
    }
    out[(i + 1) * n - int.len()..(i + 1) * n].copy_from_slice(int);
  }
  Some(out)
}

/// IEEE P1363 (`r || s`) to a DER `ECDSA-Sig-Value`.
pub fn p1363_signature_to_der(sig: &[u8], n: usize) -> Option<Vec<u8>> {
  if sig.len() != 2 * n {
    return None;
  }
  let mut body = Vec::with_capacity(2 * n + 8);
  for int in [&sig[..n], &sig[n..]] {
    let int = match int.iter().position(|&b| b != 0) {
      Some(p) => &int[p..],
      None => &int[n - 1..],
    };
    let pad = int[0] & 0x80 != 0;
    body.push(0x02);
    push_len(&mut body, int.len() + pad as usize);
    if pad {
      body.push(0);
    }
    body.extend_from_slice(int);
  }
  let mut out = Vec::with_capacity(body.len() + 4);
  out.push(0x30);
  push_len(&mut out, body.len());
  out.extend_from_slice(&body);
  Some(out)
}

fn push_len(out: &mut Vec<u8>, len: usize) {
  if len < 0x80 {
    out.push(len as u8);
  } else if len < 0x100 {
    out.extend_from_slice(&[0x81, len as u8]);
  } else {
    out.extend_from_slice(&[0x82, (len >> 8) as u8, len as u8]);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const CURVES: [BrainpoolCurve; 3] = [
    BrainpoolCurve::P256r1,
    BrainpoolCurve::P384r1,
    BrainpoolCurve::P512r1,
  ];

  #[test]
  fn ecdh_agrees() {
    for curve in CURVES {
      let a = BrainpoolPrivateKey::generate(curve).unwrap();
      let b = BrainpoolPrivateKey::generate(curve).unwrap();
      assert_eq!(a.public.len(), 1 + 2 * curve.field_len());
      let ab = a.diffie_hellman(&b.public).unwrap();
      let ba = b.diffie_hellman(&a.public).unwrap();
      assert_eq!(ab, ba);
      assert_eq!(ab.len(), curve.field_len());
      // A compressed peer point works too.
      let compressed = b.to_public_key().to_sec1(true);
      assert_eq!(compressed.len(), 1 + curve.field_len());
      assert_eq!(a.diffie_hellman(&compressed).unwrap(), ab);
    }
  }

  #[test]
  fn ecdsa_round_trip() {
    for curve in CURVES {
      let key = BrainpoolPrivateKey::generate(curve).unwrap();
      let digest = [7u8; 32];
      let sig = key.sign_digest(&digest).unwrap();
      let public = key.to_public_key();
      assert!(public.verify_digest(&digest, &sig));
      assert!(!public.verify_digest(&[8u8; 32], &sig));
      let n = curve.field_len();
      let p1363 = der_signature_to_p1363(&sig, n).unwrap();
      assert_eq!(p1363.len(), 2 * n);
      let der = p1363_signature_to_der(&p1363, n).unwrap();
      assert!(public.verify_digest(&digest, &der));
    }
  }

  #[test]
  fn rejects_bad_input() {
    for curve in CURVES {
      assert!(BrainpoolPrivateKey::from_scalar(curve, &[0]).is_none());
      assert!(
        BrainpoolPrivateKey::from_scalar(curve, &vec![0xff; curve.field_len()])
          .is_none()
      );
      let mut not_on_curve = vec![0u8; 1 + 2 * curve.field_len()];
      not_on_curve[0] = 4;
      not_on_curve[1] = 1;
      assert!(BrainpoolPublicKey::from_sec1(curve, &not_on_curve).is_none());
    }
  }

  #[test]
  fn p512_matches_node() {
    // Written by Node 26 (OpenSSL 3): createECDH("brainpoolP512r1").
    let a = hex(
      "9b2bcd16d447563999d49fbbd73e6c742b252dbea4db68f62e27e005fb191b9c\
       621565fb532bb75285c3b016eec54df931aa64ddb53a3becc175ffa78b710c93",
    );
    let a_pub = hex(
      "044b396241b3cf13828849279c89bd79488294cf69b93b703137c4b5a07c2ca5\
       51b26679786aa1c523067fd70178af791bf895cdac2ee36c4b844647695cb541\
       bc1705d887fc798b3f11abaf32a142c4c229d70e51784cff2ae94524609e0302\
       469a9a7d4fa93f5492af3379cdcbabd27b6373428f1ee9d5d103d345d68d64e3a1",
    );
    let b_pub_compressed = hex(
      "0291ea2bfd2eb2f319f993cb7d866b15809aa3e40b1c507eb850bf1b9f3ee574\
       12fc878b2c9a9d3bc366cf2b6134eda08062af06c8732c89b5c9a8dc42efff4b2a",
    );
    let secret = hex(
      "5c1a23ac90cdd2602b5d6a0931450e3d1b9e184fe830331f7c19bd6b2758cf93\
       fe86f68e91717c11e1cf3d36b935cc01d5d81d1bcfe75dced46d530e7aec35b3",
    );
    let key =
      BrainpoolPrivateKey::from_scalar(BrainpoolCurve::P512r1, &a).unwrap();
    assert_eq!(key.public, a_pub);
    assert_eq!(key.diffie_hellman(&b_pub_compressed).unwrap(), secret);
  }

  fn hex(s: &str) -> Vec<u8> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..s.len())
      .step_by(2)
      .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
      .collect()
  }
}
