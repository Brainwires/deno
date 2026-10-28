// Copyright 2018-2026 the Deno authors. MIT license.

use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use boxed_error::Boxed;
use dashmap::DashMap;
use deno_cache_dir::file_fetcher::RedirectHeaderParseError;
use deno_core::error::AnyError;
use deno_core::futures::StreamExt;
use deno_core::serde;
use deno_core::serde_json;
use deno_core::url::Url;
use deno_error::JsError;
use deno_error::JsErrorBox;
use deno_lib::version::DENO_VERSION_INFO;
use deno_runtime::deno_fetch;
use deno_runtime::deno_fetch::CreateHttpClientOptions;
use deno_runtime::deno_fetch::ResBody;
use deno_runtime::deno_fetch::create_http_client;
use deno_runtime::deno_fetch::dns::ResolvedDenyCheckKind;
use deno_runtime::deno_permissions::PermissionsContainer;
use deno_runtime::deno_tls::RootCertStoreProvider;
use deno_runtime::deno_tls::TlsKey;
use deno_runtime::deno_tls::load_certs;
use deno_runtime::deno_tls::load_private_keys;
use http::HeaderMap;
use http::StatusCode;
use http::header::CONTENT_ENCODING;
use http::header::CONTENT_LENGTH;
use http::header::HeaderName;
use http::header::HeaderValue;
use http_body_util::BodyExt;
use once_cell::sync::OnceCell;
use thiserror::Error;

use crate::util::download_deadline::AttemptFailure;
use crate::util::download_deadline::AttemptsError;
use crate::util::download_deadline::DeadlineError;
use crate::util::download_deadline::DeadlineKind;
use crate::util::download_deadline::DownloadDeadlines;
use crate::util::download_deadline::RetryBudget;
use crate::util::download_deadline::within;
use crate::util::progress_bar::UpdateGuard;

#[derive(Debug, Error)]
pub enum SendError {
  #[error(transparent)]
  Send(#[from] deno_fetch::ClientSendError),
  #[error(transparent)]
  InvalidUri(#[from] http::uri::InvalidUri),
  #[error(transparent)]
  Deadline(#[from] DeadlineError),
}

impl SendError {
  /// Whether the connection, not the server's answer, failed: a deadline, or
  /// an error sending the request. A retry should use a new connection.
  pub fn is_connection_failure(&self) -> bool {
    matches!(self, Self::Send(_) | Self::Deadline(_))
  }

  /// This failure, for a download's per-attempt report.
  pub fn attempt_failure(&self) -> AttemptFailure {
    match self {
      Self::Deadline(err) => AttemptFailure::Deadline(err.kind, err.after),
      Self::Send(err) => match connect_timed_out(err) {
        Some(after) => AttemptFailure::Deadline(DeadlineKind::Connect, after),
        None => AttemptFailure::Error(err.to_string()),
      },
      Self::InvalidUri(err) => AttemptFailure::Error(err.to_string()),
    }
  }
}

/// The connect deadline, when `err` is the connector giving up on a TCP
/// connect (hyper-util reports an `io::ErrorKind::TimedOut` there).
fn connect_timed_out(err: &deno_fetch::ClientSendError) -> Option<Duration> {
  if !err.is_connect_error() {
    return None;
  }
  let mut source: Option<&(dyn std::error::Error + 'static)> =
    Some(&err.source);
  while let Some(err) = source {
    if let Some(io) = err.downcast_ref::<std::io::Error>()
      && io.kind() == std::io::ErrorKind::TimedOut
    {
      return DownloadDeadlines::from_env().connect;
    }
    source = err.source();
  }
  None
}

/// denext (DENEXT.md item 147): the deno_fetch client a provider hands out,
/// replaceable. When a download's connection stalls or breaks, the client
/// (and with it its connection pool) is replaced, so the retry, and every
/// other request after it, opens a new connection instead of queueing on the
/// dead one; with HTTP/2 one connection carries many requests. Requests
/// already in flight on the old client fail over on their own deadlines.
#[derive(Debug)]
struct ClientPool {
  options: CreateHttpClientOptions,
  /// The current client and its generation, counted from 0.
  current: Mutex<(u64, deno_fetch::Client)>,
}

impl ClientPool {
  fn new(options: CreateHttpClientOptions) -> Result<Self, JsErrorBox> {
    let client = Self::create(&options)?;
    Ok(Self {
      options,
      current: Mutex::new((0, client)),
    })
  }

  fn create(
    options: &CreateHttpClientOptions,
  ) -> Result<deno_fetch::Client, JsErrorBox> {
    create_http_client(DENO_VERSION_INFO.user_agent, options.clone())
      .map_err(JsErrorBox::from_err)
  }

  fn current(&self) -> (u64, deno_fetch::Client) {
    self.current.lock().unwrap().clone()
  }

  /// The client to use after a connection of generation `failed` stalled or
  /// broke: a new one, unless another request has already replaced it.
  fn replace(
    &self,
    failed: u64,
  ) -> Result<(u64, deno_fetch::Client), JsErrorBox> {
    let mut current = self.current.lock().unwrap();
    if current.0 == failed {
      log::debug!("Replacing the download client (generation {failed}).");
      *current = (failed + 1, Self::create(&self.options)?);
    }
    Ok(current.clone())
  }
}

pub struct HttpClientProvider {
  options: CreateHttpClientOptions,
  root_cert_store_provider: Option<Arc<dyn RootCertStoreProvider>>,
  deadlines: DownloadDeadlines,
  client: OnceCell<Arc<ClientPool>>,
  cert_clients: DashMap<(String, String), Arc<ClientPool>>,
}

impl std::fmt::Debug for HttpClientProvider {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("HttpClient")
      .field("options", &self.options)
      .finish()
  }
}

impl HttpClientProvider {
  pub fn new(
    root_cert_store_provider: Option<Arc<dyn RootCertStoreProvider>>,
    unsafely_ignore_certificate_errors: Option<Vec<String>>,
  ) -> Self {
    let deadlines = DownloadDeadlines::from_env();
    Self {
      options: CreateHttpClientOptions {
        unsafely_ignore_certificate_errors,
        connect_timeout: deadlines.connect,
        ..Default::default()
      },
      root_cert_store_provider,
      deadlines,
      client: OnceCell::new(),
      cert_clients: DashMap::new(),
    }
  }

  #[cfg(test)]
  fn with_deadlines(mut self, deadlines: DownloadDeadlines) -> Self {
    self.options.connect_timeout = deadlines.connect;
    self.deadlines = deadlines;
    self
  }

  /// Makes every connection this provider's clients open re-check the address
  /// it resolved to against the `--deny-import` list.
  ///
  /// The import permission check runs on the specifier's hostname before any
  /// DNS lookup, so on its own it cannot see that a name resolves to a denied
  /// address. Used by the module fetcher, whose requests are the ones
  /// `--deny-import` governs.
  pub fn with_import_permissions(
    mut self,
    permissions: PermissionsContainer,
  ) -> Self {
    self.options.permissions = Some(permissions);
    self.options.resolved_deny_check_kind = ResolvedDenyCheckKind::Import;
    self
  }

  pub fn get_or_create(&self) -> Result<HttpClient, JsErrorBox> {
    let pool = self.client.get_or_try_init(|| {
      ClientPool::new(CreateHttpClientOptions {
        root_cert_store: match &self.root_cert_store_provider {
          Some(provider) => Some(provider.get_or_try_init()?.clone()),
          None => None,
        },
        ..self.options.clone()
      })
      .map(Arc::new)
    })?;
    Ok(HttpClient::new(pool.clone(), self.deadlines))
  }

  pub fn get_or_create_with_client_cert(
    &self,
    certfile: &Path,
    keyfile: &Path,
  ) -> Result<HttpClient, JsErrorBox> {
    let cache_key = (
      certfile.to_string_lossy().into_owned(),
      keyfile.to_string_lossy().into_owned(),
    );
    if let Some(pool) = self.cert_clients.get(&cache_key) {
      return Ok(HttpClient::new(pool.clone(), self.deadlines));
    }
    let cert_data = std::fs::read(certfile).map_err(|e| {
      JsErrorBox::generic(format!(
        "Failed to read npmrc certfile '{}': {e}",
        certfile.display()
      ))
    })?;
    let key_data = std::fs::read(keyfile).map_err(|e| {
      JsErrorBox::generic(format!(
        "Failed to read npmrc keyfile '{}': {e}",
        keyfile.display()
      ))
    })?;
    let certs =
      load_certs(&mut BufReader::new(cert_data.as_slice())).map_err(|e| {
        JsErrorBox::generic(format!(
          "Failed to parse npmrc certfile '{}': {e}",
          certfile.display()
        ))
      })?;
    let mut keys = load_private_keys(&key_data).map_err(|e| {
      JsErrorBox::generic(format!(
        "Failed to parse npmrc keyfile '{}': {e}",
        keyfile.display()
      ))
    })?;
    if keys.is_empty() {
      return Err(JsErrorBox::generic(format!(
        "No private keys found in npmrc keyfile '{}'",
        keyfile.display()
      )));
    }
    let tls_key = TlsKey(certs, keys.remove(0));
    let pool = Arc::new(ClientPool::new(CreateHttpClientOptions {
      root_cert_store: match &self.root_cert_store_provider {
        Some(provider) => Some(provider.get_or_try_init()?.clone()),
        None => None,
      },
      client_cert_chain_and_key: Some(tls_key),
      ..self.options.clone()
    })?);
    self.cert_clients.insert(cache_key, pool.clone());
    Ok(HttpClient::new(pool, self.deadlines))
  }
}

#[derive(Debug, Error, JsError)]
#[class(type)]
#[error("Bad response: {:?}{}", .status_code, .response_text.as_ref().map(|s| format!("\n\n{}", s)).unwrap_or_else(String::new))]
pub struct BadResponseError {
  pub status_code: StatusCode,
  pub response_text: Option<String>,
}

#[derive(Debug, Boxed, JsError)]
pub struct DownloadError(pub Box<DownloadErrorKind>);

#[derive(Debug, Error, JsError)]
pub enum DownloadErrorKind {
  #[class(inherit)]
  #[error(transparent)]
  Fetch(deno_fetch::ClientSendError),
  #[class(inherit)]
  #[error(transparent)]
  UrlParse(#[from] deno_core::url::ParseError),
  #[class(generic)]
  #[error(transparent)]
  HttpParse(#[from] http::Error),
  #[class(inherit)]
  #[error(transparent)]
  Json(#[from] serde_json::Error),
  #[class(generic)]
  #[error(transparent)]
  ToStr(#[from] http::header::ToStrError),
  #[class(inherit)]
  #[error(transparent)]
  RedirectHeaderParse(RedirectHeaderParseError),
  #[class(type)]
  #[error("Too many redirects.")]
  TooManyRedirects,
  #[class(inherit)]
  #[error(transparent)]
  BadResponse(#[from] BadResponseError),
  #[class("Http")]
  #[error("Not Found.")]
  NotFound,
  #[class("Http")]
  #[error("Received unhandled Not Modified response.")]
  UnhandledNotModified,
  #[class(inherit)]
  #[error(transparent)]
  Deadline(DeadlineError),
  #[class(inherit)]
  #[error(transparent)]
  Attempts(AttemptsError),
  #[class(inherit)]
  #[error(transparent)]
  Other(JsErrorBox),
}

// Plain functions, not `From` impls: `Boxed` already converts anything
// `DownloadErrorKind` converts from.
fn download_error_from_send(err: SendError) -> DownloadError {
  match err {
    SendError::Send(err) => DownloadErrorKind::Fetch(err).into_box(),
    SendError::InvalidUri(err) => {
      DownloadErrorKind::HttpParse(http::Error::from(err)).into_box()
    }
    SendError::Deadline(err) => DownloadErrorKind::Deadline(err).into_box(),
  }
}

fn download_error_from_body(err: ResponseBodyError) -> DownloadError {
  match err {
    ResponseBodyError::Deadline(err) => {
      DownloadErrorKind::Deadline(err).into_box()
    }
    ResponseBodyError::Read(err) => DownloadErrorKind::Other(err).into_box(),
  }
}

impl DownloadError {
  /// Whether the connection, not the server's answer, failed, so a retry
  /// should use a new connection.
  fn is_connection_failure(&self) -> bool {
    matches!(
      self.as_kind(),
      DownloadErrorKind::Fetch(_) | DownloadErrorKind::Deadline(_)
    )
  }

  fn attempt_failure(&self) -> AttemptFailure {
    match self.as_kind() {
      DownloadErrorKind::Deadline(err) => {
        AttemptFailure::Deadline(err.kind, err.after)
      }
      DownloadErrorKind::Fetch(err) => match connect_timed_out(err) {
        Some(after) => AttemptFailure::Deadline(DeadlineKind::Connect, after),
        None => AttemptFailure::Error(err.to_string()),
      },
      _ => AttemptFailure::Error(self.to_string()),
    }
  }
}

/// A response body that could not be read.
#[derive(Debug, Error, JsError)]
pub enum ResponseBodyError {
  #[class(inherit)]
  #[error(transparent)]
  Deadline(DeadlineError),
  #[class(inherit)]
  #[error(transparent)]
  Read(JsErrorBox),
}

#[derive(Debug)]
pub enum HttpClientResponse {
  Success {
    headers: HeaderMap<HeaderValue>,
    body: Vec<u8>,
  },
  NotFound,
  NotModified,
}

impl HttpClientResponse {
  pub fn into_bytes(self) -> Result<Vec<u8>, DownloadError> {
    match self {
      Self::Success { body, .. } => Ok(body),
      Self::NotFound => Err(DownloadErrorKind::NotFound.into_box()),
      Self::NotModified => {
        Err(DownloadErrorKind::UnhandledNotModified.into_box())
      }
    }
  }

  pub fn into_maybe_bytes(self) -> Result<Option<Vec<u8>>, DownloadError> {
    match self {
      Self::Success { body, .. } => Ok(Some(body)),
      Self::NotFound => Ok(None),
      Self::NotModified => {
        Err(DownloadErrorKind::UnhandledNotModified.into_box())
      }
    }
  }
}

#[derive(Debug)]
pub struct HttpClient {
  pool: Arc<ClientPool>,
  generation: u64,
  client: deno_fetch::Client,
  deadlines: DownloadDeadlines,
}

impl HttpClient {
  // DO NOT make this public. You should always be creating one of these from
  // the HttpClientProvider
  fn new(pool: Arc<ClientPool>, deadlines: DownloadDeadlines) -> Self {
    let (generation, client) = pool.current();
    Self {
      pool,
      generation,
      client,
      deadlines,
    }
  }

  #[cfg(test)]
  fn for_test(client: deno_fetch::Client) -> Self {
    let pool = ClientPool {
      options: Default::default(),
      current: Mutex::new((0, client)),
    };
    Self::new(Arc::new(pool), DownloadDeadlines::default())
  }

  /// The deadlines this client's downloads run under.
  pub fn deadlines(&self) -> DownloadDeadlines {
    self.deadlines
  }

  /// Retires this client's connections after one stalled or broke: the
  /// provider's next client (`get_or_create`) opens a new connection.
  pub fn replace_connection(&self) {
    if let Err(err) = self.pool.replace(self.generation) {
      log::debug!("Could not create a new download client: {err}");
    }
  }

  /// Sends `req` on `client`, giving up when no response headers arrive
  /// within the response deadline.
  async fn send_within_deadline(
    &self,
    client: &deno_fetch::Client,
    url: &Url,
    req: http::Request<deno_fetch::ReqBody>,
    should_decompress: bool,
  ) -> Result<http::Response<ResBody>, SendError> {
    let send = async {
      if should_decompress {
        client.clone().send(req).await
      } else {
        client.clone().send_no_decompress(req).await
      }
    };
    match within(self.deadlines.response, send).await {
      Ok(result) => result.map_err(SendError::Send),
      Err(after) => Err(SendError::Deadline(DeadlineError {
        url: url.clone(),
        kind: DeadlineKind::Response,
        after,
      })),
    }
  }

  pub fn get(&self, url: Url) -> Result<RequestBuilder, http::Error> {
    let body = deno_fetch::ReqBody::empty();
    let mut req = http::Request::new(body);
    *req.uri_mut() = url.as_str().parse()?;
    Ok(RequestBuilder {
      client: self.client.clone(),
      req,
    })
  }

  pub fn post(
    &self,
    url: Url,
    body: deno_fetch::ReqBody,
  ) -> Result<RequestBuilder, http::Error> {
    let mut req = http::Request::new(body);
    *req.method_mut() = http::Method::POST;
    *req.uri_mut() = url.as_str().parse()?;
    Ok(RequestBuilder {
      client: self.client.clone(),
      req,
    })
  }

  pub fn post_json<S>(
    &self,
    url: Url,
    ser: &S,
  ) -> Result<RequestBuilder, DownloadError>
  where
    S: serde::Serialize,
  {
    let json = deno_core::serde_json::to_vec(ser)?;
    let body = deno_fetch::ReqBody::full(json.into());
    let builder = self.post(url, body)?;
    Ok(builder.header(
      http::header::CONTENT_TYPE,
      "application/json".parse().map_err(http::Error::from)?,
    ))
  }

  pub async fn send(
    &self,
    url: &Url,
    headers: HeaderMap,
  ) -> Result<http::Response<ResBody>, SendError> {
    let body = deno_fetch::ReqBody::empty();
    let mut request = http::Request::new(body);
    *request.uri_mut() = http::Uri::try_from(url.as_str())?;
    *request.headers_mut() = headers;

    let result = self
      .send_within_deadline(&self.client, url, request, true)
      .await;
    if let Err(err) = &result
      && err.is_connection_failure()
    {
      self.replace_connection();
    }
    result
  }

  pub async fn download_text(&self, url: Url) -> Result<String, AnyError> {
    let bytes = self.download(url).await?;
    Ok(String::from_utf8(bytes)?)
  }

  pub async fn download(&self, url: Url) -> Result<Vec<u8>, DownloadError> {
    let response = self
      .download_with_retries(
        url,
        &Default::default(),
        None,
        true,
        RetryOn::ConnectionFailures,
      )
      .await?;
    response.into_bytes()
  }

  pub async fn download_with_progress_and_retries(
    &self,
    url: Url,
    headers: &HeaderMap,
    progress_guard: &UpdateGuard,
  ) -> Result<HttpClientResponse, DownloadError> {
    self
      .download_with_retries(
        url,
        headers,
        Some(progress_guard),
        true,
        RetryOn::AnyFailure,
      )
      .await
  }

  /// Like `download_with_progress_and_retries`, but bypasses the transparent
  /// decompression middleware. The response body will contain raw bytes
  /// (potentially gzip-compressed). The caller should check the
  /// Content-Encoding header and decompress if needed.
  pub async fn download_with_progress_and_retries_no_decompress(
    &self,
    url: Url,
    headers: &HeaderMap,
    progress_guard: &UpdateGuard,
  ) -> Result<HttpClientResponse, DownloadError> {
    self
      .download_with_retries(
        url,
        headers,
        Some(progress_guard),
        false,
        RetryOn::AnyFailure,
      )
      .await
  }

  /// Downloads `url`, retrying within the [`DownloadDeadlines`]: a retry
  /// after a stalled or broken connection runs on a new one. When every
  /// attempt failed and one of them on a deadline, the error lists each
  /// attempt; otherwise it is the first attempt's error.
  async fn download_with_retries(
    &self,
    url: Url,
    headers: &HeaderMap,
    progress_guard: Option<&UpdateGuard>,
    should_decompress: bool,
    retry_on: RetryOn,
  ) -> Result<HttpClientResponse, DownloadError> {
    let mut budget = RetryBudget::new(self.deadlines);
    let (mut generation, mut client) = (self.generation, self.client.clone());
    let mut first_error = None;
    loop {
      let err = match self
        .download_inner(
          &client,
          url.clone(),
          headers,
          progress_guard,
          should_decompress,
        )
        .await
      {
        Ok(response) => return Ok(response),
        Err(err) => err,
      };
      let retryable = match retry_on {
        RetryOn::AnyFailure => matches!(
          err.as_kind(),
          DownloadErrorKind::BadResponse(_)
            | DownloadErrorKind::Fetch(_)
            | DownloadErrorKind::Deadline(_)
        ),
        RetryOn::ConnectionFailures => err.is_connection_failure(),
      };
      if !retryable {
        return Err(err);
      }
      log::debug!("Downloading '{url}' failed: {err}");
      if err.is_connection_failure() {
        (generation, client) = self
          .pool
          .replace(generation)
          .map_err(|e| DownloadErrorKind::Other(e).into_box())?;
      }
      let wait = budget.record(err.attempt_failure());
      first_error.get_or_insert(err);
      match wait {
        Some(wait) => tokio::time::sleep(wait).await,
        None => break,
      }
    }
    if budget.saw_deadline() {
      Err(DownloadErrorKind::Attempts(budget.into_error(url)).into_box())
    } else {
      Err(first_error.unwrap())
    }
  }

  pub async fn get_redirected_url(
    &self,
    url: Url,
    headers: &HeaderMap<HeaderValue>,
  ) -> Result<Url, AnyError> {
    let (_, url) = self
      .get_redirected_response(&self.client, url, headers, true)
      .await?;
    Ok(url)
  }

  async fn download_inner(
    &self,
    client: &deno_fetch::Client,
    url: Url,
    headers: &HeaderMap<HeaderValue>,
    progress_guard: Option<&UpdateGuard>,
    should_decompress: bool,
  ) -> Result<HttpClientResponse, DownloadError> {
    let (response, url) = self
      .get_redirected_response(client, url, headers, should_decompress)
      .await?;

    if response.status() == 404 {
      return Ok(HttpClientResponse::NotFound);
    } else if response.status() == 304 {
      return Ok(HttpClientResponse::NotModified);
    } else if !response.status().is_success() {
      let status = response.status();
      let maybe_response_text =
        get_response_body_with_deadline(response, &url, None, self.deadlines)
          .await
          .ok()
          .and_then(|(_, body)| String::from_utf8(body).ok());
      return Err(
        DownloadErrorKind::BadResponse(BadResponseError {
          status_code: status,
          response_text: maybe_response_text
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        })
        .into_box(),
      );
    }

    get_response_body_with_deadline(
      response,
      &url,
      progress_guard,
      self.deadlines,
    )
    .await
    .map(|(headers, body)| HttpClientResponse::Success { headers, body })
    .map_err(download_error_from_body)
  }

  async fn get_redirected_response(
    &self,
    client: &deno_fetch::Client,
    mut url: Url,
    headers: &HeaderMap<HeaderValue>,
    should_decompress: bool,
  ) -> Result<(http::Response<deno_fetch::ResBody>, Url), DownloadError> {
    let mut req = self.get(url.clone())?.build();
    *req.headers_mut() = headers.clone();
    let mut response = self
      .send_within_deadline(client, &url, req, should_decompress)
      .await
      .map_err(download_error_from_send)?;
    let status = response.status();
    if status.is_redirection() && status != http::StatusCode::NOT_MODIFIED {
      let mut redirect_headers = headers.clone();
      for _ in 0..5 {
        let new_url = resolve_redirect_from_response(&url, &response)?;
        let mut req = self.get(new_url.clone())?.build();

        if should_strip_auth_on_redirect(&url, &new_url) {
          redirect_headers.remove(http::header::AUTHORIZATION);
        }
        *req.headers_mut() = redirect_headers.clone();

        let new_response = self
          .send_within_deadline(client, &new_url, req, should_decompress)
          .await
          .map_err(download_error_from_send)?;
        let status = new_response.status();
        if status.is_redirection() {
          response = new_response;
          url = new_url;
        } else {
          return Ok((new_response, new_url));
        }
      }
      Err(DownloadErrorKind::TooManyRedirects.into_box())
    } else {
      Ok((response, url))
    }
  }
}

/// Returns true if auth headers should be stripped when redirecting from
/// `original` to `new_url`. Auth is stripped when the host or port changes,
/// or on a scheme downgrade (https -> http) to avoid leaking credentials
/// over plaintext. Same-host scheme upgrades (http -> https) are allowed.
fn should_strip_auth_on_redirect(original: &Url, new_url: &Url) -> bool {
  new_url.host() != original.host()
    || new_url.port() != original.port()
    || (original.scheme() == "https" && new_url.scheme() == "http")
}

/// Which failures `download_with_retries` retries.
#[derive(Debug, Clone, Copy)]
enum RetryOn {
  /// A failed request, an error response, or a deadline.
  AnyFailure,
  /// Only a failed request or a deadline (`download`, which callers use for
  /// lookups that answer with an error status on purpose).
  ConnectionFailures,
}

/// Reads `response`'s body, reporting progress to `progress_guard`. Gives up
/// when no data arrives for the idle deadline in `deadlines`; a body that
/// keeps arriving, however slowly, is read to the end.
pub async fn get_response_body_with_deadline(
  response: http::Response<deno_fetch::ResBody>,
  url: &Url,
  progress_guard: Option<&UpdateGuard>,
  deadlines: DownloadDeadlines,
) -> Result<(HeaderMap, Vec<u8>), ResponseBodyError> {
  use http_body::Body as _;
  let mut total_size = None;
  if progress_guard.is_some() {
    total_size = response.body().size_hint().exact();
    if total_size.is_none() {
      // `Content-Length` describes the encoded body, so it can't be used as
      // the progress total when the body was transparently decompressed.
      total_size = response
        .headers()
        .get(CONTENT_LENGTH)
        .filter(|_| !response.headers().contains_key(CONTENT_ENCODING))
        .and_then(|val| val.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    }
  }
  // Only a known total drives the progress bar.
  let progress_guard = progress_guard.filter(|_| total_size.is_some());
  if let (Some(progress_guard), Some(total_size)) = (progress_guard, total_size)
  {
    progress_guard.set_total_size(total_size);
  }

  let (parts, body) = response.into_parts();
  let mut data = Vec::with_capacity(total_size.unwrap_or(0) as usize);
  let mut stream = body.into_data_stream();
  loop {
    let item =
      within(deadlines.idle, stream.next())
        .await
        .map_err(|after| {
          ResponseBodyError::Deadline(DeadlineError {
            url: url.clone(),
            kind: DeadlineKind::Idle,
            after,
          })
        })?;
    let Some(item) = item else {
      break;
    };
    let bytes = item.map_err(|err| ResponseBodyError::Read(err.into()))?;
    data.extend_from_slice(&bytes);
    if let Some(progress_guard) = progress_guard {
      progress_guard.set_position(data.len() as u64);
    }
  }
  Ok((parts.headers, data))
}

fn resolve_redirect_from_response<B>(
  request_url: &Url,
  response: &http::Response<B>,
) -> Result<Url, DownloadError> {
  debug_assert!(response.status().is_redirection());
  deno_cache_dir::file_fetcher::resolve_redirect_from_headers(
    request_url,
    response.headers(),
  )
  .map_err(|err| DownloadErrorKind::RedirectHeaderParse(*err).into_box())
}

pub async fn body_to_string<B>(body: B) -> Result<String, AnyError>
where
  B: http_body::Body,
  AnyError: From<B::Error>,
{
  let bytes = body.collect().await?.to_bytes();
  let s = std::str::from_utf8(&bytes)?;
  Ok(s.into())
}

pub async fn body_to_json<B, D>(body: B) -> Result<D, AnyError>
where
  B: http_body::Body,
  AnyError: From<B::Error>,
  D: serde::de::DeserializeOwned,
{
  let bytes = body.collect().await?.to_bytes();
  let val = deno_core::serde_json::from_slice(&bytes)?;
  Ok(val)
}

pub struct RequestBuilder {
  client: deno_fetch::Client,
  req: http::Request<deno_fetch::ReqBody>,
}

impl RequestBuilder {
  pub fn header(mut self, name: HeaderName, value: HeaderValue) -> Self {
    self.req.headers_mut().append(name, value);
    self
  }

  pub async fn send(
    self,
  ) -> Result<http::Response<deno_fetch::ResBody>, AnyError> {
    self.client.send(self.req).await.map_err(Into::into)
  }

  pub fn build(self) -> http::Request<deno_fetch::ReqBody> {
    self.req
  }
}

#[allow(clippy::print_stdout, reason = "test code")]
#[allow(clippy::print_stderr, reason = "test code")]
#[cfg(test)]
mod test {
  use std::collections::HashSet;
  use std::hash::RandomState;
  use std::sync::atomic::AtomicUsize;
  use std::sync::atomic::Ordering;
  use std::time::Instant;

  use deno_runtime::deno_tls::rustls::RootCertStore;
  use tokio::io::AsyncReadExt;
  use tokio::io::AsyncWriteExt;
  use tokio::net::TcpListener;

  use super::*;
  use crate::util::progress_bar::ProgressBar;
  use crate::util::progress_bar::ProgressBarStyle;

  #[tokio::test]
  async fn test_http_client_download_redirect() {
    let _http_server_guard = test_util::http_server();
    let client = HttpClientProvider::new(None, None).get_or_create().unwrap();

    // make a request to the redirect server
    let text = client
      .download_text(
        Url::parse("http://localhost:4546/subdir/redirects/redirect1.js")
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(text, "export const redirect = 1;\n");

    // now make one to the infinite redirects server
    let err = client
      .download_text(
        Url::parse("http://localhost:4549/subdir/redirects/redirect1.js")
          .unwrap(),
      )
      .await
      .err()
      .unwrap();
    assert_eq!(err.to_string(), "Too many redirects.");
  }

  async fn read_request(
    stream: &mut tokio::net::TcpStream,
  ) -> (String, Option<String>) {
    let mut request = Vec::new();
    let mut buffer = [0; 1024];
    loop {
      let count = stream.read(&mut buffer).await.unwrap();
      assert_ne!(count, 0, "connection closed before request headers");
      request.extend_from_slice(&buffer[..count]);
      if request.windows(4).any(|window| window == b"\r\n\r\n") {
        break;
      }
    }

    let request = String::from_utf8(request).unwrap();
    let path = request
      .lines()
      .next()
      .and_then(|line| line.split_whitespace().nth(1))
      .unwrap()
      .to_string();
    let authorization = request.lines().find_map(|line| {
      let (name, value) = line.split_once(':')?;
      name
        .eq_ignore_ascii_case("authorization")
        .then(|| value.trim().to_string())
    });
    (path, authorization)
  }

  #[tokio::test]
  async fn test_http_client_redirect_does_not_restore_auth() {
    let source_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let source_addr = source_listener.local_addr().unwrap();
    let destination_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let destination_addr = destination_listener.local_addr().unwrap();

    let source_server = tokio::spawn(async move {
      let requests = [
        ("/start", format!("http://{source_addr}/same")),
        ("/same", format!("http://{destination_addr}/first")),
      ];
      let mut auth_headers = Vec::new();
      for (expected_path, location) in requests {
        let (mut stream, _) = source_listener.accept().await.unwrap();
        let (path, authorization) = read_request(&mut stream).await;
        assert_eq!(path, expected_path);
        auth_headers.push(authorization);
        stream
          .write_all(
            format!(
              "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
          )
          .await
          .unwrap();
      }
      auth_headers
    });

    let destination_server = tokio::spawn(async move {
      let mut auth_headers = Vec::new();
      for expected_path in ["/first", "/last"] {
        let (mut stream, _) = destination_listener.accept().await.unwrap();
        let (path, authorization) = read_request(&mut stream).await;
        assert_eq!(path, expected_path);
        auth_headers.push(authorization);
        let response = if path == "/first" {
          format!(
            "HTTP/1.1 302 Found\r\nLocation: http://{destination_addr}/last\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
          )
        } else {
          "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
            .to_string()
        };
        stream.write_all(response.as_bytes()).await.unwrap();
      }
      auth_headers
    });

    let client = HttpClientProvider::new(None, None).get_or_create().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
      http::header::AUTHORIZATION,
      HeaderValue::from_static("Bearer test-token"),
    );
    let (response, url) = client
      .get_redirected_response(
        Url::parse(&format!("http://{source_addr}/start")).unwrap(),
        &headers,
        true,
      )
      .await
      .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(url.as_str(), format!("http://{destination_addr}/last"));
    assert_eq!(
      source_server.await.unwrap(),
      [
        Some("Bearer test-token".to_string()),
        Some("Bearer test-token".to_string())
      ]
    );
    assert_eq!(destination_server.await.unwrap(), [None, None]);
  }

  #[tokio::test]
  async fn test_fetch_with_cafile_string() {
    let _http_server_guard = test_util::http_server();
    let url = Url::parse("https://localhost:5545/assets/fixture.json").unwrap();

    let client = HttpClient::for_test(
      create_http_client(
        DENO_VERSION_INFO.user_agent,
        CreateHttpClientOptions {
          ca_certs: vec![
            std::fs::read(test_util::testdata_path().join("tls/RootCA.pem"))
              .unwrap(),
          ],
          ..Default::default()
        },
      )
      .unwrap(),
    );
    let response = client.send(&url, Default::default()).await.unwrap();
    assert!(response.status().is_success());
    let (parts, body) = response.into_parts();
    let headers = parts.headers;
    let body = body.collect().await.unwrap().to_bytes();
    assert!(!body.is_empty());
    assert_eq!(headers.get("content-type").unwrap(), "application/json");
    assert_eq!(headers.get("etag"), None);
    assert_eq!(headers.get("x-typescript-types"), None);
  }

  static PUBLIC_HTTPS_URLS: &[&str] = &[
    "https://deno.com/",
    "https://example.com/",
    "https://github.com/",
    "https://www.w3.org/",
  ];

  /// This test depends on external servers, so we need to be careful to avoid mistaking an offline machine with a
  /// test failure.
  #[tokio::test]
  async fn test_fetch_with_default_certificate_store() {
    let urls: HashSet<_, RandomState> =
      HashSet::from_iter(PUBLIC_HTTPS_URLS.iter());

    // Rely on the randomization of hashset iteration
    for url in urls {
      // Relies on external http server with a valid mozilla root CA cert.
      let url = Url::parse(url).unwrap();
      eprintln!("Attempting to fetch {url}...");

      let client = HttpClient::for_test(
        create_http_client(
          DENO_VERSION_INFO.user_agent,
          CreateHttpClientOptions::default(),
        )
        .unwrap(),
      );

      let result = client.send(&url, Default::default()).await;
      match result {
        Ok(response) if response.status().is_success() => {
          return; // success
        }
        _ => {
          // keep going
        }
      }
    }

    // Use 1.1.1.1 and 8.8.8.8 as our last-ditch internet check
    if std::net::TcpStream::connect("8.8.8.8:80").is_err()
      && std::net::TcpStream::connect("1.1.1.1:80").is_err()
    {
      return;
    }

    panic!(
      "None of the expected public URLs were available but internet appears to be available"
    );
  }

  #[tokio::test]
  async fn test_fetch_with_empty_certificate_store() {
    let root_cert_store = RootCertStore::empty();
    let urls: HashSet<_, RandomState> =
      HashSet::from_iter(PUBLIC_HTTPS_URLS.iter());

    // Rely on the randomization of hashset iteration
    let url = urls.into_iter().next().unwrap();
    // Relies on external http server with a valid mozilla root CA cert.
    let url = Url::parse(url).unwrap();
    eprintln!("Attempting to fetch {url}...");

    let client = HttpClient::for_test(
      create_http_client(
        DENO_VERSION_INFO.user_agent,
        CreateHttpClientOptions {
          root_cert_store: Some(root_cert_store),
          ..Default::default()
        },
      )
      .unwrap(),
    );

    let result = client.send(&url, HeaderMap::new()).await;
    assert!(result.is_err() || !result.unwrap().status().is_success());
  }

  #[tokio::test]
  async fn test_fetch_with_cafile_gzip() {
    let _http_server_guard = test_util::http_server();
    let url =
      Url::parse("https://localhost:5545/run/import_compression/gziped")
        .unwrap();
    let client = HttpClient::for_test(
      create_http_client(
        DENO_VERSION_INFO.user_agent,
        CreateHttpClientOptions {
          ca_certs: vec![
            std::fs::read(
              test_util::testdata_path()
                .join("tls/RootCA.pem")
                .to_string(),
            )
            .unwrap(),
          ],
          ..Default::default()
        },
      )
      .unwrap(),
    );
    let response = client.send(&url, Default::default()).await.unwrap();
    assert!(response.status().is_success());
    let (parts, body) = response.into_parts();
    let headers = parts.headers;
    let body = body.collect().await.unwrap().to_bytes().to_vec();
    assert_eq!(String::from_utf8(body).unwrap(), "console.log('gzip')");
    assert_eq!(
      headers.get("content-type").unwrap(),
      "application/javascript"
    );
    assert_eq!(headers.get("etag"), None);
    assert_eq!(headers.get("x-typescript-types"), None);
  }

  #[tokio::test]
  async fn test_fetch_with_cafile_with_etag() {
    let _http_server_guard = test_util::http_server();
    let url = Url::parse("https://localhost:5545/etag_script.ts").unwrap();
    let client = HttpClient::for_test(
      create_http_client(
        DENO_VERSION_INFO.user_agent,
        CreateHttpClientOptions {
          ca_certs: vec![
            std::fs::read(
              test_util::testdata_path()
                .join("tls/RootCA.pem")
                .to_string(),
            )
            .unwrap(),
          ],
          ..Default::default()
        },
      )
      .unwrap(),
    );
    let response = client.send(&url, Default::default()).await.unwrap();
    assert!(response.status().is_success());
    let (parts, body) = response.into_parts();
    let headers = parts.headers;
    let body = body.collect().await.unwrap().to_bytes().to_vec();
    assert!(!body.is_empty());
    assert_eq!(String::from_utf8(body).unwrap(), "console.log('etag')");
    assert_eq!(
      headers.get("content-type").unwrap(),
      "application/typescript"
    );
    assert_eq!(headers.get("etag").unwrap(), "33a64df551425fcc55e");
    assert_eq!(headers.get("x-typescript-types"), None);

    let mut headers = HeaderMap::new();
    headers.insert("If-None-Match", "33a64df551425fcc55e".parse().unwrap());
    let res = client.send(&url, headers).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
  }

  #[tokio::test]
  async fn test_fetch_with_cafile_brotli() {
    let _http_server_guard = test_util::http_server();
    let url =
      Url::parse("https://localhost:5545/run/import_compression/brotli")
        .unwrap();
    let client = HttpClient::for_test(
      create_http_client(
        DENO_VERSION_INFO.user_agent,
        CreateHttpClientOptions {
          ca_certs: vec![
            std::fs::read(
              test_util::testdata_path()
                .join("tls/RootCA.pem")
                .to_string(),
            )
            .unwrap(),
          ],
          ..Default::default()
        },
      )
      .unwrap(),
    );
    let response = client.send(&url, Default::default()).await.unwrap();
    assert!(response.status().is_success());
    let (parts, body) = response.into_parts();
    let headers = parts.headers;
    let body = body.collect().await.unwrap().to_bytes().to_vec();
    assert!(!body.is_empty());
    assert_eq!(String::from_utf8(body).unwrap(), "console.log('brotli');");
    assert_eq!(
      headers.get("content-type").unwrap(),
      "application/javascript"
    );
    assert_eq!(headers.get("etag"), None);
    assert_eq!(headers.get("x-typescript-types"), None);
  }

  #[test]
  fn test_should_strip_auth_on_redirect() {
    // http -> https same host: safe upgrade, retain auth
    assert!(!should_strip_auth_on_redirect(
      &Url::parse("http://npm.pkg.github.com/package").unwrap(),
      &Url::parse("https://npm.pkg.github.com/package").unwrap(),
    ));

    // same origin: retain auth
    assert!(!should_strip_auth_on_redirect(
      &Url::parse("https://registry.example.com/a").unwrap(),
      &Url::parse("https://registry.example.com/b").unwrap(),
    ));

    // different host: strip auth
    assert!(should_strip_auth_on_redirect(
      &Url::parse("https://registry.example.com/package").unwrap(),
      &Url::parse("https://other.example.com/package").unwrap(),
    ));

    // different port: strip auth
    assert!(should_strip_auth_on_redirect(
      &Url::parse("https://registry.example.com:8080/package").unwrap(),
      &Url::parse("https://registry.example.com:9090/package").unwrap(),
    ));

    // https -> http same host: scheme downgrade, strip auth
    assert!(should_strip_auth_on_redirect(
      &Url::parse("https://npm.pkg.github.com/package").unwrap(),
      &Url::parse("http://npm.pkg.github.com/package").unwrap(),
    ));
  }

  // denext (DENEXT.md item 147): download deadlines and retries.

  fn short_deadlines(ms: u64) -> DownloadDeadlines {
    DownloadDeadlines {
      connect: Some(Duration::from_millis(ms)),
      response: Some(Duration::from_millis(ms)),
      idle: Some(Duration::from_millis(ms)),
      attempts: 4,
      total: Some(Duration::from_secs(60)),
    }
  }

  /// A server on loopback; `serve` handles each connection, numbered from 0.
  async fn numbered_server<F, Fut>(serve: F) -> (Url, Arc<AtomicUsize>)
  where
    F: Fn(tokio::net::TcpStream, usize) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
  {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let count = connections.clone();
    tokio::spawn(async move {
      loop {
        let (stream, _) = listener.accept().await.unwrap();
        let n = count.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(serve(stream, n));
      }
    });
    (
      Url::parse(&format!("http://{addr}/registry/pkg")).unwrap(),
      connections,
    )
  }

  async fn download_under(
    provider: &HttpClientProvider,
    url: &Url,
  ) -> Result<HttpClientResponse, DownloadError> {
    let client = provider.get_or_create().unwrap();
    let progress_bar = ProgressBar::new(ProgressBarStyle::DownloadBars);
    let guard = progress_bar.update(url.as_str());
    client
      .download_with_progress_and_retries(
        url.clone(),
        &Default::default(),
        &guard,
      )
      .await
  }

  fn attempts_of(err: &DownloadError) -> &AttemptsError {
    match err.as_kind() {
      DownloadErrorKind::Attempts(err) => err,
      _ => panic!("expected the per-attempt report, got {err:?}"),
    }
  }

  #[tokio::test]
  async fn download_retries_a_server_that_never_answers_then_reports() {
    let (url, connections) = numbered_server(|mut stream, _| async move {
      read_request(&mut stream).await;
      // Accepted, read, and never answered.
      tokio::time::sleep(Duration::from_secs(600)).await;
      drop(stream);
    })
    .await;
    let provider =
      HttpClientProvider::new(None, None).with_deadlines(short_deadlines(300));
    let started = Instant::now();
    let err = download_under(&provider, &url).await.unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(10));
    let report = attempts_of(&err);
    assert_eq!(report.url, url);
    assert_eq!(
      report.attempts,
      vec![
        AttemptFailure::Deadline(
          DeadlineKind::Response,
          Duration::from_millis(300)
        );
        4
      ]
    );
    // Every attempt on a new connection.
    assert_eq!(connections.load(Ordering::SeqCst), 4);
    let message = err.to_string();
    assert!(
      message.starts_with(&format!(
        "Timed out downloading '{url}' (4 attempts, each on a new connection):\n    attempt 1: no response within 300ms (DENO_DOWNLOAD_RESPONSE_TIMEOUT_MS)\n"
      )),
      "{message}"
    );
  }

  #[tokio::test]
  async fn download_retries_a_body_that_stops() {
    let (url, connections) = numbered_server(|mut stream, _| async move {
      read_request(&mut stream).await;
      stream
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n0123456789")
        .await
        .unwrap();
      // 10 of the 100 bytes, then nothing.
      tokio::time::sleep(Duration::from_secs(600)).await;
      drop(stream);
    })
    .await;
    let provider =
      HttpClientProvider::new(None, None).with_deadlines(short_deadlines(300));
    let err = download_under(&provider, &url).await.unwrap_err();
    assert_eq!(
      attempts_of(&err).attempts,
      vec![
        AttemptFailure::Deadline(
          DeadlineKind::Idle,
          Duration::from_millis(300)
        );
        4
      ]
    );
    assert!(
      err.to_string().contains(
        "attempt 4: no data received for 300ms (DENO_DOWNLOAD_IDLE_TIMEOUT_MS)"
      ),
      "{err}"
    );
    assert_eq!(connections.load(Ordering::SeqCst), 4);
  }

  #[tokio::test]
  async fn download_keeps_a_slow_but_steady_body() {
    const BODY: &[u8] = b"a slow but steady body";
    let (url, connections) = numbered_server(|mut stream, _| async move {
      read_request(&mut stream).await;
      tokio::time::sleep(Duration::from_millis(100)).await;
      stream
        .write_all(
          format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", BODY.len())
            .as_bytes(),
        )
        .await
        .unwrap();
      // One byte every 100 ms: the body takes far longer than any deadline,
      // but data never stops for long.
      for byte in BODY {
        tokio::time::sleep(Duration::from_millis(100)).await;
        stream.write_all(&[*byte]).await.unwrap();
        stream.flush().await.unwrap();
      }
    })
    .await;
    let mut deadlines = short_deadlines(500);
    // Even an overall budget shorter than the body doesn't cut it off.
    deadlines.total = Some(Duration::from_millis(500));
    let provider =
      HttpClientProvider::new(None, None).with_deadlines(deadlines);
    let started = Instant::now();
    let response = download_under(&provider, &url).await.unwrap();
    assert!(started.elapsed() > Duration::from_secs(2));
    assert_eq!(response.into_bytes().unwrap(), BODY);
    assert_eq!(connections.load(Ordering::SeqCst), 1);
  }

  /// An HTTP/2 (prior knowledge) server whose first connection takes every
  /// request and never answers; later connections answer at once.
  async fn h2_server_with_a_dead_first_connection()
  -> (Url, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let stalled_requests = Arc::new(AtomicUsize::new(0));
    let stalled = stalled_requests.clone();
    let (url, connections) = numbered_server(move |stream, n| {
      let stalled = stalled.clone();
      async move {
        let service = hyper::service::service_fn(
          move |req: http::Request<hyper::body::Incoming>| {
            let stalled = stalled.clone();
            async move {
              if n == 0 {
                stalled.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_secs(600)).await;
              }
              Ok::<_, std::convert::Infallible>(http::Response::new(
                http_body_util::Full::new(bytes::Bytes::from(format!(
                  "{} from connection {n}",
                  req.uri().path()
                ))),
              ))
            }
          },
        );
        let _ = hyper::server::conn::http2::Builder::new(
          hyper_util::rt::TokioExecutor::new(),
        )
        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
        .await;
      }
    })
    .await;
    (url, connections, stalled_requests)
  }

  #[tokio::test]
  async fn download_retries_a_dead_http2_connection_on_a_new_one() {
    let (url, connections, stalled_requests) =
      h2_server_with_a_dead_first_connection().await;
    // The default deadlines: the retry must come within about 30 s.
    let mut provider = HttpClientProvider::new(None, None)
      .with_deadlines(DownloadDeadlines::default());
    provider.options.http1 = false; // HTTP/2 with prior knowledge
    let started = Instant::now();
    // Three downloads at once: all ride the first connection, all stall on
    // it, and all must fail over to a new one instead of queueing on it.
    let urls = ["/a", "/b", "/c"].map(|path| url.join(path).unwrap());
    let results = deno_core::futures::future::join_all(
      urls.iter().map(|url| download_under(&provider, url)),
    )
    .await;
    let elapsed = started.elapsed();
    for (url, result) in urls.iter().zip(results) {
      let body = result.unwrap().into_bytes().unwrap();
      let body = String::from_utf8(body).unwrap();
      assert!(body.starts_with(url.path()), "{body}");
      assert!(!body.ends_with("from connection 0"), "{body}");
    }
    assert!(stalled_requests.load(Ordering::SeqCst) >= 1);
    assert!(
      elapsed >= Duration::from_secs(15) && elapsed < Duration::from_secs(30),
      "{elapsed:?}"
    );
    // The stalled connection's client was replaced once; the retries share
    // the new connection rather than each opening one.
    assert_eq!(connections.load(Ordering::SeqCst), 2);
  }

  #[tokio::test]
  async fn send_replaces_a_stalled_connection() {
    // The module loader's path: `HttpClient::send`, then `get_or_create`
    // again for the retry.
    let (url, connections, _) = h2_server_with_a_dead_first_connection().await;
    let mut provider =
      HttpClientProvider::new(None, None).with_deadlines(short_deadlines(300));
    provider.options.http1 = false;
    let client = provider.get_or_create().unwrap();
    let Err(SendError::Deadline(deadline)) =
      client.send(&url, Default::default()).await
    else {
      panic!("expected a deadline error");
    };
    assert_eq!(deadline.kind, DeadlineKind::Response);
    let client = provider.get_or_create().unwrap();
    let Ok(response) = client.send(&url, Default::default()).await else {
      panic!("the retry should have used a new connection");
    };
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(connections.load(Ordering::SeqCst), 2);
  }
}
