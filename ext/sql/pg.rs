// Copyright 2018-2026 the Deno authors. MIT license.

//! The Postgres engine: one `PgConnection` resource per pooled connection
//! (the pool itself lives in `01_sql.js`), on `tokio-postgres` over a socket
//! this module opens itself, so the net permission is checked against the
//! host the user named and every address it resolves to before connecting,
//! and TLS is negotiated here with the runtime's rustls configuration.

use std::borrow::Cow;
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use deno_core::AsyncRefCell;
use deno_core::CancelFuture;
use deno_core::CancelHandle;
use deno_core::OpState;
use deno_core::RcRef;
use deno_core::Resource;
use deno_core::ResourceId;
use deno_core::ToV8;
use deno_core::futures::TryStreamExt;
use deno_core::futures::future::Either;
use deno_core::op2;
use deno_core::v8;
use deno_net::DefaultTlsOptions;
use deno_net::UnsafelyIgnoreCertificateErrors;
use deno_permissions::OpenAccessKind;
use deno_permissions::PermissionsContainer;
use deno_tls::SocketUse;
use deno_tls::TlsClientConfigOptions;
use deno_tls::TlsKey;
use deno_tls::TlsKeys;
use deno_tls::create_client_config;
use deno_tls::load_certs;
use deno_tls::load_private_keys;
use postgres_types::FromSql;
use postgres_types::Type;
use serde::Deserialize;
use serde::Serialize;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio::sync::mpsc;
use tokio_postgres::AsyncMessage;
use tokio_postgres::CancelToken;
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
use crate::rows::FLAG_GUARDED;
use crate::rows::FLAG_ORDERED;
use crate::rows::FLAG_RAW;
use crate::rows::FLAG_UNNAMED;
use crate::rows::RowShape;
use crate::rows::result_tuple;

const API_NAME: &str = "Deno.SQL";
/// `SSLRequest`: length 8, code 1234.5679.
const SSL_REQUEST: [u8; 8] = [0, 0, 0, 8, 0x04, 0xd2, 0x16, 0x2f];
/// How long opening the side connection for a cancel request may take.
const CANCEL_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Forward the server's notices to `op_sql_pg_next_event`.
const EVENTS_NOTICES: u32 = 1;
/// Forward `LISTEN` notifications to `op_sql_pg_next_event`.
const EVENTS_NOTIFICATIONS: u32 = 2;

/// Server parameters reported to `onconnect`.
const REPORTED_PARAMETERS: &[&str] = &[
  "server_version",
  "server_encoding",
  "client_encoding",
  "application_name",
  "DateStyle",
  "IntervalStyle",
  "TimeZone",
  "integer_datetimes",
  "standard_conforming_strings",
  "is_superuser",
  "session_authorization",
  "in_hot_standby",
];

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
  /// A Unix-domain socket to connect to instead of `hostname:port`
  /// (`<directory>/.s.PGSQL.<port>`).
  socket_path: Option<String>,
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
  /// A client certificate chain and its key (PEM), sent when the server
  /// asks for one.
  cert: Option<String>,
  key: Option<String>,
  /// Prepared statements kept per connection; 0 runs every query as an
  /// unnamed statement (`prepare: false`, for transaction-mode poolers).
  statement_cache_size: usize,
  /// `EVENTS_*` bits: which asynchronous messages to keep for
  /// `op_sql_pg_next_event` (the rest are dropped).
  #[serde(default)]
  events: u32,
}

/// What `op_sql_pg_connect` returns: the connection, and the server
/// parameters it reported at startup.
#[derive(Serialize)]
pub struct PgConnectInfo {
  rid: ResourceId,
  parameters: HashMap<String, String>,
}

/// An asynchronous message from the server: a `LISTEN` notification or a
/// notice.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum PgEvent {
  Notification {
    channel: String,
    payload: String,
    pid: i32,
  },
  Notice(Box<PgNotice>),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PgNotice {
  message: String,
  severity: String,
  code: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  detail: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  hint: Option<String>,
  #[serde(rename = "where", skip_serializing_if = "Option::is_none")]
  where_: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  schema: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  table: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  column: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  data_type: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  constraint: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  routine: Option<String>,
}

impl From<&tokio_postgres::error::DbError> for PgNotice {
  fn from(db: &tokio_postgres::error::DbError) -> Self {
    let own = |v: Option<&str>| v.map(str::to_string);
    PgNotice {
      message: db.message().to_string(),
      severity: db.severity().to_string(),
      code: db.code().code().to_string(),
      detail: own(db.detail()),
      hint: own(db.hint()),
      where_: own(db.where_()),
      schema: own(db.schema()),
      table: own(db.table()),
      column: own(db.column()),
      data_type: own(db.datatype()),
      constraint: own(db.constraint()),
      routine: own(db.routine()),
    }
  }
}

#[derive(Default)]
struct StatementCache {
  map: HashMap<String, Statement>,
  order: VecDeque<String>,
}

/// Where a connection (and so its cancel requests) goes.
#[derive(Clone, Debug)]
enum Endpoint {
  Tcp { hostname: String, port: u16 },
  Unix(PathBuf),
}

/// The requests sent on a connection whose results have not all arrived,
/// oldest first: the server answers them in order, so the front one is the
/// one it is running. Cancel requests target that one only.
#[derive(Default)]
struct InFlight {
  next_seq: u64,
  /// `(seq, query id)`.
  queue: VecDeque<(u64, u32)>,
  /// Query ids whose op is running (cancel requests for any other id are
  /// no-ops).
  live: HashSet<u32>,
}

pub struct PgConnection {
  client: Client,
  statements: RefCell<StatementCache>,
  cache_size: usize,
  cancel: Rc<CancelHandle>,
  events: Option<AsyncRefCell<mpsc::UnboundedReceiver<PgEvent>>>,
  cancel_token: CancelToken,
  endpoint: Endpoint,
  /// The TLS configuration and server name, when the connection uses TLS
  /// (a cancel request goes over TLS too).
  tls: Option<(Arc<ClientConfig>, String)>,
  in_flight: RefCell<InFlight>,
  in_flight_changed: Notify,
  /// Held by a request from its op's first poll until its messages are
  /// queued, so requests go on the wire in the order they were made even
  /// when one has to prepare its statement first ([`FLAG_ORDERED`]: a
  /// transaction's pipelined statements rely on it). FIFO, and taken on
  /// the first poll, which happens when JavaScript calls the op. Simple
  /// queries and transaction ends take it too; unordered queries (the
  /// pool's, on connections no ordered request uses) don't.
  order: tokio::sync::Mutex<()>,
  /// A [`FLAG_GUARDED`] statement failed before it went on the wire; the
  /// next `op_sql_pg_finish` rolls back instead.
  guarded_failed: Cell<bool>,
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

/// A request in [`InFlight::queue`], removed when dropped (its results have
/// arrived, or it failed).
struct Ticket<'c> {
  conn: &'c PgConnection,
  seq: u64,
}

impl Drop for Ticket<'_> {
  fn drop(&mut self) {
    let mut in_flight = self.conn.in_flight.borrow_mut();
    if let Some(i) = in_flight.queue.iter().position(|(s, _)| *s == self.seq) {
      in_flight.queue.remove(i);
    }
    drop(in_flight);
    self.conn.in_flight_changed.notify_waiters();
  }
}

/// The op of query `id` is running (cancel requests for it are accepted)
/// until dropped.
struct LiveOp<'c> {
  conn: &'c PgConnection,
  id: u32,
}

impl Drop for LiveOp<'_> {
  fn drop(&mut self) {
    self.conn.in_flight.borrow_mut().live.remove(&self.id);
    self.conn.in_flight_changed.notify_waiters();
  }
}

impl PgConnection {
  fn evict(&self, sql: &str) {
    let mut cache = self.statements.borrow_mut();
    if cache.map.remove(sql).is_some() {
      cache.order.retain(|s| s != sql);
    }
  }

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

  fn live(&self, id: u32) -> Option<LiveOp<'_>> {
    if id == 0 {
      return None;
    }
    self.in_flight.borrow_mut().live.insert(id);
    Some(LiveOp { conn: self, id })
  }

  /// Registers a request about to be sent. Call it right before the
  /// request's future is first polled (which is when `tokio-postgres`
  /// queues its messages), so the queue order is the order on the wire.
  fn enter(&self, id: u32) -> Ticket<'_> {
    let mut in_flight = self.in_flight.borrow_mut();
    let seq = in_flight.next_seq;
    in_flight.next_seq += 1;
    in_flight.queue.push_back((seq, id));
    Ticket { conn: self, seq }
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
  let tls_error =
    |e: String| SqlError::postgres_client(codes::TLS_UPGRADE_FAILED, e);
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
    .map_err(|e| tls_error(e.to_string()))?
    .flatten();
  let cert_chain_and_key = match (&args.cert, &args.key) {
    (Some(cert), Some(key)) => {
      let certs = load_certs(&mut cert.as_bytes()).map_err(|e| {
        tls_error(format!("Invalid client certificate (sslcert): {e}"))
      })?;
      let key = load_private_keys(key.as_bytes())
        .map_err(|e| tls_error(format!("Invalid client key (sslkey): {e}")))?
        .into_iter()
        .next()
        .ok_or_else(|| tls_error("No private key in sslkey".into()))?;
      TlsKeys::Static(TlsKey(certs, key))
    }
    (None, None) => TlsKeys::Null,
    _ => {
      return Err(SqlError::type_error(
        "A client certificate (sslcert) needs its key (sslkey), and the other way around",
      ));
    }
  };
  let config = create_client_config(TlsClientConfigOptions {
    root_cert_store,
    ca_certs: args.ca.iter().map(|c| c.as_bytes().to_vec()).collect(),
    unsafely_ignore_certificate_errors,
    unsafely_disable_hostname_verification: args.ssl_mode == SslMode::VerifyCa,
    cert_chain_and_key,
    socket_use: SocketUse::GeneralSsl,
  })
  .map_err(|e| tls_error(e.to_string()))?;
  Ok(Some(Arc::new(config)))
}

fn connect_failed(addr: &str, err: impl std::fmt::Display) -> SqlError {
  SqlError::postgres_client(
    codes::CONNECTION_FAILED,
    format!("Failed to connect to {addr}: {err}"),
  )
}

async fn open_tcp(
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

#[cfg(unix)]
async fn open_unix(path: &std::path::Path) -> Result<UnixStream, SqlError> {
  UnixStream::connect(path)
    .await
    .map_err(|e| connect_failed(&path.display().to_string(), e))
}

#[cfg(unix)]
use tokio::net::UnixStream;

/// Checks the permissions to reach `endpoint`: `--allow-net` for the host,
/// or for a Unix socket what `Deno.connect({ transport: "unix" })` checks
/// (read and write access to the path, and `--allow-net` for it).
fn check_endpoint(
  state: &mut OpState,
  endpoint: &Endpoint,
) -> Result<(), SqlError> {
  let permissions = state.borrow_mut::<PermissionsContainer>();
  match endpoint {
    Endpoint::Tcp { hostname, port } => {
      permissions.check_net(&(hostname, Some(*port)), API_NAME)?;
    }
    Endpoint::Unix(path) => {
      deno_net::check_unix_socket_path(
        permissions,
        Cow::Borrowed(path.as_path()),
        OpenAccessKind::ReadWriteNoFollow,
        Some(API_NAME),
      )?;
    }
  }
  Ok(())
}

enum Negotiated<S> {
  Plain(S),
  Tls(Box<tokio_rustls::client::TlsStream<S>>),
}

async fn negotiate_tls<S>(
  mut stream: S,
  mode: SslMode,
  config: Option<Arc<ClientConfig>>,
  server_name: &str,
) -> Result<Negotiated<S>, SqlError>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
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

struct Started {
  client: Client,
  events: Option<mpsc::UnboundedReceiver<PgEvent>>,
  parameters: HashMap<String, String>,
}

async fn startup<S>(
  stream: S,
  config: &tokio_postgres::Config,
  events: u32,
) -> Result<Started, SqlError>
where
  S: AsyncRead + AsyncWrite + Unpin + 'static,
{
  let (client, mut connection) = config.connect_raw(stream, NoTls).await?;
  let parameters = REPORTED_PARAMETERS
    .iter()
    .filter_map(|name| {
      connection
        .parameter(name)
        .map(|v| (name.to_string(), v.to_string()))
    })
    .collect();
  let (tx, rx) = if events != 0 {
    let (tx, rx) = mpsc::unbounded_channel();
    (Some(tx), Some(rx))
  } else {
    (None, None)
  };
  deno_core::unsync::spawn(async move {
    // Drives the socket until the client is dropped (after sending
    // Terminate) or the socket fails, which the client sees as
    // `is_closed()` and the event receiver as the end of its channel.
    // Asynchronous messages nobody asked for are dropped here.
    while let Some(Ok(message)) =
      std::future::poll_fn(|cx| connection.poll_message(cx)).await
    {
      let Some(tx) = &tx else { continue };
      let event = match message {
        AsyncMessage::Notification(n) if events & EVENTS_NOTIFICATIONS != 0 => {
          PgEvent::Notification {
            channel: n.channel().to_string(),
            payload: n.payload().to_string(),
            pid: n.process_id(),
          }
        }
        AsyncMessage::Notice(notice) if events & EVENTS_NOTICES != 0 => {
          PgEvent::Notice(Box::new(PgNotice::from(&notice)))
        }
        _ => continue,
      };
      let _ = tx.send(event);
    }
  });
  Ok(Started {
    client,
    events: rx,
    parameters,
  })
}

/// The connection, the TLS configuration and server name it ended up using
/// (if any).
type Connected = (Started, Option<(Arc<ClientConfig>, String)>);

async fn connect(
  state: Rc<RefCell<OpState>>,
  args: &PgConnectArgs,
  endpoint: &Endpoint,
  tls: Option<Arc<ClientConfig>>,
) -> Result<Connected, SqlError> {
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

  match endpoint {
    Endpoint::Tcp { hostname, port } => {
      let stream = open_tcp(&state, hostname, *port).await?;
      let server_name = args
        .server_name
        .as_deref()
        .unwrap_or(hostname)
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
      match negotiate_tls(stream, args.ssl_mode, tls.clone(), &server_name)
        .await?
      {
        Negotiated::Plain(s) => {
          Ok((startup(s, &config, args.events).await?, None))
        }
        Negotiated::Tls(s) => Ok((
          startup(*s, &config, args.events).await?,
          tls.map(|t| (t, server_name)),
        )),
      }
    }
    #[cfg(unix)]
    Endpoint::Unix(path) => {
      // As libpq: no TLS over a Unix-domain socket, whatever the sslmode.
      let stream = open_unix(path).await?;
      Ok((startup(stream, &config, args.events).await?, None))
    }
    #[cfg(not(unix))]
    Endpoint::Unix(_) => Err(SqlError::type_error(
      "Unix-socket Postgres hosts are not supported on this platform",
    )),
  }
}

#[op2]
#[serde]
pub async fn op_sql_pg_connect(
  state: Rc<RefCell<OpState>>,
  #[serde] args: PgConnectArgs,
) -> Result<PgConnectInfo, SqlError> {
  let endpoint = match &args.socket_path {
    Some(path) => Endpoint::Unix(PathBuf::from(path)),
    None => Endpoint::Tcp {
      hostname: args.hostname.clone(),
      port: args.port,
    },
  };
  let tls = {
    let mut s = state.borrow_mut();
    check_endpoint(&mut s, &endpoint)?;
    if matches!(endpoint, Endpoint::Unix(_)) {
      None
    } else {
      tls_config(&s, &args)?
    }
  };
  let display = match &endpoint {
    Endpoint::Tcp { hostname, port } => format!("{hostname}:{port}"),
    Endpoint::Unix(path) => path.display().to_string(),
  };
  let fut = connect(state.clone(), &args, &endpoint, tls);
  let (started, tls) = if args.connect_timeout_ms > 0 {
    tokio::time::timeout(Duration::from_millis(args.connect_timeout_ms), fut)
      .await
      .map_err(|_| {
        SqlError::postgres_client(
          codes::CONNECTION_TIMEOUT,
          format!(
            "Connection to {display} timed out after {}ms",
            args.connect_timeout_ms
          ),
        )
      })??
  } else {
    fut.await?
  };
  let cancel_token = started.client.cancel_token();
  let rid = state.borrow_mut().resource_table.add(PgConnection {
    client: started.client,
    statements: RefCell::new(StatementCache::default()),
    cache_size: args.statement_cache_size,
    cancel: CancelHandle::new_rc(),
    events: started.events.map(AsyncRefCell::new),
    cancel_token,
    endpoint,
    tls,
    in_flight: RefCell::new(InFlight::default()),
    in_flight_changed: Notify::new(),
    order: tokio::sync::Mutex::new(()),
    guarded_failed: Cell::new(false),
  });
  Ok(PgConnectInfo {
    rid,
    parameters: started.parameters,
  })
}

#[op2(fast)]
pub fn op_sql_pg_is_closed(state: &OpState, #[smi] rid: ResourceId) -> bool {
  state
    .resource_table
    .get::<PgConnection>(rid)
    .map(|c| c.client.is_closed())
    .unwrap_or(true)
}

/// The next notification or notice of a connection opened with `events`;
/// `null` once the connection has closed.
#[op2]
#[serde]
pub async fn op_sql_pg_next_event(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
) -> Result<Option<PgEvent>, SqlError> {
  let conn = state.borrow().resource_table.get::<PgConnection>(rid)?;
  if conn.events.is_none() {
    return Ok(None);
  }
  let cancel = conn.cancel.clone();
  let events = RcRef::map(&conn, |c| c.events.as_ref().unwrap());
  let mut receiver = events.borrow_mut().await;
  match receiver.recv().or_cancel(cancel).await {
    Ok(event) => Ok(event),
    Err(_) => Ok(None),
  }
}

/// Sends a CancelRequest for the backend of `conn` over a new connection to
/// the same server (TLS when the connection uses TLS).
async fn send_cancel(
  state: &Rc<RefCell<OpState>>,
  conn: &PgConnection,
) -> Result<(), SqlError> {
  let token = conn.cancel_token.clone();
  match &conn.endpoint {
    Endpoint::Tcp { hostname, port } => {
      let stream = open_tcp(state, hostname, *port).await?;
      match &conn.tls {
        Some((config, server_name)) => {
          match negotiate_tls(
            stream,
            SslMode::Require,
            Some(config.clone()),
            server_name,
          )
          .await?
          {
            Negotiated::Tls(s) => token.cancel_query_raw(*s, NoTls).await?,
            Negotiated::Plain(s) => token.cancel_query_raw(s, NoTls).await?,
          }
        }
        None => token.cancel_query_raw(stream, NoTls).await?,
      }
    }
    #[cfg(unix)]
    Endpoint::Unix(path) => {
      token
        .cancel_query_raw(open_unix(path).await?, NoTls)
        .await?
    }
    #[cfg(not(unix))]
    Endpoint::Unix(_) => {}
  }
  Ok(())
}

/// Cancels query `id` on connection `rid` once the server is running it:
/// requests on one connection run in order, so with others ahead of it the
/// cancel request waits for its turn (a request sent earlier would cancel
/// another query). Resolves `true` when a cancel request was sent, `false`
/// when the query was not (or no longer) running.
#[op2]
pub async fn op_sql_pg_cancel(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[smi] id: u32,
) -> Result<bool, SqlError> {
  let conn = state.borrow().resource_table.get::<PgConnection>(rid)?;
  {
    // The cancel connection needs the same permissions as the connection.
    let mut s = state.borrow_mut();
    check_endpoint(&mut s, &conn.endpoint)?;
  }
  let cancel = conn.cancel.clone();
  let wait_turn = async {
    loop {
      let changed = conn.in_flight_changed.notified();
      {
        let in_flight = conn.in_flight.borrow();
        if !in_flight.live.contains(&id) {
          return false;
        }
        if matches!(in_flight.queue.front(), Some((_, front)) if *front == id) {
          return true;
        }
      }
      changed.await;
    }
  };
  match wait_turn.or_cancel(cancel).await {
    Ok(true) => {}
    _ => return Ok(false),
  }
  let send = send_cancel(&state, &conn);
  tokio::time::timeout(CANCEL_CONNECT_TIMEOUT, send)
    .await
    .map_err(|_| {
      SqlError::postgres_client(
        codes::CONNECTION_TIMEOUT,
        "Timed out sending the cancel request",
      )
    })??;
  Ok(true)
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

/// Polls `fut` once: `tokio-postgres` queues a request's messages on the
/// first poll of its future (and fails it there if it can't encode them).
async fn poll_once<F: Future>(mut fut: Pin<&mut F>) -> Poll<F::Output> {
  std::future::poll_fn(|cx| Poll::Ready(fut.as_mut().poll(cx))).await
}

#[op2]
pub async fn op_sql_pg_query(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[string] sql: String,
  #[scoped] params: SqlParams,
  #[smi] flags: u32,
  #[smi] query_id: u32,
) -> Result<PgRows, SqlError> {
  let conn = state.borrow().resource_table.get::<PgConnection>(rid)?;
  let _live = conn.live(query_id);
  let cancel = conn.cancel.clone();
  let guarded = flags & FLAG_GUARDED != 0;
  let unnamed = conn.cache_size == 0 || flags & FLAG_UNNAMED != 0;
  let ordered = flags & FLAG_ORDERED != 0;
  let run = async {
    let order = if ordered {
      Some(conn.order.lock().await)
    } else {
      None
    };
    let prepared = async {
      if let Some(index) = params.0.iter().position(contains_nul) {
        return Err(SqlError::postgres_client(
          codes::INVALID_PARAMETER,
          format!(
            "Parameter ${} contains a NUL (\\0) character, which Postgres text can't hold",
            index + 1
          ),
        ));
      }
      if unnamed {
        return Ok(None);
      }
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
      Ok(Some(stmt))
    }
    .await;
    let statement = match prepared {
      Ok(statement) => statement,
      Err(err) => {
        // A statement that fails to prepare on the server aborts the
        // transaction itself; only client-side failures need the flag, but
        // setting it for both is harmless (the transaction ends in a
        // rollback either way).
        if guarded {
          conn.guarded_failed.set(true);
        }
        return Err(err);
      }
    };
    let _ticket = conn.enter(query_id);
    let fut = match &statement {
      None => {
        let unspecified = unspecified_type();
        let typed = params.0.iter().map(move |p| (p, unspecified.clone()));
        Either::Left(conn.client.query_typed_raw(&sql, typed))
      }
      Some(stmt) => Either::Right(conn.client.query_raw(stmt, params.0.iter())),
    };
    let mut fut = std::pin::pin!(fut);
    let first = poll_once(fut.as_mut()).await;
    if guarded && matches!(first, Poll::Ready(Err(_))) {
      conn.guarded_failed.set(true);
    }
    drop(order);
    let stream = match first {
      Poll::Ready(result) => result?,
      Poll::Pending => fut.await?,
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
    Ok(Err(err)) => {
      if is_stale_statement(&err) {
        // The next call prepares the statement afresh (after ALTER TABLE,
        // DISCARD ALL, ...).
        conn.evict(&sql);
      }
      Err(err)
    }
    Ok(result) => result,
    Err(_) => Err(closed()),
  }
}

/// A cached prepared statement the server can no longer run as prepared:
/// "cached plan must not change result type" (0A000) or a statement that no
/// longer exists (26000).
fn is_stale_statement(err: &SqlError) -> bool {
  let SqlError::Postgres { props, .. } = err else {
    return false;
  };
  props.iter().any(|(k, v)| {
    *k == "code"
      && matches!(v, deno_error::PropertyValue::String(c) if c == "0A000" || c == "26000")
  })
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
  #[smi] query_id: u32,
) -> Result<PgSimpleResults, SqlError> {
  let conn = state.borrow().resource_table.get::<PgConnection>(rid)?;
  let _live = conn.live(query_id);
  let cancel = conn.cancel.clone();
  let run = async {
    let order = conn.order.lock().await;
    let _ticket = conn.enter(query_id);
    let mut fut = std::pin::pin!(conn.client.simple_query(&sql));
    let first = poll_once(fut.as_mut()).await;
    if flags & FLAG_GUARDED != 0 && matches!(first, Poll::Ready(Err(_))) {
      conn.guarded_failed.set(true);
    }
    drop(order);
    let messages = match first {
      Poll::Ready(result) => result?,
      Poll::Pending => fut.await?,
    };
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

/// Ends a pipelined transaction or savepoint right behind the statements
/// sent before it: `ok` (`COMMIT`, `RELEASE SAVEPOINT ...`), or `fail`
/// (`ROLLBACK`, `ROLLBACK TO SAVEPOINT ...`; nothing when empty) when a
/// [`FLAG_GUARDED`] statement failed before it went on the wire, which the
/// server can't know about (one it ran and failed has aborted the
/// transaction already, so `COMMIT` rolls back). Returns whether it sent
/// `ok`.
#[op2]
pub async fn op_sql_pg_finish(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[string] ok: String,
  #[string] fail: String,
) -> Result<bool, SqlError> {
  let conn = state.borrow().resource_table.get::<PgConnection>(rid)?;
  let cancel = conn.cancel.clone();
  let run = async {
    let order = conn.order.lock().await;
    let failed = conn.guarded_failed.replace(false);
    let text = if failed { &fail } else { &ok };
    if text.is_empty() {
      return Ok(false);
    }
    let _ticket = conn.enter(0);
    let mut fut = std::pin::pin!(conn.client.simple_query(text));
    let first = poll_once(fut.as_mut()).await;
    drop(order);
    match first {
      Poll::Ready(result) => result?,
      Poll::Pending => fut.await?,
    };
    Ok(!failed)
  };
  match run.or_cancel(cancel).await {
    Ok(result) => result,
    Err(_) => Err(closed()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn ssl_request_is_length_8_code_80877103() {
    assert_eq!(i32::from_be_bytes(SSL_REQUEST[0..4].try_into().unwrap()), 8);
    assert_eq!(
      i32::from_be_bytes(SSL_REQUEST[4..8].try_into().unwrap()),
      80_877_103
    );
  }

  #[test]
  fn stale_statements() {
    let stale = SqlError::Postgres {
      message: "cached plan must not change result type".into(),
      props: vec![(
        "code",
        deno_error::PropertyValue::String(Cow::Borrowed("0A000")),
      )],
    };
    assert!(is_stale_statement(&stale));
    assert!(!is_stale_statement(&SqlError::postgres_client(
      codes::CONNECTION_CLOSED,
      "closed"
    )));
  }

  #[test]
  fn nul_detection() {
    use crate::params::SqlParam;
    assert!(contains_nul(&SqlParam::Text("a\0".into())));
    assert!(contains_nul(&SqlParam::Array(vec![SqlParam::Json(
      "\"\0\"".into()
    )])));
    assert!(!contains_nul(&SqlParam::Bytes(vec![0])));
  }
}
