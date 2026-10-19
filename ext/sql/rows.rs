// Copyright 2018-2026 the Deno authors. MIT license.

//! Result rows built straight into V8: one op call returns every row of a
//! query, each row created with a single `with_prototype_and_properties`
//! call (objects) or `Array::new_with_elements` (`.values()` / `.raw()`).

use std::collections::HashMap;

use deno_core::v8;

/// Rows as arrays of values (`.values()`).
pub const FLAG_VALUES: u32 = 1;
/// Cells as the raw bytes the server sent (`.raw()`, implies arrays).
pub const FLAG_RAW: u32 = 2;
/// 64-bit integers as `bigint`.
pub const FLAG_BIGINT: u32 = 4;
/// Postgres: run as an unnamed statement even when statements are cached
/// (cursor commands, whose text differs per cursor).
pub const FLAG_UNNAMED: u32 = 8;
/// Postgres: a statement a pipelined transaction end is sent right behind
/// (`op_sql_pg_finish`). If it fails before going on the wire, the end that
/// follows it rolls back instead of committing.
pub const FLAG_GUARDED: u32 = 16;
/// Postgres: goes on the wire after every request made before it on its
/// connection, even when it has to prepare its statement first (statements
/// of a transaction or a reserved connection). Without it a cached
/// statement may overtake one being prepared, which only independent
/// queries (the pool's) can afford.
pub const FLAG_ORDERED: u32 = 32;

pub struct RowShape<'a> {
  arrays: bool,
  names: Vec<v8::Local<'a, v8::Name>>,
  /// For each entry of `names`, the column it takes its value from: the last
  /// column of that name, as when assigning the columns in order.
  pick: Vec<usize>,
  proto: v8::Local<'a, v8::Value>,
}

impl<'a> RowShape<'a> {
  pub fn new<S: AsRef<str>>(
    scope: &mut v8::PinScope<'a, '_>,
    columns: &[S],
    flags: u32,
  ) -> Result<Self, String> {
    let arrays = flags & (FLAG_VALUES | FLAG_RAW) != 0;
    let proto = v8::Object::new(scope)
      .get_prototype(scope)
      .unwrap_or_else(|| v8::null(scope).into());
    let mut names = Vec::new();
    let mut pick = Vec::new();
    if !arrays {
      let mut seen: HashMap<&str, usize> = HashMap::new();
      for (i, column) in columns.iter().enumerate() {
        let column = column.as_ref();
        if let Some(&slot) = seen.get(column) {
          pick[slot] = i;
          continue;
        }
        let name = v8::String::new(scope, column)
          .ok_or_else(|| "column name too long".to_string())?;
        seen.insert(column, names.len());
        names.push(name.into());
        pick.push(i);
      }
    }
    Ok(Self {
      arrays,
      names,
      pick,
      proto,
    })
  }

  pub fn build(
    &self,
    scope: &mut v8::PinScope<'a, '_>,
    cells: &[v8::Local<'a, v8::Value>],
  ) -> v8::Local<'a, v8::Value> {
    if self.arrays {
      return v8::Array::new_with_elements(scope, cells).into();
    }
    if self.pick.len() == cells.len() {
      return v8::Object::with_prototype_and_properties(
        scope,
        self.proto,
        &self.names,
        cells,
      )
      .into();
    }
    let values: Vec<_> = self.pick.iter().map(|&i| cells[i]).collect();
    v8::Object::with_prototype_and_properties(
      scope,
      self.proto,
      &self.names,
      &values,
    )
    .into()
  }
}

/// The op result: `[rows, count, extra, columns]`, where `columns` is the
/// array of column metadata objects (`result.columns`).
pub fn result_tuple<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  rows: &[v8::Local<'a, v8::Value>],
  count: f64,
  extra: v8::Local<'a, v8::Value>,
  columns: &[v8::Local<'a, v8::Value>],
) -> v8::Local<'a, v8::Value> {
  let rows = v8::Array::new_with_elements(scope, rows);
  let count = v8::Number::new(scope, count);
  let columns = v8::Array::new_with_elements(scope, columns);
  v8::Array::new_with_elements(
    scope,
    &[rows.into(), count.into(), extra, columns.into()],
  )
  .into()
}

/// Plain objects that all have the same keys in the same order (a result's
/// column metadata), so they share one hidden class.
pub struct Record<'a> {
  names: Vec<v8::Local<'a, v8::Name>>,
  proto: v8::Local<'a, v8::Value>,
}

impl<'a> Record<'a> {
  pub fn new(scope: &mut v8::PinScope<'a, '_>, keys: &[&str]) -> Self {
    let proto = v8::Object::new(scope)
      .get_prototype(scope)
      .unwrap_or_else(|| v8::null(scope).into());
    let names = keys
      .iter()
      .map(|key| {
        v8::String::new_from_one_byte(
          scope,
          key.as_bytes(),
          v8::NewStringType::Internalized,
        )
        .unwrap()
        .into()
      })
      .collect();
    Self { names, proto }
  }

  /// An object with the record's keys and `values`, in that order.
  pub fn build(
    &self,
    scope: &mut v8::PinScope<'a, '_>,
    values: &[v8::Local<'a, v8::Value>],
  ) -> v8::Local<'a, v8::Value> {
    debug_assert_eq!(values.len(), self.names.len());
    v8::Object::with_prototype_and_properties(
      scope,
      self.proto,
      &self.names,
      values,
    )
    .into()
  }
}

/// A string, or `null` for `None` (or text V8 can't hold).
pub fn v8_str_or_null<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  value: Option<&str>,
) -> v8::Local<'a, v8::Value> {
  value
    .and_then(|s| v8::String::new(scope, s))
    .map(Into::into)
    .unwrap_or_else(|| v8::null(scope).into())
}

/// A number, or `null` for `None`.
pub fn v8_num_or_null<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  value: Option<f64>,
) -> v8::Local<'a, v8::Value> {
  match value {
    Some(n) => v8::Number::new(scope, n).into(),
    None => v8::null(scope).into(),
  }
}
