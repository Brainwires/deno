// Copyright 2018-2026 the Deno authors. MIT license.

//! Query parameters: JavaScript values converted once on the way in, then
//! encoded for the engine. Postgres parameters go over the wire in the text
//! format (the server parses them as whatever type it inferred for `$n`), so
//! one encoder covers every type; only `bytea` takes raw bytes.

use deno_core::FromV8;
use deno_core::v8;
use postgres_types::Format;
use postgres_types::IsNull;
use postgres_types::Kind;
use postgres_types::ToSql;
use postgres_types::Type;

use crate::error::SqlError;

/// One query parameter.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlParam {
  Null,
  Bool(bool),
  Number(f64),
  /// A JavaScript `bigint`, as its decimal digits.
  BigInt(String),
  Text(String),
  Bytes(Vec<u8>),
  /// A plain object, already `JSON.stringify`-ed.
  Json(String),
  /// A `Date`: milliseconds since the epoch, and the local time zone's
  /// offset from UTC at that instant in seconds (east positive), as the
  /// runtime's `Date` sees it.
  Date(f64, i32),
  Array(Vec<SqlParam>),
}

/// The parameter list of one query.
#[derive(Debug, Default)]
pub struct SqlParams(pub Vec<SqlParam>);

const MAX_ARRAY_DEPTH: u32 = 32;

/// `date.getTimezoneOffset()`: minutes local time is behind UTC at the
/// date's instant, from the runtime's own time-zone data (so `TZ` applies the
/// same way it does to JavaScript).
pub fn tz_offset_minutes<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  date: v8::Local<'a, v8::Date>,
) -> Option<f64> {
  let key = v8::String::new(scope, "getTimezoneOffset")?;
  let method = date.get(scope, key.into())?;
  let method = v8::Local::<v8::Function>::try_from(method).ok()?;
  method.call(scope, date.into(), &[])?.number_value(scope)
}

fn from_v8_value<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  value: v8::Local<'a, v8::Value>,
  depth: u32,
) -> Result<SqlParam, SqlError> {
  if value.is_null_or_undefined() {
    return Ok(SqlParam::Null);
  }
  if value.is_boolean() {
    return Ok(SqlParam::Bool(value.boolean_value(scope)));
  }
  if value.is_number() {
    return Ok(SqlParam::Number(
      value.number_value(scope).unwrap_or(f64::NAN),
    ));
  }
  if value.is_string() {
    return Ok(SqlParam::Text(value.to_rust_string_lossy(scope)));
  }
  if value.is_big_int() {
    return Ok(SqlParam::BigInt(value.to_rust_string_lossy(scope)));
  }
  if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(value) {
    let mut bytes = vec![0u8; view.byte_length()];
    view.copy_contents(&mut bytes);
    return Ok(SqlParam::Bytes(bytes));
  }
  if let Ok(buf) = v8::Local::<v8::ArrayBuffer>::try_from(value) {
    let len = buf.byte_length();
    let mut bytes = vec![0u8; len];
    if len > 0
      && let Some(data) = buf.data()
    {
      // SAFETY: `data` points at `len` live bytes of the buffer, which stays
      // alive for the duration of this call.
      bytes.copy_from_slice(unsafe {
        std::slice::from_raw_parts(data.as_ptr() as *const u8, len)
      });
    }
    return Ok(SqlParam::Bytes(bytes));
  }
  if let Ok(date) = v8::Local::<v8::Date>::try_from(value) {
    let offset = tz_offset_minutes(scope, date).unwrap_or(0.0);
    let offset = if offset.is_finite() {
      -(offset * 60.0).round() as i32
    } else {
      0
    };
    return Ok(SqlParam::Date(date.value_of(), offset));
  }
  if let Ok(array) = v8::Local::<v8::Array>::try_from(value) {
    if depth >= MAX_ARRAY_DEPTH {
      return Err(SqlError::type_error("Array parameter is nested too deeply"));
    }
    let len = array.length();
    let mut out = Vec::with_capacity(len as usize);
    for i in 0..len {
      let item = array
        .get_index(scope, i)
        .unwrap_or_else(|| v8::undefined(scope).into());
      out.push(from_v8_value(scope, item, depth + 1)?);
    }
    return Ok(SqlParam::Array(out));
  }
  if value.is_object() {
    return match v8::json::stringify(scope, value) {
      Some(s) => Ok(SqlParam::Json(s.to_rust_string_lossy(scope))),
      None => Err(SqlError::type_error(
        "Parameter could not be serialized to JSON",
      )),
    };
  }
  // Symbols and functions have no SQL representation.
  Err(SqlError::type_error(format!(
    "Unsupported parameter type: {}",
    value.type_repr()
  )))
}

impl<'a> FromV8<'a> for SqlParams {
  type Error = SqlError;

  fn from_v8<'i>(
    scope: &mut v8::PinScope<'a, 'i>,
    value: v8::Local<'a, v8::Value>,
  ) -> Result<Self, Self::Error> {
    if value.is_null_or_undefined() {
      return Ok(SqlParams(Vec::new()));
    }
    let array = v8::Local::<v8::Array>::try_from(value)
      .map_err(|_| SqlError::type_error("Query parameters must be an array"))?;
    let len = array.length();
    let mut out = Vec::with_capacity(len as usize);
    for i in 0..len {
      let item = array
        .get_index(scope, i)
        .unwrap_or_else(|| v8::undefined(scope).into());
      out.push(from_v8_value(scope, item, 0)?);
    }
    Ok(SqlParams(out))
  }
}

// ---------------------------------------------------------------------------
// Text encoders

/// A JavaScript number as Postgres reads it: integral values without a
/// fraction, the special values by their Postgres names.
pub fn number_to_text(n: f64) -> String {
  if n.is_nan() {
    "NaN".to_string()
  } else if n.is_infinite() {
    if n > 0.0 { "Infinity" } else { "-Infinity" }.to_string()
  } else if n == 0.0 {
    "0".to_string()
  } else {
    // `Display` for f64 is the shortest round-tripping decimal, never in
    // exponent notation, which every numeric input function accepts.
    format!("{n}")
  }
}

/// `Date.prototype.toISOString()` for a time value; `None` for an invalid
/// date.
pub fn date_to_iso(ms: f64) -> Option<String> {
  if !ms.is_finite() || ms.abs() > 8.64e15 {
    return None;
  }
  let ms = ms as i64;
  let days = ms.div_euclid(86_400_000);
  let rem = ms.rem_euclid(86_400_000);
  let (y, m, d) = civil_from_days(days);
  let (h, mi, s, milli) = (
    rem / 3_600_000,
    (rem / 60_000) % 60,
    (rem / 1000) % 60,
    rem % 1000,
  );
  let year = if (0..=9999).contains(&y) {
    format!("{y:04}")
  } else if y < 0 {
    format!("-{:06}", -y)
  } else {
    format!("+{y:06}")
  };
  Some(format!(
    "{year}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{milli:03}Z"
  ))
}

/// Days since 1970-01-01 to a proleptic Gregorian (year, month, day).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
  let z = z + 719_468;
  let era = z.div_euclid(146_097);
  let doe = z.rem_euclid(146_097);
  let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
  let y = yoe + era * 400;
  let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
  let mp = (5 * doy + 2) / 153;
  let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
  let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
  (if m <= 2 { y + 1 } else { y }, m, d)
}

fn hex_bytea(bytes: &[u8]) -> String {
  const HEX: &[u8; 16] = b"0123456789abcdef";
  let mut out = String::with_capacity(2 + bytes.len() * 2);
  out.push_str("\\x");
  for b in bytes {
    out.push(HEX[(b >> 4) as usize] as char);
    out.push(HEX[(b & 0xf) as usize] as char);
  }
  out
}

fn date_text(ms: f64) -> Result<String, SqlError> {
  date_to_iso(ms).ok_or_else(|| SqlError::type_error("Invalid Date parameter"))
}

/// A `Date` as local time with its UTC offset
/// (`2024-01-15T12:00:00.000-06:00`): a `timestamptz` reads the same instant,
/// a `timestamp` (without time zone) the local wall-clock time, as Bun,
/// postgres.js and node-postgres send it.
pub fn local_date_text(ms: f64, offset_s: i32) -> Result<String, SqlError> {
  let wall = date_text(ms + offset_s as f64 * 1000.0)?;
  let mut out = wall.trim_end_matches('Z').to_string();
  let abs = offset_s.unsigned_abs();
  out.push(if offset_s < 0 { '-' } else { '+' });
  out.push_str(&format!("{:02}:{:02}", abs / 3600, (abs / 60) % 60));
  if !abs.is_multiple_of(60) {
    out.push_str(&format!(":{:02}", abs % 60));
  }
  Ok(out)
}

/// How a `Date` is written for a Postgres parameter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DateStyle {
  /// Local time with its offset (timestamps, and parameters whose type the
  /// server picks).
  Local,
  /// The UTC instant (`date`, which `Date`s are read back from as UTC
  /// midnight; JSON).
  Utc,
}

/// A scalar parameter in the text format (arrays use [`array_literal`] or
/// [`json_text`]).
fn scalar_text(p: &SqlParam, dates: DateStyle) -> Result<String, SqlError> {
  Ok(match p {
    SqlParam::Null => String::new(),
    // `true` / `false`: what `bool::text` gives, and the bool input reads.
    SqlParam::Bool(b) => if *b { "true" } else { "false" }.to_string(),
    SqlParam::Number(n) => number_to_text(*n),
    SqlParam::BigInt(s) | SqlParam::Text(s) | SqlParam::Json(s) => s.clone(),
    SqlParam::Bytes(b) => hex_bytea(b),
    SqlParam::Date(ms, offset) => match dates {
      DateStyle::Local => local_date_text(*ms, *offset)?,
      DateStyle::Utc => date_text(*ms)?,
    },
    SqlParam::Array(_) => json_text(p)?,
  })
}

/// A Postgres array literal: `{1,"a b",NULL,{2,3}}`. Every non-null element is
/// double-quoted with `\` and `"` escaped, which the array input function
/// accepts for any element type.
pub fn array_literal(
  items: &[SqlParam],
  dates: DateStyle,
) -> Result<String, SqlError> {
  let mut out = String::from("{");
  for (i, item) in items.iter().enumerate() {
    if i > 0 {
      out.push(',');
    }
    match item {
      SqlParam::Null => out.push_str("NULL"),
      SqlParam::Array(inner) => out.push_str(&array_literal(inner, dates)?),
      other => {
        out.push('"');
        for c in scalar_text(other, dates)?.chars() {
          if c == '"' || c == '\\' {
            out.push('\\');
          }
          out.push(c);
        }
        out.push('"');
      }
    }
  }
  out.push('}');
  Ok(out)
}

fn push_json_string(out: &mut String, s: &str) {
  out.push('"');
  for c in s.chars() {
    match c {
      '"' => out.push_str("\\\""),
      '\\' => out.push_str("\\\\"),
      '\n' => out.push_str("\\n"),
      '\r' => out.push_str("\\r"),
      '\t' => out.push_str("\\t"),
      c if (c as u32) < 0x20 => {
        out.push_str(&format!("\\u{:04x}", c as u32));
      }
      c => out.push(c),
    }
  }
  out.push('"');
}

fn write_json(out: &mut String, p: &SqlParam) -> Result<(), SqlError> {
  match p {
    SqlParam::Null => out.push_str("null"),
    SqlParam::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
    SqlParam::Number(n) => {
      if n.is_finite() {
        out.push_str(&number_to_text(*n));
      } else {
        out.push_str("null");
      }
    }
    SqlParam::BigInt(s) | SqlParam::Json(s) => out.push_str(s),
    SqlParam::Text(s) => push_json_string(out, s),
    SqlParam::Bytes(b) => push_json_string(out, &hex_bytea(b)),
    SqlParam::Date(ms, _) => push_json_string(out, &date_text(*ms)?),
    SqlParam::Array(items) => {
      out.push('[');
      for (i, item) in items.iter().enumerate() {
        if i > 0 {
          out.push(',');
        }
        write_json(out, item)?;
      }
      out.push(']');
    }
  }
  Ok(())
}

/// A parameter as JSON text (for `json` / `jsonb` targets and SQLite).
pub fn json_text(p: &SqlParam) -> Result<String, SqlError> {
  let mut out = String::new();
  write_json(&mut out, p)?;
  Ok(out)
}

fn is_json(ty: &Type) -> bool {
  *ty == Type::JSON || *ty == Type::JSONB
}

fn is_array(ty: &Type) -> bool {
  matches!(ty.kind(), Kind::Array(_))
}

fn is_numeric(ty: &Type) -> bool {
  [
    Type::INT2,
    Type::INT4,
    Type::INT8,
    Type::NUMERIC,
    Type::FLOAT4,
    Type::FLOAT8,
    Type::OID,
  ]
  .contains(ty)
}

fn date_style(ty: &Type) -> DateStyle {
  if *ty == Type::DATE || *ty == Type::DATE_ARRAY {
    DateStyle::Utc
  } else {
    DateStyle::Local
  }
}

/// The text a parameter is sent as for a parameter of type `ty`.
pub fn pg_text(p: &SqlParam, ty: &Type) -> Result<String, SqlError> {
  if let (SqlParam::Bool(b), true) = (p, is_numeric(ty)) {
    // A boolean where the server expects a number (`${true}::int`): 1 / 0.
    return Ok(if *b { "1" } else { "0" }.to_string());
  }
  if is_json(ty) {
    return match p {
      // A string bound to a json parameter is the JSON text itself.
      SqlParam::Text(s) => Ok(s.clone()),
      other => json_text(other),
    };
  }
  match p {
    SqlParam::Array(items) => {
      if is_array(ty) || ty.oid() == 0 || *ty == Type::UNKNOWN {
        array_literal(items, date_style(ty))
      } else {
        // An array bound to a scalar (text, ...) parameter: its JSON.
        json_text(p)
      }
    }
    other => scalar_text(other, date_style(ty)),
  }
}

impl ToSql for SqlParam {
  fn to_sql(
    &self,
    ty: &Type,
    out: &mut bytes::BytesMut,
  ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
    if matches!(self, SqlParam::Null) {
      return Ok(IsNull::Yes);
    }
    if let (SqlParam::Bytes(bytes), true) = (self, *ty == Type::BYTEA) {
      out.extend_from_slice(bytes);
      return Ok(IsNull::No);
    }
    let text = pg_text(self, ty).map_err(|e| e.to_string())?;
    if text.contains('\0') {
      return Err("text parameters cannot contain NUL (\\0) characters".into());
    }
    out.extend_from_slice(text.as_bytes());
    Ok(IsNull::No)
  }

  fn accepts(_ty: &Type) -> bool {
    true
  }

  fn encode_format(&self, ty: &Type) -> Format {
    if matches!(self, SqlParam::Bytes(_)) && *ty == Type::BYTEA {
      Format::Binary
    } else {
      Format::Text
    }
  }

  postgres_types::to_sql_checked!();
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn numbers() {
    assert_eq!(number_to_text(1.0), "1");
    assert_eq!(number_to_text(-0.0), "0");
    assert_eq!(number_to_text(1.5), "1.5");
    assert_eq!(number_to_text(0.1 + 0.2), "0.30000000000000004");
    assert_eq!(number_to_text(1e21), "1000000000000000000000");
    assert_eq!(number_to_text(f64::NAN), "NaN");
    assert_eq!(number_to_text(f64::NEG_INFINITY), "-Infinity");
  }

  #[test]
  fn iso_dates() {
    assert_eq!(date_to_iso(0.0).unwrap(), "1970-01-01T00:00:00.000Z");
    assert_eq!(
      date_to_iso(1_700_000_000_123.0).unwrap(),
      "2023-11-14T22:13:20.123Z"
    );
    assert_eq!(date_to_iso(-1.0).unwrap(), "1969-12-31T23:59:59.999Z");
    assert_eq!(
      date_to_iso(951_782_400_000.0).unwrap(),
      "2000-02-29T00:00:00.000Z"
    );
    assert!(date_to_iso(f64::NAN).is_none());
  }

  #[test]
  fn array_literals_escape() {
    let lit = array_literal(
      &[
        SqlParam::Number(1.0),
        SqlParam::Text("a \"b\" \\c".into()),
        SqlParam::Null,
        SqlParam::Text("NULL".into()),
        SqlParam::Array(vec![SqlParam::Bool(true)]),
      ],
      DateStyle::Local,
    )
    .unwrap();
    assert_eq!(lit, r#"{"1","a \"b\" \\c",NULL,"NULL",{"true"}}"#);
    assert_eq!(array_literal(&[], DateStyle::Local).unwrap(), "{}");
    assert_eq!(
      array_literal(&[SqlParam::Bytes(vec![0xde, 0xad])], DateStyle::Local)
        .unwrap(),
      r#"{"\\xdead"}"#
    );
  }

  #[test]
  fn local_dates() {
    // 2024-01-15T18:00:00Z in UTC-6.
    let ms = 1_705_341_600_000.0;
    assert_eq!(
      local_date_text(ms, -6 * 3600).unwrap(),
      "2024-01-15T12:00:00.000-06:00"
    );
    assert_eq!(
      local_date_text(ms, 5 * 3600 + 1800).unwrap(),
      "2024-01-15T23:30:00.000+05:30"
    );
    // A historic offset with seconds (LMT).
    assert_eq!(
      local_date_text(ms, -(5 * 3600 + 50 * 60 + 36)).unwrap(),
      "2024-01-15T12:09:24.000-05:50:36"
    );
    let d = SqlParam::Date(ms, -6 * 3600);
    assert_eq!(
      pg_text(&d, &Type::TIMESTAMP).unwrap(),
      "2024-01-15T12:00:00.000-06:00"
    );
    assert_eq!(
      pg_text(&d, &Type::DATE).unwrap(),
      "2024-01-15T18:00:00.000Z"
    );
    assert_eq!(
      pg_text(&SqlParam::Array(vec![d.clone()]), &Type::DATE_ARRAY).unwrap(),
      r#"{"2024-01-15T18:00:00.000Z"}"#
    );
    assert_eq!(
      pg_text(&SqlParam::Array(vec![d.clone()]), &Type::JSONB).unwrap(),
      r#"["2024-01-15T18:00:00.000Z"]"#
    );
  }

  #[test]
  fn json_encoding() {
    let p = SqlParam::Array(vec![
      SqlParam::Text("q\"\n".into()),
      SqlParam::Number(2.5),
      SqlParam::Json("{\"a\":1}".into()),
      SqlParam::Null,
      SqlParam::Number(f64::NAN),
    ]);
    assert_eq!(json_text(&p).unwrap(), r#"["q\"\n",2.5,{"a":1},null,null]"#);
    assert_eq!(
      pg_text(&SqlParam::Text("{}".into()), &Type::JSONB).unwrap(),
      "{}"
    );
    assert_eq!(pg_text(&SqlParam::Bool(true), &Type::INT4).unwrap(), "1");
    assert_eq!(
      pg_text(&SqlParam::Bool(false), &Type::TEXT).unwrap(),
      "false"
    );
    assert_eq!(
      pg_text(&SqlParam::Array(vec![SqlParam::Number(1.0)]), &Type::JSONB)
        .unwrap(),
      "[1]"
    );
    assert_eq!(
      pg_text(
        &SqlParam::Array(vec![SqlParam::Number(1.0)]),
        &Type::INT4_ARRAY
      )
      .unwrap(),
      r#"{"1"}"#
    );
  }

  #[test]
  fn formats() {
    assert!(matches!(
      SqlParam::Bytes(vec![1]).encode_format(&Type::BYTEA),
      Format::Binary
    ));
    assert!(matches!(
      SqlParam::Bytes(vec![1]).encode_format(&Type::TEXT),
      Format::Text
    ));
    let mut buf = bytes::BytesMut::new();
    SqlParam::Bytes(vec![0xff, 0x00])
      .to_sql(&Type::BYTEA, &mut buf)
      .unwrap();
    assert_eq!(&buf[..], &[0xff, 0x00]);
    let mut buf = bytes::BytesMut::new();
    assert!(
      SqlParam::Text("a\0b".into())
        .to_sql(&Type::TEXT, &mut buf)
        .is_err()
    );
  }
}
