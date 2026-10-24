// Copyright 2018-2026 the Deno authors. MIT license.

//! JSON Web Key (RFC 7518 §6.2) support for EC keys.
//!
//! elliptic-curve 0.14 dropped the `jwk` feature (`JwkEcKey`,
//! `to_jwk`/`from_jwk`) that this crate used with 0.13. This is a port of
//! that 0.13.8 implementation with the same wire behaviour: the same
//! serialization (member order `kty`, `crv`, `x`, `y`, then `d` when
//! present), the same deserialization rules (`kty` must be `"EC"`; `crv`,
//! `x`, `y` required; `d` optional; an unknown or duplicate member is an
//! error) and the same coordinate decoding (unpadded base64url into a
//! field-sized buffer) and key validation (a private key's `d` must match
//! its `x`/`y`).

use std::fmt;

use base64ct::Base64UrlUnpadded;
use base64ct::Encoding;
use elliptic_curve::AffinePoint;
use elliptic_curve::CurveArithmetic;
use elliptic_curve::FieldBytes;
use elliptic_curve::FieldBytesSize;
use elliptic_curve::PublicKey;
use elliptic_curve::SecretKey;
use elliptic_curve::sec1::Coordinates;
use elliptic_curve::sec1::FromSec1Point;
use elliptic_curve::sec1::ModulusSize;
use elliptic_curve::sec1::Sec1Point;
use elliptic_curve::sec1::ToSec1Point;
use elliptic_curve::sec1::ValidatePublicKey;
use serde::Deserialize;
use serde::Serialize;
use serde::de;
use serde::ser::SerializeStruct;
use zeroize::Zeroize;

/// The `kty` of an EC key.
const EC_KTY: &str = "EC";
/// Struct name used by the serde impls (as in elliptic-curve 0.13).
const JWK_TYPE_NAME: &str = "JwkEcKey";
/// The members a JWK EC key has.
const FIELDS: &[&str] = &["kty", "crv", "x", "y", "d"];

/// A curve with a JWK `crv` name.
pub trait JwkParameters: elliptic_curve::Curve {
  /// The `crv` member for this curve.
  const CRV: &'static str;
}

impl JwkParameters for p256::NistP256 {
  const CRV: &'static str = "P-256";
}

impl JwkParameters for p384::NistP384 {
  const CRV: &'static str = "P-384";
}

impl JwkParameters for p521::NistP521 {
  const CRV: &'static str = "P-521";
}

impl JwkParameters for k256::Secp256k1 {
  const CRV: &'static str = "secp256k1";
}

/// An EC key in JWK form. `d` is zeroized on drop.
#[derive(Clone)]
pub struct JwkEcKey {
  crv: String,
  x: String,
  y: String,
  d: Option<String>,
}

impl JwkEcKey {
  /// The `crv` member.
  pub fn crv(&self) -> &str {
    &self.crv
  }

  fn from_point<C>(point: &Sec1Point<C>) -> Option<Self>
  where
    C: JwkParameters,
    FieldBytesSize<C>: ModulusSize,
  {
    match point.coordinates() {
      Coordinates::Uncompressed { x, y } => Some(JwkEcKey {
        crv: C::CRV.to_owned(),
        x: Base64UrlUnpadded::encode_string(x),
        y: Base64UrlUnpadded::encode_string(y),
        d: None,
      }),
      _ => None,
    }
  }

  fn to_point<C>(&self) -> elliptic_curve::Result<Sec1Point<C>>
  where
    C: JwkParameters,
    FieldBytesSize<C>: ModulusSize,
  {
    if self.crv != C::CRV {
      return Err(elliptic_curve::Error);
    }
    let x = decode_base64url_fe::<C>(&self.x)?;
    let y = decode_base64url_fe::<C>(&self.y)?;
    Ok(Sec1Point::<C>::from_affine_coordinates(&x, &y, false))
  }
}

/// JWK of a public key (always the uncompressed coordinates).
pub fn public_key_to_jwk<C>(public_key: &PublicKey<C>) -> JwkEcKey
where
  C: CurveArithmetic + JwkParameters,
  AffinePoint<C>: FromSec1Point<C> + ToSec1Point<C>,
  FieldBytesSize<C>: ModulusSize,
{
  JwkEcKey::from_point::<C>(&public_key.to_sec1_point(false))
    .expect("JWK encoding error")
}

/// JWK of a private key: the public coordinates plus `d`.
pub fn secret_key_to_jwk<C>(secret_key: &SecretKey<C>) -> JwkEcKey
where
  C: CurveArithmetic + JwkParameters,
  AffinePoint<C>: FromSec1Point<C> + ToSec1Point<C>,
  FieldBytesSize<C>: ModulusSize,
{
  let mut jwk = public_key_to_jwk(&secret_key.public_key());
  let mut d = secret_key.to_bytes();
  jwk.d = Some(Base64UrlUnpadded::encode_string(&d));
  d.zeroize();
  jwk
}

/// The public key a JWK describes (`d`, if any, is ignored).
pub fn public_key_from_jwk<C>(
  jwk: &JwkEcKey,
) -> elliptic_curve::Result<PublicKey<C>>
where
  C: CurveArithmetic + JwkParameters,
  AffinePoint<C>: FromSec1Point<C> + ToSec1Point<C>,
  FieldBytesSize<C>: ModulusSize,
{
  PublicKey::from_sec1_bytes(jwk.to_point::<C>()?.as_bytes())
}

/// The private key a JWK describes; `d` must be present and match `x`/`y`.
pub fn secret_key_from_jwk<C>(
  jwk: &JwkEcKey,
) -> elliptic_curve::Result<SecretKey<C>>
where
  C: JwkParameters + ValidatePublicKey,
  FieldBytesSize<C>: ModulusSize,
{
  let Some(d_base64) = &jwk.d else {
    return Err(elliptic_curve::Error);
  };
  let public_key = jwk.to_point::<C>()?;
  let mut d_bytes = decode_base64url_fe::<C>(d_base64)?;
  let result = SecretKey::from_slice(&d_bytes);
  d_bytes.zeroize();
  let secret_key = result?;
  C::validate_public_key(&secret_key, &public_key)?;
  Ok(secret_key)
}

/// Decode an unpadded base64url field element into a field-sized buffer.
fn decode_base64url_fe<C: elliptic_curve::Curve>(
  s: &str,
) -> elliptic_curve::Result<FieldBytes<C>> {
  let mut result = FieldBytes::<C>::default();
  Base64UrlUnpadded::decode(s, &mut result)
    .map_err(|_| elliptic_curve::Error)?;
  Ok(result)
}

impl fmt::Debug for JwkEcKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let d = if self.d.is_some() {
      "Some(...)"
    } else {
      "None"
    };
    f.debug_struct(JWK_TYPE_NAME)
      .field("crv", &self.crv)
      .field("x", &self.x)
      .field("y", &self.y)
      .field("d", &d)
      .finish()
  }
}

impl Drop for JwkEcKey {
  fn drop(&mut self) {
    if let Some(d) = &mut self.d {
      d.zeroize();
    }
  }
}

impl Serialize for JwkEcKey {
  fn serialize<S: serde::Serializer>(
    &self,
    serializer: S,
  ) -> Result<S::Ok, S::Error> {
    let mut state = serializer.serialize_struct(JWK_TYPE_NAME, 5)?;
    for (i, field) in [EC_KTY, &self.crv, &self.x, &self.y].iter().enumerate() {
      state.serialize_field(FIELDS[i], field)?;
    }
    if let Some(d) = &self.d {
      state.serialize_field("d", d)?;
    }
    state.end()
  }
}

impl<'de> Deserialize<'de> for JwkEcKey {
  fn deserialize<D: de::Deserializer<'de>>(
    deserializer: D,
  ) -> Result<Self, D::Error> {
    enum Field {
      Kty,
      Crv,
      X,
      Y,
      D,
    }

    struct FieldVisitor;

    impl de::Visitor<'_> for FieldVisitor {
      type Value = Field;

      fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("field identifier")
      }

      fn visit_u64<E: de::Error>(self, value: u64) -> Result<Field, E> {
        match value {
          0 => Ok(Field::Kty),
          1 => Ok(Field::Crv),
          2 => Ok(Field::X),
          3 => Ok(Field::Y),
          4 => Ok(Field::D),
          _ => Err(de::Error::invalid_value(
            de::Unexpected::Unsigned(value),
            &"field index 0 <= i < 5",
          )),
        }
      }

      fn visit_str<E: de::Error>(self, value: &str) -> Result<Field, E> {
        self.visit_bytes(value.as_bytes())
      }

      fn visit_bytes<E: de::Error>(self, value: &[u8]) -> Result<Field, E> {
        match value {
          b"kty" => Ok(Field::Kty),
          b"crv" => Ok(Field::Crv),
          b"x" => Ok(Field::X),
          b"y" => Ok(Field::Y),
          b"d" => Ok(Field::D),
          _ => Err(de::Error::unknown_field(
            &String::from_utf8_lossy(value),
            FIELDS,
          )),
        }
      }
    }

    impl<'de> Deserialize<'de> for Field {
      fn deserialize<D: de::Deserializer<'de>>(
        deserializer: D,
      ) -> Result<Self, D::Error> {
        deserializer.deserialize_identifier(FieldVisitor)
      }
    }

    struct Visitor;

    impl<'de> de::Visitor<'de> for Visitor {
      type Value = JwkEcKey;

      fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("struct JwkEcKey")
      }

      fn visit_seq<A: de::SeqAccess<'de>>(
        self,
        mut seq: A,
      ) -> Result<JwkEcKey, A::Error> {
        const EXPECTED: &str = "struct JwkEcKey with 5 elements";
        let kty = seq
          .next_element::<String>()?
          .ok_or_else(|| de::Error::invalid_length(0, &EXPECTED))?;
        if kty != EC_KTY {
          return Err(de::Error::custom(format!(
            "unsupported JWK kty: {kty:?}"
          )));
        }
        let crv = seq
          .next_element::<String>()?
          .ok_or_else(|| de::Error::invalid_length(1, &EXPECTED))?;
        let x = seq
          .next_element::<String>()?
          .ok_or_else(|| de::Error::invalid_length(2, &EXPECTED))?;
        let y = seq
          .next_element::<String>()?
          .ok_or_else(|| de::Error::invalid_length(3, &EXPECTED))?;
        let d = seq
          .next_element::<Option<String>>()?
          .ok_or_else(|| de::Error::invalid_length(4, &EXPECTED))?;
        Ok(JwkEcKey { crv, x, y, d })
      }

      fn visit_map<A: de::MapAccess<'de>>(
        self,
        mut map: A,
      ) -> Result<JwkEcKey, A::Error> {
        let mut kty: Option<String> = None;
        let mut crv: Option<String> = None;
        let mut x: Option<String> = None;
        let mut y: Option<String> = None;
        let mut d: Option<String> = None;

        while let Some(key) = map.next_key::<Field>()? {
          match key {
            Field::Kty => {
              if kty.is_some() {
                return Err(de::Error::duplicate_field(FIELDS[0]));
              }
              kty = Some(map.next_value()?);
            }
            Field::Crv => {
              if crv.is_some() {
                return Err(de::Error::duplicate_field(FIELDS[1]));
              }
              crv = Some(map.next_value()?);
            }
            Field::X => {
              if x.is_some() {
                return Err(de::Error::duplicate_field(FIELDS[2]));
              }
              x = Some(map.next_value()?);
            }
            Field::Y => {
              if y.is_some() {
                return Err(de::Error::duplicate_field(FIELDS[3]));
              }
              y = Some(map.next_value()?);
            }
            Field::D => {
              if d.is_some() {
                return Err(de::Error::duplicate_field(FIELDS[4]));
              }
              d = map.next_value::<Option<String>>()?;
            }
          }
        }

        let kty = kty.ok_or_else(|| de::Error::missing_field("kty"))?;
        if kty != EC_KTY {
          return Err(de::Error::custom(format!("unsupported JWK kty: {kty}")));
        }
        let crv = crv.ok_or_else(|| de::Error::missing_field("crv"))?;
        let x = x.ok_or_else(|| de::Error::missing_field("x"))?;
        let y = y.ok_or_else(|| de::Error::missing_field("y"))?;
        Ok(JwkEcKey { crv, x, y, d })
      }
    }

    deserializer.deserialize_struct(JWK_TYPE_NAME, FIELDS, Visitor)
  }
}
