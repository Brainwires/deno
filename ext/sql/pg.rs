// Copyright 2018-2026 the Deno authors. MIT license.

//! The Postgres engine: one `PgConnection` resource per pooled connection
//! (the pool itself lives in `01_sql.js`), on `tokio-postgres` over a socket
//! this module opens itself, so the net permission is checked against the
//! host the user named and every address it resolves to before connecting,
//! and TLS is negotiated here with the runtime's rustls configuration.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use deno_core::CancelFuture;
use deno_core::CancelHandle;
use deno_core::OpState;
use deno_core::Resource;
use deno_core::ResourceId;
use deno_core::ToV8;
use deno_core::futures::TryStreamExt;
use deno_core::op2;
use deno_core::v8;
use deno_net::DefaultTlsOptions;
use deno_net::UnsafelyIgnoreCertificateErrors;
use deno_permissions::PermissionsContainer;
use deno_tls::SocketUse;
use deno_tls::TlsClientConfigOptions;
use deno_tls::TlsKeys;
use deno_tls::create_client_config;
use postgres_types::FromSql;
use postgres_types::Type;
use serde::Deserialize;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio_postgres::Client;
use tokio_postgres::NoTls;
use tokio_postgres::Row;
use tokio_postgres::SimpleQueryMessage;
use tokio_postgres::Statement;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::ClientConfig;
use tokio_rustls::rustls::pki_types::ServerName;

use crate::error::SqlError;
use crate::error::codes;
use crate::params::SqlParams;
use crate::pg_decode;
use crate::pg_decode::DecodeOptions;
use crate::rows::FLAG_BIGINT;
use crate::rows::FLAG_RAW;
use crate::rows::RowShape;
use crate::rows::result_tuple;

const API_NAME: &str = "Deno.SQL";
/// `SSLRequest`: length 8, code 1234.5679.
const SSL_REQUEST: [u8; 8] = [0, 0, 0, 8, 0x04, 0xd2, 0x16, 0x2f];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SslMode {
  Disable,
  Prefer,
  Require,
  VerifyCa,
  VerifyFull,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PgConnectArgs {
  hostname: String,
  port: u16,
  username: String,
  password: Option<String>,
  database: Option<String>,
  ssl_mode: SslMode,
  application_name: Option<String>,
  /// The startup `options` parameter (`-c name=value ...`).
  options: Option<String>,
  /// 0: no limit.
  connect_timeout_ms: u64,
  #[serde(default)]
  ca: Vec<String>,
  server_name: Option<String>,
  /// Prepared statements kept per connection; 0 runs every query as an
  /// unnamed statement (`prepare: false`, for transaction-mode poolers).
  statement_cache_size: usize,
}

#[derive(Default)]
struct StatementCache {
  map: HashMap<String, Statement>,
  order: VecDeque<String>,
}

pub struct PgConnection {
  client: Client,
  statements: RefCell<StatementCache>,
  cache_size: usize,
  cancel: Rc<CancelHandle>,
}

impl Resource for PgConnection {
  fn name(&self) -> Cow<'_, str> {
    "sqlPostgresConnection".into()
  }

  fn close(self: Rc<Self>) {
    // Fails every in-flight query on this connection; the client (and so the
    // socket, after a Terminate) goes away with the last reference.
    self.cancel.cancel();
  }
}

impl PgConnection {
  async fn statement(&self, sql: &str) -> Result<Statement, SqlError> {
    if let Some(stmt) = self.statements.borrow().map.get(sql) {
      return Ok(stmt.clone());
    }
    let stmt = self.client.prepare(sql).await?;
    let mut cache = self.statements.borrow_mut();
    if !cache.map.contains_key(sql) {
      while cache.map.len() >= self.cache_size {
        match cache.order.pop_front() {
          Some(old) => {
            cache.map.remove(&old);
          }
          None => break,
        }
      }
      cache.order.push_back(sql.to_string());
      cache.map.insert(sql.to_string(), stmt.clone());
    }
    Ok(stmt)
  }
}

fn tls_config(
  state: &OpState,
  args: &PgConnectArgs,
) -> Result<Option<Arc<ClientConfig>>, SqlError> {
  let verify = match args.ssl_mode {
    SslMode::Disable => return Ok(None),
    SslMode::Prefer | SslMode::Require => false,
    SslMode::VerifyCa | SslMode::VerifyFull => true,
  };
  // `--unsafely-ignore-certificate-errors` applies here as it does to every
  // other TLS client in the runtime.
  let unsafely_ignore_certificate_errors = if verify {
    state
      .try_borrow::<UnsafelyIgnoreCertificateErrors>()
      .and_then(|it| it.0.clone())
  } else {
    Some(Vec::new())
  };
  let root_cert_store = state
    .try_borrow::<DefaultTlsOptions>()
    .map(|o| o.root_cert_store())
    .transpose()
    .map_err(|e| {
      SqlError::postgres_client(codes::TLS_UPGRADE_FAILED, e.to_string())
    })?
    .flatten();
  let config = create_client_config(TlsClientConfigOptions {
    root_cert_store,
    ca_certs: args.ca.iter().map(|c| c.as_bytes().to_vec()).collect(),
    unsafely_ignore_certificate_errors,
    unsafely_disable_hostname_verification: args.ssl_mode == SslMode::VerifyCa,
    cert_chain_and_key: TlsKeys::Null,
    socket_use: SocketUse::GeneralSsl,
  })
  .map_err(|e| {
    SqlError::postgres_client(codes::TLS_UPGRADE_FAILED, e.to_string())
  })?;
  Ok(Some(Arc::new(config)))
}

fn connect_failed(addr: &str, err: impl std::fmt::Display) -> SqlError {
  SqlError::postgres_client(
    codes::CONNECTION_FAILED,
    format!("Failed to connect to {addr}: {err}"),
  )
}

async fn open_socket(
  state: &Rc<RefCell<OpState>>,
  hostname: &str,
  port: u16,
) -> Result<TcpStream, SqlError> {
  let display = format!("{hostname}:{port}");
  let addrs: Vec<SocketAddr> =
    deno_net::resolve_addr::resolve_addr(hostname, port)
      .await
      .map_err(|e| connect_failed(&display, e))?
      .collect();
  if addrs.is_empty() {
    return Err(connect_failed(&display, "no addresses resolved"));
  }
  let mut last_err = None;
  for addr in addrs {
    // The host was checked as named; check each address it resolved to as
    // well (as Deno.connect does), so an IP-literal deny rule holds.
    state
      .borrow_mut()
      .borrow_mut::<PermissionsContainer>()
      .check_net_resolved(&addr.ip(), addr.port(), API_NAME)?;
    match TcpStream::connect(addr).await {
      Ok(stream) => {
        let _ = stream.set_nodelay(true);
        return Ok(stream);
      }
      Err(e) => last_err = Some(e),
    }
  }
  Err(connect_failed(
    &display,
    last_err.map(|e| e.to_string()).unwrap_or_default(),
  ))
}

enum Negotiated {
  Plain(TcpStream),
  Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

async fn negotiate_tls(
  mut stream: TcpStream,
  mode: SslMode,
  config: Option<Arc<ClientConfig>>,
  server_name: &str,
) -> Result<Negotiated, SqlError> {
  let Some(config) = config else {
    return Ok(Negotiated::Plain(stream));
  };
  let io = |e: std::io::Error| {
    SqlError::postgres_client(codes::CONNECTION_CLOSED, e.to_string())
  };
  stream.write_all(&SSL_REQUEST).await.map_err(io)?;
  // Exactly one byte: anything the server sent after it before the handshake
  // must not be read as if it came over TLS.
  let mut answer = [0u8; 1];
  stream.read_exact(&mut answer).await.map_err(io)?;
  match answer[0] {
    b'S' => {
      let name =
        ServerName::try_from(server_name.to_string()).map_err(|e| {
          SqlError::postgres_client(
            codes::TLS_UPGRADE_FAILED,
            format!("Invalid TLS server name \"{server_name}\": {e}"),
          )
        })?;
      let tls = TlsConnector::from(config)
        .connect(name, stream)
        .await
        .map_err(|e| {
          SqlError::postgres_client(
            codes::TLS_UPGRADE_FAILED,
            format!("TLS handshake failed: {e}"),
          )
        })?;
      Ok(Negotiated::Tls(Box::new(tls)))
    }
    b'N' if mode == SslMode::Prefer => Ok(Negotiated::Plain(stream)),
    b'N' => Err(SqlError::postgres_client(
      codes::TLS_NOT_AVAILABLE,
      format!(
        "The server does not support TLS connections (sslmode={})",
        match mode {
          SslMode::Require => "require",
          SslMode::VerifyCa => "verify-ca",
          _ => "verify-full",
        }
      ),
    )),
    _ => Err(SqlError::postgres_client(
      codes::PROTOCOL,
      "Unexpected response to the TLS request",
    )),
  }
}

async fn startup<S>(
  stream: S,
  config: &tokio_postgres::Config,
) -> Result<Client, SqlError>
where
  S: AsyncRead + AsyncWrite + Unpin + 'static,
{
  let (client, mut connection) = config.connect_raw(stream, NoTls).await?;
  deno_core::unsync::spawn(async move {
    // Drives the socket until the client is dropped (after sending
    // Terminate) or the socket fails, which the client sees as
    // `is_closed()`. Notices and notifications are dropped here rather than
    // logged.
    loop {
      match std::future::poll_fn(|cx| connection.poll_message(cx)).await {
        Some(Ok(_)) => {}
        Some(Err(_)) | None => break,
      }
    }
  });
  Ok(client)
}

async fn connect(
  state: Rc<RefCell<OpState>>,
  args: &PgConnectArgs,
  tls: Option<Arc<ClientConfig>>,
) -> Result<Client, SqlError> {
  let stream = open_socket(&state, &args.hostname, args.port).await?;
  let server_name = args
    .server_name
    .as_deref()
    .unwrap_or(&args.hostname)
    .trim_start_matches('[')
    .trim_end_matches(']');
  let negotiated =
    negotiate_tls(stream, args.ssl_mode, tls, server_name).await?;

  let mut config = tokio_postgres::Config::new();
  config.user(&args.username);
  if let Some(password) = &args.password {
    config.password(password);
  }
  if let Some(database) = &args.database {
    config.dbname(database);
  }
  if let Some(name) = &args.application_name {
    config.application_name(name);
  }
  if let Some(options) = &args.options {
    config.options(options);
  }
  // TLS (if any) is already in place underneath.
  config.ssl_mode(tokio_postgres::config::SslMode::Disable);

  match negotiated {
    Negotiated::Plain(s) => startup(s, &config).await,
    Negotiated::Tls(s) => startup(*s, &config).await,
  }
}

#[op2]
#[smi]
pub async fn op_sql_pg_connect(
  state: Rc<RefCell<OpState>>,
  #[serde] args: PgConnectArgs,
) -> Result<ResourceId, SqlError> {
  let tls = {
    let mut s = state.borrow_mut();
    s.borrow_mut::<PermissionsContainer>()
      .check_net(&(&args.hostname, Some(args.port)), API_NAME)?;
    tls_config(&s, &args)?
  };
  let fut = connect(state.clone(), &args, tls);
  let client = if args.connect_timeout_ms > 0 {
    tokio::time::timeout(Duration::from_millis(args.connect_timeout_ms), fut)
      .await
      .map_err(|_| {
        SqlError::postgres_client(
          codes::CONNECTION_TIMEOUT,
          format!(
            "Connection to {}:{} timed out after {}ms",
            args.hostname, args.port, args.connect_timeout_ms
          ),
        )
      })??
  } else {
    fut.await?
  };
  let rid = state.borrow_mut().resource_table.add(PgConnection {
    client,
    statements: RefCell::new(StatementCache::default()),
    cache_size: args.statement_cache_size,
    cancel: CancelHandle::new_rc(),
  });
  Ok(rid)
}

#[op2(fast)]
pub fn op_sql_pg_is_closed(state: &OpState, #[smi] rid: ResourceId) -> bool {
  state
    .resource_table
    .get::<PgConnection>(rid)
    .map(|c| c.client.is_closed())
    .unwrap_or(true)
}

/// A column value exactly as the server sent it (binary format).
struct RawCell<'r>(Option<&'r [u8]>);

impl<'r> FromSql<'r> for RawCell<'r> {
  fn from_sql(
    _: &Type,
    raw: &'r [u8],
  ) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
    Ok(RawCell(Some(raw)))
  }

  fn from_sql_null(
    _: &Type,
  ) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
    Ok(RawCell(None))
  }

  fn accepts(_: &Type) -> bool {
    true
  }
}

pub struct PgRows {
  columns: Vec<(String, Type)>,
  rows: Vec<Row>,
  affected: Option<u64>,
  flags: u32,
}

impl<'a> ToV8<'a> for PgRows {
  type Error = SqlError;

  fn to_v8<'i>(
    self,
    scope: &mut v8::PinScope<'a, 'i>,
  ) -> Result<v8::Local<'a, v8::Value>, Self::Error> {
    let opts = DecodeOptions {
      bigint: self.flags & FLAG_BIGINT != 0,
    };
    let raw = self.flags & FLAG_RAW != 0;
    let names: Vec<&str> =
      self.columns.iter().map(|(n, _)| n.as_str()).collect();
    let shape = RowShape::new(scope, &names, self.flags)
      .map_err(|e| SqlError::postgres_client(codes::PROTOCOL, e))?;
    let mut out = Vec::with_capacity(self.rows.len());
    let mut cells = Vec::with_capacity(self.columns.len());
    for row in &self.rows {
      cells.clear();
      for (i, (name, ty)) in self.columns.iter().enumerate() {
        let cell: RawCell = row.try_get(i)?;
        cells.push(match cell.0 {
          None => v8::null(scope).into(),
          Some(bytes) if raw => pg_decode::v8_bytes(scope, bytes),
          Some(bytes) => {
            pg_decode::decode(scope, ty, bytes, opts).map_err(|e| {
              SqlError::postgres_client(
                codes::PROTOCOL,
                format!(
                  "Could not decode column \"{name}\" of type {}: {e}",
                  ty.name()
                ),
              )
            })?
          }
        });
      }
      out.push(shape.build(scope, &cells));
    }
    let count = self.affected.unwrap_or(self.rows.len() as u64) as f64;
    let undefined = v8::undefined(scope).into();
    Ok(result_tuple(scope, &out, count, undefined))
  }
}

fn contains_nul(p: &crate::params::SqlParam) -> bool {
  use crate::params::SqlParam;
  match p {
    SqlParam::Text(s) | SqlParam::Json(s) => s.contains('\0'),
    SqlParam::Array(items) => items.iter().any(contains_nul),
    _ => false,
  }
}

fn unspecified_type() -> Type {
  // OID 0: the server infers the parameter's type, as for a prepared
  // statement.
  Type::new(
    "unspecified".to_string(),
    0,
    postgres_types::Kind::Pseudo,
    "pg_catalog".to_string(),
  )
}

fn closed() -> SqlError {
  SqlError::postgres_client(codes::CONNECTION_CLOSED, "Connection closed")
}

#[op2]
pub async fn op_sql_pg_query(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[string] sql: String,
  #[scoped] params: SqlParams,
  #[smi] flags: u32,
) -> Result<PgRows, SqlError> {
  let conn = state.borrow().resource_table.get::<PgConnection>(rid)?;
  if let Some(index) = params.0.iter().position(contains_nul) {
    return Err(SqlError::postgres_client(
      codes::INVALID_PARAMETER,
      format!(
        "Parameter ${} contains a NUL (\\0) character, which Postgres text can't hold",
        index + 1
      ),
    ));
  }
  let cancel = conn.cancel.clone();
  let run = async {
    let (stream, statement) = if conn.cache_size == 0 {
      let unspecified = unspecified_type();
      let typed = params.0.iter().map(|p| (p, unspecified.clone()));
      (conn.client.query_typed_raw(&sql, typed).await?, None)
    } else {
      let stmt = conn.statement(&sql).await?;
      if stmt.params().len() != params.0.len() {
        return Err(SqlError::postgres_client(
          codes::INVALID_PARAMETER,
          format!(
            "The query expects {} parameter(s) but {} were given",
            stmt.params().len(),
            params.0.len()
          ),
        ));
      }
      (
        conn.client.query_raw(&stmt, params.0.iter()).await?,
        Some(stmt),
      )
    };
    let mut stream = std::pin::pin!(stream);
    let mut rows = Vec::new();
    while let Some(row) = stream.try_next().await? {
      rows.push(row);
    }
    let affected = stream.rows_affected();
    let columns = match (rows.first(), &statement) {
      (Some(row), _) => row.columns(),
      (None, Some(stmt)) => stmt.columns(),
      (None, None) => &[],
    }
    .iter()
    .map(|c| (c.name().to_string(), c.type_().clone()))
    .collect();
    Ok(PgRows {
      columns,
      rows,
      affected,
      flags,
    })
  };
  match run.or_cancel(cancel).await {
    Ok(result) => result,
    Err(_) => Err(closed()),
  }
}

/// One statement's result over the simple protocol: column names, rows of
/// text values, and the row count.
type SimpleResult = (Vec<String>, Vec<Vec<Option<String>>>, u64);

pub struct PgSimpleResults {
  results: Vec<SimpleResult>,
  flags: u32,
}

impl<'a> ToV8<'a> for PgSimpleResults {
  type Error = SqlError;

  fn to_v8<'i>(
    self,
    scope: &mut v8::PinScope<'a, 'i>,
  ) -> Result<v8::Local<'a, v8::Value>, Self::Error> {
    let mut out = Vec::with_capacity(self.results.len());
    for (columns, rows, count) in &self.results {
      let shape = RowShape::new(scope, columns, self.flags)
        .map_err(|e| SqlError::postgres_client(codes::PROTOCOL, e))?;
      let mut built = Vec::with_capacity(rows.len());
      for row in rows {
        let cells: Vec<v8::Local<v8::Value>> = row
          .iter()
          .map(|cell| match cell {
            Some(s) => v8::String::new(scope, s)
              .map(Into::into)
              .unwrap_or_else(|| v8::null(scope).into()),
            None => v8::null(scope).into(),
          })
          .collect();
        built.push(shape.build(scope, &cells));
      }
      let undefined = v8::undefined(scope).into();
      out.push(result_tuple(scope, &built, *count as f64, undefined));
    }
    Ok(v8::Array::new_with_elements(scope, &out).into())
  }
}

/// The simple query protocol: no parameters, any number of statements, every
/// value as the server's text.
#[op2]
pub async fn op_sql_pg_simple(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[string] sql: String,
  #[smi] flags: u32,
) -> Result<PgSimpleResults, SqlError> {
  let conn = state.borrow().resource_table.get::<PgConnection>(rid)?;
  let cancel = conn.cancel.clone();
  let run = async {
    let messages = conn.client.simple_query(&sql).await?;
    let mut results = Vec::new();
    let mut columns: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    for message in messages {
      match message {
        SimpleQueryMessage::RowDescription(cols) => {
          columns = cols.iter().map(|c| c.name().to_string()).collect();
        }
        SimpleQueryMessage::Row(row) => {
          rows.push(
            (0..row.len())
              .map(|i| row.get(i).map(str::to_string))
              .collect(),
          );
        }
        SimpleQueryMessage::CommandComplete(n) => {
          results.push((
            std::mem::take(&mut columns),
            std::mem::take(&mut rows),
            n,
          ));
        }
        _ => {}
      }
    }
    Ok(PgSimpleResults { results, flags })
  };
  match run.or_cancel(cancel).await {
    Ok(result) => result,
    Err(_) => Err(closed()),
  }
}
