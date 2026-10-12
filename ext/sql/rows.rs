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

/// The op result: `[rows, count, extra]`.
pub fn result_tuple<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  rows: &[v8::Local<'a, v8::Value>],
  count: f64,
  extra: v8::Local<'a, v8::Value>,
) -> v8::Local<'a, v8::Value> {
  let rows = v8::Array::new_with_elements(scope, rows);
  let count = v8::Number::new(scope, count);
  v8::Array::new_with_elements(scope, &[rows.into(), count.into(), extra])
    .into()
}
