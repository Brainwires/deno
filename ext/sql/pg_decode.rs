// Copyright 2018-2026 the Deno authors. MIT license.

//! Postgres binary-format values to JavaScript values, built directly in V8
//! (no intermediate serde tree). The text helpers here reproduce the server's
//! own text output for the types that have no natural JavaScript value.

use deno_core::v8;
use postgres_types::Kind;
use postgres_types::Type;

pub type DecodeResult<T> = Result<T, String>;

#[derive(Clone, Copy, Debug, Default)]
pub struct DecodeOptions {
  /// int8 (and xid8) as `bigint` instead of a decimal string.
  pub bigint: bool,
}

/// Microseconds between 1970-01-01 and Postgres' epoch, 2000-01-01.
const PG_EPOCH_MS: i64 = 946_684_800_000;
const PG_EPOCH_DAYS: i64 = 10_957;
/// The first OID a user-defined object (an extension's type) can get.
const FIRST_NORMAL_OBJECT_ID: u32 = 16_384;
const MAX_NESTING: u32 = 32;

mod oid {
  pub const BOOL: u32 = 16;
  pub const BYTEA: u32 = 17;
  pub const CHAR: u32 = 18;
  pub const NAME: u32 = 19;
  pub const INT8: u32 = 20;
  pub const INT2: u32 = 21;
  pub const INT2VECTOR: u32 = 22;
  pub const INT4: u32 = 23;
  pub const REGPROC: u32 = 24;
  pub const TEXT: u32 = 25;
  pub const OID: u32 = 26;
  pub const XID: u32 = 28;
  pub const CID: u32 = 29;
  pub const OIDVECTOR: u32 = 30;
  pub const JSON: u32 = 114;
  pub const XML: u32 = 142;
  pub const POINT: u32 = 600;
  pub const CIDR: u32 = 650;
  pub const FLOAT4: u32 = 700;
  pub const FLOAT8: u32 = 701;
  pub const UNKNOWN: u32 = 705;
  pub const MACADDR8: u32 = 774;
  pub const MONEY: u32 = 790;
  pub const MACADDR: u32 = 829;
  pub const INET: u32 = 869;
  pub const BPCHAR: u32 = 1042;
  pub const VARCHAR: u32 = 1043;
  pub const DATE: u32 = 1082;
  pub const TIME: u32 = 1083;
  pub const TIMESTAMP: u32 = 1114;
  pub const TIMESTAMPTZ: u32 = 1184;
  pub const INTERVAL: u32 = 1186;
  pub const TIMETZ: u32 = 1266;
  pub const BIT: u32 = 1560;
  pub const VARBIT: u32 = 1562;
  pub const NUMERIC: u32 = 1700;
  pub const REFCURSOR: u32 = 1790;
  pub const REGPROCEDURE: u32 = 2202;
  pub const REGOPER: u32 = 2203;
  pub const REGOPERATOR: u32 = 2204;
  pub const REGCLASS: u32 = 2205;
  pub const REGTYPE: u32 = 2206;
  pub const VOID: u32 = 2278;
  pub const UUID: u32 = 2950;
  pub const PG_LSN: u32 = 3220;
  pub const REGCONFIG: u32 = 3734;
  pub const REGDICTIONARY: u32 = 3769;
  pub const JSONB: u32 = 3802;
  pub const JSONPATH: u32 = 4072;
  pub const REGNAMESPACE: u32 = 4089;
  pub const REGROLE: u32 = 4096;
  pub const REGCOLLATION: u32 = 4191;
  pub const XID8: u32 = 5069;
}

// ---------------------------------------------------------------------------
// Byte readers

struct Reader<'b> {
  buf: &'b [u8],
}

impl<'b> Reader<'b> {
  fn new(buf: &'b [u8]) -> Self {
    Self { buf }
  }
  fn take(&mut self, n: usize) -> DecodeResult<&'b [u8]> {
    if self.buf.len() < n {
      return Err("truncated value".into());
    }
    let (head, tail) = self.buf.split_at(n);
    self.buf = tail;
    Ok(head)
  }
  fn i16(&mut self) -> DecodeResult<i16> {
    Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
  }
  fn u16(&mut self) -> DecodeResult<u16> {
    Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
  }
  fn i32(&mut self) -> DecodeResult<i32> {
    Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
  }
  fn u32(&mut self) -> DecodeResult<u32> {
    Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
  }
  fn i64(&mut self) -> DecodeResult<i64> {
    Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
  }
  fn f64(&mut self) -> DecodeResult<f64> {
    Ok(f64::from_be_bytes(self.take(8)?.try_into().unwrap()))
  }
  fn rest(&mut self) -> &'b [u8] {
    std::mem::take(&mut self.buf)
  }
  fn done(&self) -> DecodeResult<()> {
    if self.buf.is_empty() {
      Ok(())
    } else {
      Err("trailing bytes in value".into())
    }
  }
}

fn exact<const N: usize>(buf: &[u8]) -> DecodeResult<[u8; N]> {
  buf
    .try_into()
    .map_err(|_| format!("expected {N} bytes, got {}", buf.len()))
}

// ---------------------------------------------------------------------------
// Text renderings (the server's text output)

/// `numeric` from its binary form (base-10000 digits) to its text form.
pub fn numeric_to_string(buf: &[u8]) -> DecodeResult<String> {
  let mut r = Reader::new(buf);
  let ndigits = r.i16()?;
  let weight = r.i16()? as i32;
  let sign = r.u16()?;
  let dscale = r.u16()? as i32;
  match sign {
    0xC000 => return Ok("NaN".into()),
    0xD000 => return Ok("Infinity".into()),
    0xF000 => return Ok("-Infinity".into()),
    0x0000 | 0x4000 => {}
    _ => return Err("invalid numeric sign".into()),
  }
  if ndigits < 0 {
    return Err("invalid numeric digit count".into());
  }
  let mut digits = Vec::with_capacity(ndigits as usize);
  for _ in 0..ndigits {
    let d = r.i16()?;
    if !(0..10_000).contains(&d) {
      return Err("invalid numeric digit".into());
    }
    digits.push(d);
  }
  r.done()?;
  let digit = |i: i32| -> i16 {
    if i >= 0 && (i as usize) < digits.len() {
      digits[i as usize]
    } else {
      0
    }
  };
  let mut out = String::new();
  if sign == 0x4000 {
    out.push('-');
  }
  if weight < 0 {
    out.push('0');
  } else {
    for i in 0..=weight {
      let d = digit(i);
      if i == 0 {
        out.push_str(&d.to_string());
      } else {
        out.push_str(&format!("{d:04}"));
      }
    }
  }
  if dscale > 0 {
    out.push('.');
    let mut frac = String::new();
    let groups = (dscale + 3) / 4;
    for g in 0..groups {
      frac.push_str(&format!("{:04}", digit(weight + 1 + g)));
    }
    frac.truncate(dscale as usize);
    out.push_str(&frac);
  }
  Ok(out)
}

fn push_fraction(out: &mut String, micros: i64) {
  if micros != 0 {
    let mut frac = format!(".{micros:06}");
    while frac.ends_with('0') {
      frac.pop();
    }
    out.push_str(&frac);
  }
}

/// `time` (microseconds since midnight) as `HH:MM:SS[.ffffff]`.
pub fn time_to_string(micros: i64) -> String {
  let secs = micros / 1_000_000;
  let mut out = format!(
    "{:02}:{:02}:{:02}",
    secs / 3600,
    (secs / 60) % 60,
    secs % 60
  );
  push_fraction(&mut out, micros % 1_000_000);
  out
}

/// `timetz`: the time plus the zone offset (`zone` is seconds west of UTC).
pub fn timetz_to_string(micros: i64, zone: i32) -> String {
  let mut out = time_to_string(micros);
  let offset = -(zone as i64);
  let sign = if offset < 0 { '-' } else { '+' };
  let abs = offset.abs();
  let (h, m, s) = (abs / 3600, (abs / 60) % 60, abs % 60);
  out.push(sign);
  out.push_str(&format!("{h:02}"));
  if m != 0 || s != 0 {
    out.push_str(&format!(":{m:02}"));
  }
  if s != 0 {
    out.push_str(&format!(":{s:02}"));
  }
  out
}

/// `interval` in the server's default `IntervalStyle` (`postgres`).
pub fn interval_to_string(micros: i64, days: i32, months: i32) -> String {
  let mut out = String::new();
  let mut is_zero = true;
  let mut is_before = false;
  let mut part = |value: i64, unit: &str, out: &mut String| {
    if value == 0 {
      return;
    }
    if !is_zero {
      out.push(' ');
    }
    if is_before && value > 0 {
      out.push('+');
    }
    out.push_str(&format!(
      "{value} {unit}{}",
      if value != 1 { "s" } else { "" }
    ));
    is_before = value < 0;
    is_zero = false;
  };
  part((months / 12) as i64, "year", &mut out);
  part((months % 12) as i64, "mon", &mut out);
  part(days as i64, "day", &mut out);
  let hour = micros / 3_600_000_000;
  let min = (micros / 60_000_000) % 60;
  let sec = (micros / 1_000_000) % 60;
  let fsec = micros % 1_000_000;
  if is_zero || hour != 0 || min != 0 || sec != 0 || fsec != 0 {
    let minus = hour < 0 || min < 0 || sec < 0 || fsec < 0;
    if !is_zero {
      out.push(' ');
    }
    if minus {
      out.push('-');
    } else if is_before {
      out.push('+');
    }
    out.push_str(&format!(
      "{:02}:{:02}:{:02}",
      hour.abs(),
      min.abs(),
      sec.abs()
    ));
    push_fraction(&mut out, fsec.abs());
  }
  out
}

pub fn uuid_to_string(b: &[u8; 16]) -> String {
  let hex = |s: &[u8]| s.iter().map(|b| format!("{b:02x}")).collect::<String>();
  format!(
    "{}-{}-{}-{}-{}",
    hex(&b[0..4]),
    hex(&b[4..6]),
    hex(&b[6..8]),
    hex(&b[8..10]),
    hex(&b[10..16])
  )
}

/// `inet` / `cidr`: `cidr` always shows the prefix length, `inet` only when it
/// is not the full address width.
pub fn inet_to_string(buf: &[u8], is_cidr: bool) -> DecodeResult<String> {
  let mut r = Reader::new(buf);
  let family = r.take(1)?[0];
  let bits = r.take(1)?[0];
  let _is_cidr_flag = r.take(1)?[0];
  let len = r.take(1)?[0] as usize;
  let addr = r.take(len)?;
  r.done()?;
  let (text, width) = match (family, len) {
    (2, 4) => (
      std::net::Ipv4Addr::from(exact::<4>(addr)?).to_string(),
      32u8,
    ),
    (3, 16) => (
      std::net::Ipv6Addr::from(exact::<16>(addr)?).to_string(),
      128u8,
    ),
    _ => return Err("invalid inet value".into()),
  };
  Ok(if is_cidr || bits != width {
    format!("{text}/{bits}")
  } else {
    text
  })
}

pub fn macaddr_to_string(buf: &[u8]) -> String {
  buf
    .iter()
    .map(|b| format!("{b:02x}"))
    .collect::<Vec<_>>()
    .join(":")
}

pub fn bits_to_string(buf: &[u8]) -> DecodeResult<String> {
  let mut r = Reader::new(buf);
  let nbits = r.i32()?;
  if nbits < 0 {
    return Err("invalid bit string length".into());
  }
  let bytes = r.rest();
  let nbits = nbits as usize;
  if bytes.len() * 8 < nbits {
    return Err("truncated bit string".into());
  }
  Ok(
    (0..nbits)
      .map(|i| {
        if bytes[i / 8] & (0x80 >> (i % 8)) != 0 {
          '1'
        } else {
          '0'
        }
      })
      .collect(),
  )
}

pub fn money_to_string(cents: i64) -> String {
  let sign = if cents < 0 { "-" } else { "" };
  let abs = cents.unsigned_abs();
  format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

// ---------------------------------------------------------------------------
// V8 construction

fn v8_str<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  s: &[u8],
) -> DecodeResult<v8::Local<'a, v8::Value>> {
  v8::String::new_from_utf8(scope, s, v8::NewStringType::Normal)
    .map(Into::into)
    .ok_or_else(|| "string value too long".to_string())
}

fn utf8(buf: &[u8]) -> DecodeResult<&[u8]> {
  std::str::from_utf8(buf)
    .map(|s| s.as_bytes())
    .map_err(|_| "invalid UTF-8 in text value".to_string())
}

pub fn v8_bytes<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  buf: &[u8],
) -> v8::Local<'a, v8::Value> {
  let len = buf.len();
  let ab = if len == 0 {
    v8::ArrayBuffer::new(scope, 0)
  } else {
    let bs =
      v8::ArrayBuffer::new_backing_store_from_vec(buf.to_vec()).make_shared();
    v8::ArrayBuffer::with_backing_store(scope, &bs)
  };
  v8::Uint8Array::new(scope, ab, 0, len).unwrap().into()
}

fn v8_date<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  ms: f64,
) -> DecodeResult<v8::Local<'a, v8::Value>> {
  v8::Date::new(scope, ms)
    .map(Into::into)
    .ok_or_else(|| "could not create Date".to_string())
}

fn v8_i64<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  v: i64,
  opts: DecodeOptions,
) -> DecodeResult<v8::Local<'a, v8::Value>> {
  if opts.bigint {
    Ok(v8::BigInt::new_from_i64(scope, v).into())
  } else {
    v8_str(scope, v.to_string().as_bytes())
  }
}

fn v8_json<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  text: &[u8],
) -> DecodeResult<v8::Local<'a, v8::Value>> {
  let s =
    v8::String::new_from_utf8(scope, utf8(text)?, v8::NewStringType::Normal)
      .ok_or_else(|| "JSON value too long".to_string())?;
  v8::json::parse(scope, s).ok_or_else(|| "invalid JSON value".to_string())
}

fn looks_like_text(buf: &[u8]) -> bool {
  std::str::from_utf8(buf).is_ok()
    && !buf.iter().any(|&b| b < 0x09 || (0x0e..0x20).contains(&b))
}

/// Decodes one non-null binary-format value of type `ty`.
pub fn decode<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  ty: &Type,
  buf: &[u8],
  opts: DecodeOptions,
) -> DecodeResult<v8::Local<'a, v8::Value>> {
  decode_nested(scope, ty, buf, opts, 0)
}

fn decode_nested<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  ty: &Type,
  buf: &[u8],
  opts: DecodeOptions,
  depth: u32,
) -> DecodeResult<v8::Local<'a, v8::Value>> {
  if depth > MAX_NESTING {
    return Err("value is nested too deeply".into());
  }
  let value: v8::Local<v8::Value> = match ty.oid() {
    oid::BOOL => v8::Boolean::new(scope, exact::<1>(buf)?[0] != 0).into(),
    oid::INT2 => {
      v8::Integer::new(scope, i16::from_be_bytes(exact(buf)?) as i32).into()
    }
    oid::INT4 => {
      v8::Integer::new(scope, i32::from_be_bytes(exact(buf)?)).into()
    }
    oid::OID
    | oid::XID
    | oid::CID
    | oid::REGPROC
    | oid::REGPROCEDURE
    | oid::REGOPER
    | oid::REGOPERATOR
    | oid::REGCLASS
    | oid::REGTYPE
    | oid::REGCONFIG
    | oid::REGDICTIONARY
    | oid::REGNAMESPACE
    | oid::REGROLE
    | oid::REGCOLLATION => {
      v8::Integer::new_from_unsigned(scope, u32::from_be_bytes(exact(buf)?))
        .into()
    }
    oid::INT8 => v8_i64(scope, i64::from_be_bytes(exact(buf)?), opts)?,
    oid::XID8 => {
      let v = u64::from_be_bytes(exact(buf)?);
      if opts.bigint {
        v8::BigInt::new_from_u64(scope, v).into()
      } else {
        v8_str(scope, v.to_string().as_bytes())?
      }
    }
    oid::FLOAT4 => {
      v8::Number::new(scope, f32::from_be_bytes(exact(buf)?) as f64).into()
    }
    oid::FLOAT8 => {
      v8::Number::new(scope, f64::from_be_bytes(exact(buf)?)).into()
    }
    oid::NUMERIC => v8_str(scope, numeric_to_string(buf)?.as_bytes())?,
    oid::MONEY => v8_str(
      scope,
      money_to_string(i64::from_be_bytes(exact(buf)?)).as_bytes(),
    )?,
    oid::TEXT
    | oid::VARCHAR
    | oid::BPCHAR
    | oid::NAME
    | oid::CHAR
    | oid::UNKNOWN
    | oid::XML
    | oid::REFCURSOR => v8_str(scope, utf8(buf)?)?,
    oid::JSON => v8_json(scope, buf)?,
    oid::JSONB => match buf.split_first() {
      Some((1, rest)) => v8_json(scope, rest)?,
      _ => return Err("unsupported jsonb version".into()),
    },
    oid::JSONPATH => match buf.split_first() {
      Some((1, rest)) => v8_str(scope, utf8(rest)?)?,
      _ => return Err("unsupported jsonpath version".into()),
    },
    oid::BYTEA => v8_bytes(scope, buf),
    oid::TIMESTAMP | oid::TIMESTAMPTZ => {
      let us = i64::from_be_bytes(exact(buf)?);
      match us {
        i64::MAX => v8_str(scope, b"infinity")?,
        i64::MIN => v8_str(scope, b"-infinity")?,
        // A Date holds whole milliseconds: floor the microseconds.
        _ => v8_date(scope, (us.div_euclid(1000) + PG_EPOCH_MS) as f64)?,
      }
    }
    oid::DATE => {
      let days = i32::from_be_bytes(exact(buf)?);
      match days {
        i32::MAX => v8_str(scope, b"infinity")?,
        i32::MIN => v8_str(scope, b"-infinity")?,
        _ => {
          v8_date(scope, ((days as i64 + PG_EPOCH_DAYS) * 86_400_000) as f64)?
        }
      }
    }
    oid::TIME => v8_str(
      scope,
      time_to_string(i64::from_be_bytes(exact(buf)?)).as_bytes(),
    )?,
    oid::TIMETZ => {
      let mut r = Reader::new(buf);
      let (us, zone) = (r.i64()?, r.i32()?);
      r.done()?;
      v8_str(scope, timetz_to_string(us, zone).as_bytes())?
    }
    oid::INTERVAL => {
      let mut r = Reader::new(buf);
      let (us, days, months) = (r.i64()?, r.i32()?, r.i32()?);
      r.done()?;
      v8_str(scope, interval_to_string(us, days, months).as_bytes())?
    }
    oid::UUID => v8_str(scope, uuid_to_string(&exact(buf)?).as_bytes())?,
    oid::INET => v8_str(scope, inet_to_string(buf, false)?.as_bytes())?,
    oid::CIDR => v8_str(scope, inet_to_string(buf, true)?.as_bytes())?,
    oid::MACADDR => {
      v8_str(scope, macaddr_to_string(&exact::<6>(buf)?).as_bytes())?
    }
    oid::MACADDR8 => {
      v8_str(scope, macaddr_to_string(&exact::<8>(buf)?).as_bytes())?
    }
    oid::BIT | oid::VARBIT => v8_str(scope, bits_to_string(buf)?.as_bytes())?,
    oid::PG_LSN => {
      let v = u64::from_be_bytes(exact(buf)?);
      v8_str(
        scope,
        format!("{:X}/{:X}", v >> 32, v & 0xffff_ffff).as_bytes(),
      )?
    }
    oid::POINT => {
      let mut r = Reader::new(buf);
      let (x, y) = (r.f64()?, r.f64()?);
      r.done()?;
      let names = [
        v8::String::new(scope, "x").unwrap().into(),
        v8::String::new(scope, "y").unwrap().into(),
      ];
      let values = [
        v8::Number::new(scope, x).into(),
        v8::Number::new(scope, y).into(),
      ];
      let proto = v8::Object::new(scope).get_prototype(scope).unwrap();
      v8::Object::with_prototype_and_properties(scope, proto, &names, &values)
        .into()
    }
    oid::VOID => v8::null(scope).into(),
    oid::INT2VECTOR => decode_array(scope, &Type::INT2, buf, opts, depth)?,
    oid::OIDVECTOR => decode_array(scope, &Type::OID, buf, opts, depth)?,
    _ => match ty.kind() {
      Kind::Array(elem) => decode_array(scope, elem, buf, opts, depth)?,
      Kind::Domain(base) => decode_nested(scope, base, buf, opts, depth + 1)?,
      Kind::Enum(_) => v8_str(scope, utf8(buf)?)?,
      Kind::Composite(fields) => {
        decode_composite(scope, fields, buf, opts, depth)?
      }
      _ if ty.oid() >= FIRST_NORMAL_OBJECT_ID && looks_like_text(buf) => {
        // An extension type whose binary form is its text (citext, ...).
        v8_str(scope, buf)?
      }
      // Ranges, geometry other than point, tsvector, ...: the raw bytes.
      _ => v8_bytes(scope, buf),
    },
  };
  Ok(value)
}

fn decode_array<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  elem: &Type,
  buf: &[u8],
  opts: DecodeOptions,
  depth: u32,
) -> DecodeResult<v8::Local<'a, v8::Value>> {
  let mut r = Reader::new(buf);
  let ndim = r.i32()?;
  let _has_nulls = r.i32()?;
  let _elem_oid = r.u32()?;
  if !(0..=6).contains(&ndim) {
    return Err("invalid array dimensions".into());
  }
  let mut dims = Vec::with_capacity(ndim as usize);
  for _ in 0..ndim {
    let len = r.i32()?;
    let _lower = r.i32()?;
    if len < 0 {
      return Err("invalid array dimension length".into());
    }
    dims.push(len as usize);
  }
  if dims.is_empty() {
    return Ok(v8::Array::new(scope, 0).into());
  }
  build_array(scope, elem, &mut r, &dims, opts, depth)
}

fn build_array<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  elem: &Type,
  r: &mut Reader<'_>,
  dims: &[usize],
  opts: DecodeOptions,
  depth: u32,
) -> DecodeResult<v8::Local<'a, v8::Value>> {
  let mut items = Vec::with_capacity(dims[0].min(1 << 16));
  for _ in 0..dims[0] {
    if dims.len() > 1 {
      items.push(build_array(scope, elem, r, &dims[1..], opts, depth)?);
    } else {
      let len = r.i32()?;
      if len < 0 {
        items.push(v8::null(scope).into());
      } else {
        let bytes = r.take(len as usize)?;
        items.push(decode_nested(scope, elem, bytes, opts, depth + 1)?);
      }
    }
  }
  Ok(v8::Array::new_with_elements(scope, &items).into())
}

fn decode_composite<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  fields: &[postgres_types::Field],
  buf: &[u8],
  opts: DecodeOptions,
  depth: u32,
) -> DecodeResult<v8::Local<'a, v8::Value>> {
  let mut r = Reader::new(buf);
  let count = r.i32()?;
  if count < 0 || count as usize != fields.len() {
    return Err("composite field count mismatch".into());
  }
  let obj = v8::Object::new(scope);
  for field in fields {
    let _oid = r.u32()?;
    let len = r.i32()?;
    let value = if len < 0 {
      v8::null(scope).into()
    } else {
      let bytes = r.take(len as usize)?;
      decode_nested(scope, field.type_(), bytes, opts, depth + 1)?
    };
    let key = v8_str(scope, field.name().as_bytes())?;
    obj.create_data_property(scope, key.try_into().unwrap(), value);
  }
  Ok(obj.into())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn numeric(
    ndigits: i16,
    weight: i16,
    sign: u16,
    dscale: u16,
    d: &[i16],
  ) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&ndigits.to_be_bytes());
    v.extend_from_slice(&weight.to_be_bytes());
    v.extend_from_slice(&sign.to_be_bytes());
    v.extend_from_slice(&dscale.to_be_bytes());
    for x in d {
      v.extend_from_slice(&x.to_be_bytes());
    }
    v
  }

  #[test]
  fn numerics() {
    // 12345.678 = [1, 2345, 6780] weight 1, dscale 3
    assert_eq!(
      numeric_to_string(&numeric(3, 1, 0, 3, &[1, 2345, 6780])).unwrap(),
      "12345.678"
    );
    assert_eq!(numeric_to_string(&numeric(0, 0, 0, 0, &[])).unwrap(), "0");
    assert_eq!(
      numeric_to_string(&numeric(0, 0, 0, 2, &[])).unwrap(),
      "0.00"
    );
    // -0.0001234 = [1, 2340] weight -1 dscale 7
    assert_eq!(
      numeric_to_string(&numeric(2, -1, 0x4000, 7, &[1, 2340])).unwrap(),
      "-0.0001234"
    );
    // 0.00000001 = [1] weight -2 dscale 8
    assert_eq!(
      numeric_to_string(&numeric(1, -2, 0, 8, &[1])).unwrap(),
      "0.00000001"
    );
    // 100000000 = [1] weight 2
    assert_eq!(
      numeric_to_string(&numeric(1, 2, 0, 0, &[1])).unwrap(),
      "100000000"
    );
    assert_eq!(
      numeric_to_string(&numeric(0, 0, 0xC000, 0, &[])).unwrap(),
      "NaN"
    );
    assert!(numeric_to_string(&numeric(1, 0, 0, 0, &[])).is_err());
  }

  #[test]
  fn times() {
    assert_eq!(time_to_string(0), "00:00:00");
    assert_eq!(time_to_string(45_296_789_000), "12:34:56.789");
    assert_eq!(time_to_string(86_400_000_000), "24:00:00");
    assert_eq!(timetz_to_string(0, -3600 * 5 - 1800), "00:00:00+05:30");
    assert_eq!(timetz_to_string(1, 3600 * 8), "00:00:00.000001-08");
  }

  #[test]
  fn intervals() {
    assert_eq!(interval_to_string(0, 0, 0), "00:00:00");
    assert_eq!(
      interval_to_string(14_706_789_000, 3, 14),
      "1 year 2 mons 3 days 04:05:06.789"
    );
    assert_eq!(interval_to_string(0, 1, 0), "1 day");
    assert_eq!(interval_to_string(-3_600_000_000, 0, 0), "-01:00:00");
    assert_eq!(
      interval_to_string(3_600_000_000, -1, 0),
      "-1 days +01:00:00"
    );
    assert_eq!(interval_to_string(0, 2, -1), "-1 mons +2 days");
  }

  #[test]
  fn misc_text() {
    let u = [
      0xa0, 0xee, 0xbc, 0x99, 0x9c, 0x0b, 0x4e, 0xf8, 0xbb, 0x6d, 0x6b, 0xb9,
      0xbd, 0x38, 0x0a, 0x11,
    ];
    assert_eq!(uuid_to_string(&u), "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11");
    assert_eq!(
      inet_to_string(&[2, 32, 0, 4, 10, 0, 0, 1], false).unwrap(),
      "10.0.0.1"
    );
    assert_eq!(
      inet_to_string(&[2, 24, 1, 4, 10, 0, 0, 0], true).unwrap(),
      "10.0.0.0/24"
    );
    assert_eq!(
      inet_to_string(&[2, 24, 0, 4, 10, 0, 0, 1], false).unwrap(),
      "10.0.0.1/24"
    );
    assert_eq!(
      macaddr_to_string(&[8, 0, 0x2b, 1, 2, 3]),
      "08:00:2b:01:02:03"
    );
    assert_eq!(bits_to_string(&[0, 0, 0, 5, 0b1010_1000]).unwrap(), "10101");
    assert_eq!(money_to_string(-1234), "-12.34");
    assert_eq!(money_to_string(5), "0.05");
    assert!(looks_like_text(b"hello"));
    assert!(!looks_like_text(&[0, 1, 2]));
  }
}
