// Copyright 2018-2026 the Deno authors. MIT license.

//! The SQLite engine behind the same `Deno.SQL` surface, on the runtime's
//! bundled SQLite (the `rusqlite` that `node:sqlite` uses). Opening a file
//! needs read (and, unless read-only, write) permission for it; without full
//! permissions ATTACH is held off, as `node:sqlite` does, so a query can't
//! reach other files.

use std::borrow::Cow;
use std::cell::RefCell;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;

use deno_core::OpState;
use deno_core::Resource;
use deno_core::ResourceId;
use deno_core::op2;
use deno_core::v8;
use deno_permissions::OpenAccessKind;
use deno_permissions::PermissionsContainer;
use rusqlite::OpenFlags;
use rusqlite::config::DbConfig;
use rusqlite::limits::Limit;
use rusqlite::types::ToSqlOutput;
use rusqlite::types::Value;
use rusqlite::types::ValueRef;

use crate::error::SqlError;
use crate::params::SqlParam;
use crate::params::SqlParams;
use crate::params::date_to_iso;
use crate::params::json_text;
use crate::pg_decode::v8_bytes;
use crate::rows::FLAG_BIGINT;
use crate::rows::FLAG_RAW;
use crate::rows::Record;
use crate::rows::RowShape;
use crate::rows::result_tuple;
use crate::rows::v8_str_or_null;

const API_NAME: &str = "Deno.SQL";
const MAX_SAFE_JS_INTEGER: i64 = 9_007_199_254_740_991;
const STATEMENT_CACHE_CAPACITY: usize = 128;

pub struct SqliteConnection {
  conn: RefCell<Option<rusqlite::Connection>>,
}

impl Resource for SqliteConnection {
  fn name(&self) -> Cow<'_, str> {
    "sqlSqliteConnection".into()
  }

  fn close(self: Rc<Self>) {
    self.conn.borrow_mut().take();
  }
}

fn closed() -> SqlError {
  SqlError::Sqlite {
    message: "Connection closed".into(),
    props: vec![(
      "code",
      deno_error::PropertyValue::String(Cow::Borrowed("ERR_SQLITE_CLOSED")),
    )],
  }
}

/// SQLITE_OPEN_NOFOLLOW refuses a symlink anywhere in the path; macOS keeps
/// its temporary directories behind fixed root-owned aliases (`/tmp`,
/// `/var`), so resolve only those, as `node:sqlite` does.
fn resolve_system_path_alias(path: &Path) -> PathBuf {
  #[cfg(target_os = "macos")]
  for prefix in [Path::new("/var"), Path::new("/tmp")] {
    if let Ok(suffix) = path.strip_prefix(prefix) {
      #[allow(
        clippy::disallowed_methods,
        reason = "SQLite opens the real file system path"
      )]
      if let Ok(prefix) = std::fs::canonicalize(prefix) {
        return prefix.join(suffix);
      }
    }
  }
  path.to_path_buf()
}

fn harden(
  conn: &rusqlite::Connection,
  disable_attach: bool,
) -> Result<(), SqlError> {
  conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
  // Extension loading stays at SQLite's default (off): nothing here turns
  // it on, and it would need --allow-ffi.
  if disable_attach {
    // ATTACH (and VACUUM INTO, which attaches) could open files outside the
    // permission checked here.
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_WRITE, false)?;
    conn.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)?;
  }
  conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE_CAPACITY);
  Ok(())
}

// `nofast`: a `SqlError` is built by a JavaScript builder (see `error.rs`).
#[op2(nofast, stack_trace)]
#[smi]
pub fn op_sql_sqlite_open(
  state: &mut OpState,
  #[string] path: String,
  readonly: bool,
  create: bool,
) -> Result<ResourceId, SqlError> {
  let perms = state.borrow::<PermissionsContainer>();
  let disable_attach =
    perms.check_has_all_permissions(Path::new(&path)).is_err();
  let conn = if path == ":memory:" || path.is_empty() {
    rusqlite::Connection::open_in_memory()?
  } else {
    let checked = perms
      .check_open(
        Cow::Borrowed(Path::new(&path)),
        if readonly {
          OpenAccessKind::ReadNoFollow
        } else {
          OpenAccessKind::ReadWriteNoFollow
        },
        Some(API_NAME),
      )?
      .into_path();
    let location = resolve_system_path_alias(&checked);
    let mut flags =
      OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    if readonly {
      flags |= OpenFlags::SQLITE_OPEN_READ_ONLY;
    } else {
      flags |= OpenFlags::SQLITE_OPEN_READ_WRITE;
      if create {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
      }
    }
    rusqlite::Connection::open_with_flags(&location, flags)?
  };
  harden(&conn, disable_attach)?;
  Ok(state.resource_table.add(SqliteConnection {
    conn: RefCell::new(Some(conn)),
  }))
}

struct Bind<'p>(&'p SqlParam);

impl rusqlite::ToSql for Bind<'_> {
  fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
    let conversion = |msg: String| {
      rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(
        msg,
      )))
    };
    Ok(match self.0 {
      SqlParam::Null => ToSqlOutput::Owned(Value::Null),
      SqlParam::Bool(b) => ToSqlOutput::Owned(Value::Integer(*b as i64)),
      SqlParam::Number(n) => {
        if n.fract() == 0.0 && n.abs() <= MAX_SAFE_JS_INTEGER as f64 {
          ToSqlOutput::Owned(Value::Integer(*n as i64))
        } else {
          ToSqlOutput::Owned(Value::Real(*n))
        }
      }
      SqlParam::BigInt(s) => {
        ToSqlOutput::Owned(Value::Integer(s.parse::<i64>().map_err(|_| {
          conversion(format!("bigint {s} does not fit in 64 bits"))
        })?))
      }
      SqlParam::Text(s) | SqlParam::Json(s) => {
        ToSqlOutput::Borrowed(ValueRef::Text(s.as_bytes()))
      }
      SqlParam::Bytes(b) => ToSqlOutput::Borrowed(ValueRef::Blob(b)),
      SqlParam::Date(ms, _) => ToSqlOutput::Owned(Value::Text(
        date_to_iso(*ms).ok_or_else(|| conversion("Invalid Date".into()))?,
      )),
      SqlParam::Array(_) => ToSqlOutput::Owned(Value::Text(
        json_text(self.0).map_err(|e| conversion(e.to_string()))?,
      )),
    })
  }
}

fn to_v8<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  value: ValueRef<'_>,
  flags: u32,
) -> v8::Local<'a, v8::Value> {
  match value {
    ValueRef::Null => v8::null(scope).into(),
    ValueRef::Integer(i) => {
      if flags & FLAG_BIGINT != 0 {
        v8::BigInt::new_from_i64(scope, i).into()
      } else if i.abs() <= MAX_SAFE_JS_INTEGER {
        v8::Number::new(scope, i as f64).into()
      } else {
        // Beyond 2^53 a number would silently round: the decimal digits,
        // as Postgres' int8 arrives without `bigint: true`.
        v8::String::new(scope, &i.to_string()).unwrap().into()
      }
    }
    ValueRef::Real(f) => v8::Number::new(scope, f).into(),
    ValueRef::Text(t) if flags & FLAG_RAW != 0 => v8_bytes(scope, t),
    ValueRef::Text(t) => {
      v8::String::new_from_utf8(scope, t, v8::NewStringType::Normal)
        .map(Into::into)
        .unwrap_or_else(|| v8::null(scope).into())
    }
    ValueRef::Blob(b) => v8_bytes(scope, b),
  }
}

/// The keys of a SQLite column's metadata object (`result.columns`), as
/// `node:sqlite`'s `StatementSync#columns()` names them, plus `nullable`.
const SQLITE_COLUMN_KEYS: &[&str] =
  &["name", "type", "nullable", "table", "column", "database"];

/// The metadata of every result column of `stmt`. A column read straight
/// from a table (or through a view or subquery) names its database, table
/// and column, its declared type and whether that column is declared NOT
/// NULL; an expression has `null` for all of them, as SQLite reports.
fn column_objects<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  stmt: &rusqlite::Statement<'_>,
  names: &[String],
) -> Vec<v8::Local<'a, v8::Value>> {
  let record = Record::new(scope, SQLITE_COLUMN_KEYS);
  let mut out = Vec::with_capacity(names.len());
  for (i, name) in names.iter().enumerate() {
    // An error (the table was dropped since) reads as an expression.
    let meta = stmt.column_metadata(i).ok().flatten();
    let text = |s: &std::ffi::CStr| s.to_string_lossy().into_owned();
    let (database, table, column, decl_type, not_null) = match meta {
      Some((database, table, column, decl_type, _, not_null, _, _)) => (
        Some(text(database)),
        Some(text(table)),
        Some(text(column)),
        decl_type.map(text),
        Some(not_null),
      ),
      None => (None, None, None, None, None),
    };
    let nullable: v8::Local<v8::Value> = match not_null {
      Some(not_null) => v8::Boolean::new(scope, !not_null).into(),
      None => v8::null(scope).into(),
    };
    let values = [
      v8_str_or_null(scope, Some(name.as_str())),
      v8_str_or_null(scope, decl_type.as_deref()),
      nullable,
      v8_str_or_null(scope, table.as_deref()),
      v8_str_or_null(scope, column.as_deref()),
      v8_str_or_null(scope, database.as_deref()),
    ];
    out.push(record.build(scope, &values));
  }
  out
}

#[op2]
pub fn op_sql_sqlite_query<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[string] sql: &str,
  #[scoped] params: SqlParams,
  #[smi] flags: u32,
) -> Result<v8::Local<'a, v8::Value>, SqlError> {
  let resource = state.borrow().resource_table.get::<SqliteConnection>(rid)?;
  let guard = resource.conn.borrow();
  let conn = guard.as_ref().ok_or_else(closed)?;
  let mut stmt = conn.prepare_cached(sql)?;
  let expected = stmt.parameter_count();
  if expected != params.0.len() {
    return Err(SqlError::Sqlite {
      message: format!(
        "The query expects {expected} parameter(s) but {} were given",
        params.0.len()
      ),
      props: vec![(
        "code",
        deno_error::PropertyValue::String(Cow::Borrowed("SQLITE_RANGE")),
      )],
    });
  }
  for (i, p) in params.0.iter().enumerate() {
    stmt.raw_bind_parameter(i + 1, Bind(p))?;
  }
  let names: Vec<String> =
    stmt.column_names().iter().map(|s| s.to_string()).collect();
  let shape = RowShape::new(scope, &names, flags).map_err(SqlError::Type)?;
  let readonly = stmt.readonly();
  let ncols = names.len();
  let mut out = Vec::new();
  {
    let mut rows = stmt.raw_query();
    let mut cells = Vec::with_capacity(ncols);
    while let Some(row) = rows.next()? {
      cells.clear();
      for i in 0..ncols {
        cells.push(to_v8(scope, row.get_ref(i)?, flags));
      }
      out.push(shape.build(scope, &cells));
    }
  }
  let columns = column_objects(scope, &stmt, &names);
  let count = if readonly {
    out.len() as f64
  } else {
    conn.changes() as f64
  };
  let last_id = conn.last_insert_rowid();
  let last_id = if flags & FLAG_BIGINT != 0 {
    v8::BigInt::new_from_i64(scope, last_id).into()
  } else {
    v8::Number::new(scope, last_id as f64).into()
  };
  Ok(result_tuple(scope, &out, count, last_id, &columns))
}

/// Any number of statements, no parameters, no rows (transaction control,
/// `sql.unsafe(...).simple()`, migrations).
// `nofast`: a `SqlError` is built by a JavaScript builder (see `error.rs`).
#[op2(nofast)]
pub fn op_sql_sqlite_exec(
  state: &OpState,
  #[smi] rid: ResourceId,
  #[string] sql: &str,
) -> Result<(), SqlError> {
  let resource = state.resource_table.get::<SqliteConnection>(rid)?;
  let guard = resource.conn.borrow();
  let conn = guard.as_ref().ok_or_else(closed)?;
  conn.execute_batch(sql)?;
  Ok(())
}
