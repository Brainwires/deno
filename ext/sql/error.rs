// Copyright 2018-2026 the Deno authors. MIT license.

use std::borrow::Cow;

use deno_error::AdditionalProperties;
use deno_error::JsErrorClass;
use deno_error::PropertyValue;

/// The class name `01_sql.js` registers `SQL.PostgresError` under.
pub const POSTGRES_ERROR_CLASS: &str = "DenoSqlPostgresError";
/// The class name `01_sql.js` registers `SQL.SQLiteError` under.
pub const SQLITE_ERROR_CLASS: &str = "DenoSqlSQLiteError";

/// Client-side error codes (the server's own errors carry their SQLSTATE).
pub mod codes {
  pub const CONNECTION_CLOSED: &str = "ERR_POSTGRES_CONNECTION_CLOSED";
  pub const CONNECTION_TIMEOUT: &str = "ERR_POSTGRES_CONNECTION_TIMEOUT";
  pub const CONNECTION_FAILED: &str = "ERR_POSTGRES_CONNECTION_FAILED";
  pub const TLS_NOT_AVAILABLE: &str = "ERR_POSTGRES_TLS_NOT_AVAILABLE";
  pub const TLS_UPGRADE_FAILED: &str = "ERR_POSTGRES_TLS_UPGRADE_FAILED";
  pub const PROTOCOL: &str = "ERR_POSTGRES_PROTOCOL_ERROR";
  pub const INVALID_PARAMETER: &str = "ERR_POSTGRES_INVALID_PARAMETER";
  pub const SQLITE: &str = "ERR_SQLITE_ERROR";
}

pub type Props = Vec<(&'static str, PropertyValue)>;

#[derive(Debug, thiserror::Error)]
pub enum SqlError {
  #[error(transparent)]
  Permission(#[from] deno_permissions::PermissionCheckError),
  #[error(transparent)]
  Resource(#[from] deno_core::error::ResourceError),
  /// Reading a file (`sql.file`, `sslcert`, ...) failed.
  #[error(transparent)]
  Io(std::io::Error),
  /// A `SQL.PostgresError`: a server error (SQLSTATE in `code`) or a
  /// client-side failure (an `ERR_POSTGRES_*` code).
  #[error("{message}")]
  Postgres { message: String, props: Props },
  /// A `SQL.SQLiteError`.
  #[error("{message}")]
  Sqlite { message: String, props: Props },
  #[error("{0}")]
  Type(String),
}

impl SqlError {
  pub fn postgres_client(
    code: &'static str,
    message: impl Into<String>,
  ) -> Self {
    SqlError::Postgres {
      message: message.into(),
      props: vec![("code", PropertyValue::String(Cow::Borrowed(code)))],
    }
  }

  pub fn type_error(message: impl Into<String>) -> Self {
    SqlError::Type(message.into())
  }
}

impl From<rusqlite::Error> for SqlError {
  fn from(err: rusqlite::Error) -> Self {
    let (extended, offset) = match &err {
      rusqlite::Error::SqliteFailure(e, _) => (Some(e.extended_code), None),
      rusqlite::Error::SqlInputError { error, offset, .. } => {
        (Some(error.extended_code), Some(*offset))
      }
      _ => (None, None),
    };
    let code = extended.map(sqlite_code_name).unwrap_or(codes::SQLITE);
    let mut props: Props =
      vec![("code", PropertyValue::String(Cow::Borrowed(code)))];
    if let Some(extended) = extended {
      props.push(("errno", PropertyValue::Number(extended as f64)));
    }
    if let Some(offset) = offset {
      props.push(("byteOffset", PropertyValue::Number(offset as f64)));
    }
    SqlError::Sqlite {
      message: err.to_string(),
      props,
    }
  }
}

/// Maps a `tokio_postgres::Error` to a `SQL.PostgresError`: the server's
/// error fields when it carries an `ErrorResponse`, otherwise a client code.
impl From<tokio_postgres::Error> for SqlError {
  fn from(err: tokio_postgres::Error) -> Self {
    if let Some(db) = err.as_db_error() {
      return from_db_error(db);
    }
    let code = if err.is_closed() {
      codes::CONNECTION_CLOSED
    } else {
      codes::PROTOCOL
    };
    let mut message = err.to_string();
    if let Some(source) = std::error::Error::source(&err) {
      let source = source.to_string();
      if !message.contains(&source) {
        message = format!("{message}: {source}");
      }
    }
    SqlError::postgres_client(code, message)
  }
}

fn from_db_error(db: &tokio_postgres::error::DbError) -> SqlError {
  fn s(v: &str) -> PropertyValue {
    PropertyValue::String(Cow::Owned(v.to_string()))
  }
  let code = db.code().code();
  let mut props: Props = vec![
    ("code", s(code)),
    ("errno", s(code)),
    ("severity", s(db.severity())),
  ];
  let mut opt = |name: &'static str, v: Option<&str>| {
    if let Some(v) = v {
      props.push((name, s(v)));
    }
  };
  opt("detail", db.detail());
  opt("hint", db.hint());
  opt("where", db.where_());
  opt("schema", db.schema());
  opt("table", db.table());
  opt("column", db.column());
  opt("dataType", db.datatype());
  opt("constraint", db.constraint());
  opt("file", db.file());
  opt("routine", db.routine());
  if let Some(line) = db.line() {
    props.push(("line", PropertyValue::String(Cow::Owned(line.to_string()))));
  }
  match db.position() {
    Some(tokio_postgres::error::ErrorPosition::Original(p)) => {
      props.push(("position", PropertyValue::Number(*p as f64)));
    }
    Some(tokio_postgres::error::ErrorPosition::Internal {
      position,
      query,
    }) => {
      props.push(("internalPosition", PropertyValue::Number(*position as f64)));
      props.push(("internalQuery", s(query)));
    }
    None => {}
  }
  SqlError::Postgres {
    message: db.message().to_string(),
    props,
  }
}

impl JsErrorClass for SqlError {
  fn get_class(&self) -> Cow<'static, str> {
    match self {
      SqlError::Permission(e) => e.get_class(),
      SqlError::Resource(e) => e.get_class(),
      SqlError::Io(e) => e.get_class(),
      SqlError::Postgres { .. } => Cow::Borrowed(POSTGRES_ERROR_CLASS),
      SqlError::Sqlite { .. } => Cow::Borrowed(SQLITE_ERROR_CLASS),
      SqlError::Type(_) => Cow::Borrowed("TypeError"),
    }
  }

  fn get_message(&self) -> Cow<'static, str> {
    match self {
      SqlError::Permission(e) => e.get_message(),
      SqlError::Resource(e) => e.get_message(),
      SqlError::Io(e) => e.get_message(),
      SqlError::Postgres { message, .. } | SqlError::Sqlite { message, .. } => {
        Cow::Owned(message.clone())
      }
      SqlError::Type(m) => Cow::Owned(m.clone()),
    }
  }

  fn get_additional_properties(&self) -> AdditionalProperties {
    match self {
      SqlError::Permission(e) => e.get_additional_properties(),
      SqlError::Resource(e) => e.get_additional_properties(),
      SqlError::Io(e) => e.get_additional_properties(),
      SqlError::Postgres { props, .. } | SqlError::Sqlite { props, .. } => {
        Box::new(
          props
            .clone()
            .into_iter()
            .map(|(k, v)| (Cow::Borrowed(k), v)),
        )
      }
      SqlError::Type(_) => Box::new(std::iter::empty()),
    }
  }

  fn get_ref(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
    self
  }
}

/// The `SQLITE_*` name of a (possibly extended) SQLite result code.
pub fn sqlite_code_name(extended: std::ffi::c_int) -> &'static str {
  match extended {
    275 => "SQLITE_CONSTRAINT_CHECK",
    787 => "SQLITE_CONSTRAINT_FOREIGNKEY",
    1299 => "SQLITE_CONSTRAINT_NOTNULL",
    1555 => "SQLITE_CONSTRAINT_PRIMARYKEY",
    2067 => "SQLITE_CONSTRAINT_UNIQUE",
    _ => match extended & 0xff {
      1 => "SQLITE_ERROR",
      2 => "SQLITE_INTERNAL",
      3 => "SQLITE_PERM",
      4 => "SQLITE_ABORT",
      5 => "SQLITE_BUSY",
      6 => "SQLITE_LOCKED",
      7 => "SQLITE_NOMEM",
      8 => "SQLITE_READONLY",
      9 => "SQLITE_INTERRUPT",
      10 => "SQLITE_IOERR",
      11 => "SQLITE_CORRUPT",
      12 => "SQLITE_NOTFOUND",
      13 => "SQLITE_FULL",
      14 => "SQLITE_CANTOPEN",
      15 => "SQLITE_PROTOCOL",
      17 => "SQLITE_SCHEMA",
      18 => "SQLITE_TOOBIG",
      19 => "SQLITE_CONSTRAINT",
      20 => "SQLITE_MISMATCH",
      21 => "SQLITE_MISUSE",
      23 => "SQLITE_AUTH",
      25 => "SQLITE_RANGE",
      26 => "SQLITE_NOTADB",
      _ => codes::SQLITE,
    },
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn sqlite_code_names() {
    assert_eq!(sqlite_code_name(2067), "SQLITE_CONSTRAINT_UNIQUE");
    assert_eq!(sqlite_code_name(19), "SQLITE_CONSTRAINT");
    // SQLITE_BUSY_RECOVERY falls back to its primary code.
    assert_eq!(sqlite_code_name(261), "SQLITE_BUSY");
    assert_eq!(sqlite_code_name(256), codes::SQLITE);
  }

  #[test]
  fn client_errors_carry_codes() {
    let err = SqlError::postgres_client(codes::CONNECTION_TIMEOUT, "late");
    assert_eq!(err.get_class(), POSTGRES_ERROR_CLASS);
    assert_eq!(err.get_message(), "late");
    let props: Vec<_> = err.get_additional_properties().collect();
    assert_eq!(
      props,
      vec![(
        Cow::Borrowed("code"),
        PropertyValue::String(Cow::Borrowed(codes::CONNECTION_TIMEOUT))
      )]
    );
    assert_eq!(SqlError::type_error("x").get_class(), "TypeError");
  }
}
