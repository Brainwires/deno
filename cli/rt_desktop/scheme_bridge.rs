// Copyright 2018-2026 the Deno authors. MIT license.

//! Bridges webview custom-scheme requests to the in-process `Deno.serve`
//! server.
//!
//! The desktop runtime serves the user's `Deno.serve` app over an in-memory
//! byte channel (`DENO_SERVE_ADDRESS=memory:…`) instead of a plain TCP loopback.
//! The embedded browser can't speak to an in-memory channel directly, so we
//! register a laufey custom scheme handler for the app origin's scheme
//! (`desktop.app.origin` in deno.json, [`AppOrigin`]): each browser request is
//! delivered here, we open a fresh connection to the in-process listener,
//! speak HTTP/1.1 over it with a hyper client (the server side is hyper too),
//! and stream the response back to the webview.
//!
//! The page therefore runs at a STABLE origin (e.g. `myapp://app`) — no random
//! loopback port — which is what an external identity provider that validates
//! the browser `Origin` server-side needs to allow-list, and what keeps
//! origin-keyed storage (`localStorage`, IndexedDB, cookies) in place across
//! launches. The bridge serves exactly that one origin: a request the webview
//! delivers for another host on the same scheme is answered with `421
//! Misdirected Request`, so no second origin can be backed by the app server.
//!
//! HTTP is fully handled that way. WebSockets can't be: modern webviews route
//! `ws://` through their own network stack (never through the scheme handler),
//! and even if they didn't, `laufey::SchemeExchange` is strictly HTTP one-shot
//! (`read_body` → `begin` → `write` → `finish`) — no upgrade/duplex primitive.
//!
//! To keep the memory transport for HTTP while still letting user code open a
//! WebSocket to its own server, the desktop runtime binds a narrow TCP loopback
//! that only accepts WebSocket upgrades and proxies them into the in-memory
//! listener ([`proxy_ws_connection`] below). The relay is reachable by any
//! local process and by any page in any browser on the machine, so it admits
//! an upgrade only when its `Origin` header is exactly the app origin — a
//! browser page elsewhere cannot forge that header, which closes the
//! cross-site WebSocket hijacking hole a plain loopback listener has. (A local
//! native process can still set any header; the relay is not a boundary
//! against other software running as the same user — no loopback listener
//! is.) Because the page's origin no longer carries the loopback address, the
//! relay's `ws://127.0.0.1:PORT` address is published to the app's Deno code
//! through [`WS_ORIGIN_ENV`] (set before user code runs), and the page origin
//! through [`APP_ORIGIN_ENV`]; the app hands them to the page however it
//! likes. Plain HTTP against the loopback is rejected with 400 so the proxy is
//! WebSocket-only — regular requests still have to come through the scheme
//! handler.
//!
//! # What the `Deno.serve` handler sees
//!
//! Requests that arrived through the in-process transport — from the scheme
//! handler or the WebSocket relay — have `info.remoteAddr` equal to
//! `{ transport: "memory", name: "deno-desktop" }`, and a `request.url` whose
//! scheme is `http+memory:` (`new URL(request.url).protocol ===
//! "http+memory:"`). Gate privileged, desktop-only endpoints on the
//! transport (`info.remoteAddr.transport === "memory"`), and treat the URL
//! as a second check, not the proof: `request.url` takes its scheme from an
//! absolute-form request target (`POST http+memory://app/x HTTP/1.1`), which
//! any client can send. `Deno.serve` rejects such a target with 400 on every
//! listener that is not the memory transport (and HTTP/2's `:scheme` the
//! same way), so over TCP the URL cannot carry the scheme either, but only
//! the transport is established by the connection itself. Within that class:
//!
//! * a page request from the scheme handler has `Host: <origin host>` (so
//!   `request.url` is `http+memory://<host>/path`) and, for same-origin
//!   fetches, NO `Origin` header — the browser omits it as it does for any
//!   same-origin `GET`;
//! * a relayed WebSocket upgrade has `Host: 127.0.0.1:<relay port>`, an
//!   `Origin` header equal to the app origin (the relay guarantees it), and
//!   exactly one [`RELAY_MARKER_HEADER`] header, `x-deno-desktop-relay: 1`.
//!
//! # Telling relayed connections apart
//!
//! The relay is reachable by every local process, and a native one can send
//! the app origin as `Origin`, so the app must treat a relayed request as
//! less trusted than a page request: it may only be a WebSocket upgrade.
//! The contract, on the memory transport (`info.remoteAddr.transport ===
//! "memory"`):
//!
//! * a request carrying [`RELAY_MARKER_HEADER`] came through the loopback
//!   relay. The relay removes every copy the client sent and adds exactly
//!   one, `x-deno-desktop-relay: 1`, to every request it forwards;
//! * a request without it came from the scheme handler (the app's page): the
//!   bridge removes the header, in any case, from every request the webview
//!   delivers, and from every response it returns.
//!
//! The relay forwards only the client's request head (never bytes pipelined
//! after it), then reads the server's response head: anything but `101
//! Switching Protocols` is replaced with the relay's own `502` and the
//! connection is closed, so a relayed connection never carries a non-101
//! response from the app nor a second request.
//!
//! The scheme handler buffers a request body of at most
//! [`MAX_BRIDGE_REQUEST_BODY`] bytes (`413` beyond it).

use std::net::SocketAddr;

use deno_lib::standalone::app_origin::AppOrigin;
use deno_net::memory::connect_memory;
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::header::CONNECTION;
use hyper::header::HOST;
use hyper::header::PROXY_AUTHENTICATE;
use hyper::header::PROXY_AUTHORIZATION;
use hyper::header::TE;
use hyper::header::TRAILER;
use hyper::header::TRANSFER_ENCODING;
use hyper::header::UPGRADE;
use hyper_util::rt::TokioIo;
use laufey::SchemeRequest;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::net::TcpStream;

/// Name of the in-process memory listener the desktop app serves on. Shared
/// with the `DENO_SERVE_ADDRESS=memory:<name>` published at startup.
pub const DESKTOP_SERVE_NAME: &str = "deno-desktop";

/// Environment variable through which the app's Deno code learns the page
/// origin (`<scheme>://<host>`, e.g. `myapp://app`) — the value of
/// `location.origin` in the webview and of the `Origin` header the page
/// sends. Set by `laufey::main!` before the runtime — and therefore before
/// user code — starts.
pub const APP_ORIGIN_ENV: &str = "DENO_DESKTOP_APP_ORIGIN";

/// Environment variable through which the app's Deno code learns the
/// WebSocket-only loopback relay address (`ws://127.0.0.1:<port>`). Set by
/// `laufey::main!` before the runtime — and therefore before user code —
/// starts.
pub const WS_ORIGIN_ENV: &str = "DENO_DESKTOP_WS_ORIGIN";

/// The request header that marks a connection from the WebSocket loopback
/// relay (see the module docs). Lower-case: header names are compared
/// case-insensitively.
pub const RELAY_MARKER_HEADER: &str = "x-deno-desktop-relay";

/// The largest request body the scheme handler forwards to `Deno.serve`; a
/// larger one is answered with `413 Payload Too Large`. Matches denext's
/// desktop bridge limit.
pub const MAX_BRIDGE_REQUEST_BODY: usize = 4 * 1024 * 1024;

/// The `ws://` origin the page dials to reach the relay bound at `addr`.
pub fn ws_relay_origin(addr: SocketAddr) -> String {
  format!("ws://127.0.0.1:{}", addr.port())
}

type BridgeError = Box<dyn std::error::Error + Send + Sync>;

/// Register the custom scheme handler for `origin`'s scheme on the current
/// tokio runtime. Each request is bridged on its own spawned task so the
/// laufey IO thread is never blocked. Must be called from within the Deno
/// tokio runtime context, and before the first webview is created (WebKit
/// reads URL-scheme handlers from the web view's configuration at creation).
pub fn register(origin: AppOrigin) {
  let handle = tokio::runtime::Handle::current();
  let scheme = origin.scheme().to_owned();
  laufey::register_scheme_handler(&scheme, move |req| {
    log::trace!("[desktop] {} {}", req.method, req.url);
    handle.spawn(handle_request(req, origin.clone()));
  });
}

async fn handle_request(req: SchemeRequest, origin: AppOrigin) {
  let exchange = req.exchange;
  let mut began = false;
  if let Err(e) = Box::pin(bridge(
    &req.method,
    &req.url,
    &req.headers,
    &origin,
    &exchange,
    &mut began,
  ))
  .await
  {
    log::error!("[desktop] {}:// bridge error: {e}", origin.scheme());
    if !began {
      // Surface a minimal error page if we never sent a response head.
      exchange.begin(
        502,
        &[(
          "content-type".to_string(),
          "text/plain; charset=utf-8".to_string(),
        )],
      );
      let _ =
        exchange.write(format!("desktop transport error: {e}").as_bytes());
    }
  }
  exchange.finish();
}

async fn bridge(
  method: &str,
  url: &str,
  headers: &[(String, String)],
  origin: &AppOrigin,
  exchange: &laufey::SchemeExchange,
  began: &mut bool,
) -> Result<(), BridgeError> {
  // The bridge backs exactly one origin. A URL on the app's scheme but with
  // another authority is a different origin to the browser (an opaque host is
  // compared byte-for-byte), and serving it would let app content run at an
  // origin nobody configured, so it is refused up front.
  if !authority_matches(url, origin) {
    exchange.begin(
      421,
      &[(
        "content-type".to_string(),
        "text/plain; charset=utf-8".to_string(),
      )],
    );
    *began = true;
    let _ = exchange.write(
      format!("misdirected request: this app is served at {origin}\n")
        .as_bytes(),
    );
    return Ok(());
  }

  // The request body is fully buffered by the backend, so these pulls are
  // non-blocking copies.
  let mut body = Vec::new();
  let mut buf = [0u8; 16 * 1024];
  loop {
    let n = exchange.read_body(&mut buf);
    if n <= 0 {
      break;
    }
    if body_exceeds_limit(body.len(), n as usize) {
      exchange.begin(
        413,
        &[(
          "content-type".to_string(),
          "text/plain; charset=utf-8".to_string(),
        )],
      );
      *began = true;
      let _ = exchange.write(
        format!("request body larger than {MAX_BRIDGE_REQUEST_BODY} bytes\n")
          .as_bytes(),
      );
      return Ok(());
    }
    body.extend_from_slice(&buf[..n as usize]);
  }

  // Open a fresh in-process connection to the Deno.serve listener and drive an
  // HTTP/1.1 client over it.
  let stream = connect_memory(DESKTOP_SERVE_NAME)?;
  let io = TokioIo::new(stream);
  let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;
  tokio::spawn(async move {
    let _ = conn.await;
  });

  let mut builder = hyper::Request::builder()
    .method(method)
    .uri(path_and_query(url));
  for (name, value) in headers {
    if should_skip_request_header(name) {
      continue;
    }
    builder = builder.header(name.as_str(), value.as_str());
  }
  // The authority the page addressed is the origin's host (checked above), so
  // `request.url` on the Deno side reads `http+memory://<host>/...`.
  builder = builder.header(HOST, origin.host());
  let request = builder.body(Full::new(bytes::Bytes::from(body)))?;

  // The webview may cancel the request (navigation away, an aborted fetch,
  // the window closing) while the app is still working on it: drop the
  // request then, which closes the memory connection, so the app's
  // `request.signal` aborts instead of the app computing a response nobody
  // reads (a long poll, a slow render). `write()` failing only told the
  // bridge once a body chunk arrived.
  let response =
    match until_cancelled(exchange, sender.send_request(request)).await {
      Some(response) => response?,
      None => return Ok(()),
    };

  let status = response.status().as_u16() as i32;
  let mut resp_headers = Vec::with_capacity(response.headers().len());
  for (name, value) in response.headers() {
    if is_hop_by_hop_header(name.as_str()) || is_relay_marker(name.as_str()) {
      continue;
    }
    resp_headers.push((
      name.as_str().to_string(),
      response_header_value(name.as_str(), value.as_bytes(), origin),
    ));
  }
  exchange.begin(status, &resp_headers);
  *began = true;

  let mut body = response.into_body();
  while let Some(frame) = until_cancelled(exchange, body.frame()).await {
    let Some(frame) = frame else {
      break;
    };
    let frame = frame?;
    if let Some(chunk) = frame.data_ref() {
      // The next frame is only pulled once the webview took this one, so a
      // slow reader holds the server back instead of the backend buffering
      // an endless stream (server-sent events, a large download).
      if !write_all(exchange, chunk.as_ref()).await {
        // The webview cancelled / went away.
        break;
      }
    }
  }

  Ok(())
}

/// How often a bridged request checks whether the webview cancelled it.
const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// `future`'s output, or `None` once the webview cancelled the exchange
/// (laufey's `on_cancel`; see [`CancelSource`]). Backends that can't report
/// a cancel in some state never do, and a failed `write` remains the signal
/// there.
async fn until_cancelled<T>(
  exchange: &impl CancelSource,
  future: impl std::future::Future<Output = T>,
) -> Option<T> {
  let mut future = std::pin::pin!(future);
  let mut poll = tokio::time::interval(CANCEL_POLL);
  poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
  loop {
    tokio::select! {
      out = &mut future => return Some(out),
      _ = poll.tick() => {
        if exchange.is_cancelled() {
          return None;
        }
      }
    }
  }
}

/// Whether the webview cancelled the request (a fake in tests).
trait CancelSource {
  fn is_cancelled(&self) -> bool;
}

impl CancelSource for laufey::SchemeExchange {
  fn is_cancelled(&self) -> bool {
    laufey::SchemeExchange::is_cancelled(self)
  }
}

/// Where a response body chunk goes (the webview's exchange; a fake in
/// tests).
trait ResponseSink {
  /// Bytes accepted (possibly fewer than offered, possibly 0 while the
  /// consumer is full), or negative once the consumer has gone away.
  fn write(&self, buf: &[u8]) -> isize;
}

impl ResponseSink for laufey::SchemeExchange {
  fn write(&self, buf: &[u8]) -> isize {
    laufey::SchemeExchange::write(self, buf)
  }
}

/// Hand all of `chunk` to `sink`, waiting (with a growing pause, at most
/// [`WRITE_RETRY_MAX`]) while it accepts nothing. A short write used to drop
/// the rest of the chunk. False once the consumer has gone away.
async fn write_all(sink: &impl ResponseSink, mut chunk: &[u8]) -> bool {
  let mut pause = WRITE_RETRY_MIN;
  while !chunk.is_empty() {
    let n = sink.write(chunk);
    if n < 0 {
      return false;
    }
    let n = (n as usize).min(chunk.len());
    if n == 0 {
      tokio::time::sleep(pause).await;
      pause = (pause * 2).min(WRITE_RETRY_MAX);
      continue;
    }
    chunk = &chunk[n..];
    pause = WRITE_RETRY_MIN;
  }
  true
}

const WRITE_RETRY_MIN: std::time::Duration =
  std::time::Duration::from_millis(1);
const WRITE_RETRY_MAX: std::time::Duration =
  std::time::Duration::from_millis(50);

/// A response header's value as the webview gets it.
///
/// Header values are bytes; a non-ASCII one (a UTF-8 `filename` in
/// `content-disposition`, a localized `x-*` header) used to be dropped
/// because `HeaderValue::to_str` only takes visible ASCII. It is decoded as
/// UTF-8 when it is, else as Latin-1 (what browsers do with raw header
/// bytes).
///
/// The app's `request.url` is `http+memory://<host>/…`, so an absolute URL
/// the app builds from it (`Response.redirect(new URL("/login", req.url))`)
/// points at a scheme the webview cannot load. In `location`,
/// `content-location` and `refresh` such a URL is rewritten onto the app
/// origin's scheme.
fn response_header_value(
  name: &str,
  value: &[u8],
  origin: &AppOrigin,
) -> String {
  let text = match std::str::from_utf8(value) {
    Ok(text) => text.to_string(),
    Err(_) => value.iter().map(|&b| b as char).collect(),
  };
  if name.eq_ignore_ascii_case("location")
    || name.eq_ignore_ascii_case("content-location")
  {
    return rewrite_memory_url(&text, origin).unwrap_or(text);
  }
  if name.eq_ignore_ascii_case("refresh") {
    // `<seconds>; url=<url>` (the `url=` part may be quoted or absent).
    let lower = text.to_ascii_lowercase();
    if let Some(i) = lower.find("url=") {
      let start = i + "url=".len();
      let (quote, start) = match text[start..].chars().next() {
        Some(q @ ('\'' | '"')) => (Some(q), start + 1),
        _ => (None, start),
      };
      let end = quote
        .and_then(|q| text[start..].find(q).map(|j| start + j))
        .unwrap_or(text.len());
      if let Some(url) = rewrite_memory_url(&text[start..end], origin) {
        return format!("{}{url}{}", &text[..start], &text[end..]);
      }
    }
  }
  text
}

/// `http+memory://<authority>/rest` -> `<app scheme>://<authority>/rest`;
/// `None` for any other URL.
///
/// The prefix is compared as bytes: the value is whatever the app put in a
/// response header (decoded from UTF-8 or Latin-1), so the byte at the prefix
/// length may be inside a multi-byte character, and slicing the `&str` there
/// panicked, which exits the app.
fn rewrite_memory_url(url: &str, origin: &AppOrigin) -> Option<String> {
  let rest =
    strip_prefix_ignore_ascii_case(url.trim_start(), "http+memory://")?;
  Some(format!("{}://{rest}", origin.scheme()))
}

/// `text` without `prefix` (ASCII, compared ASCII case-insensitively), or
/// `None` when `text` does not start with it. `text` is only ever split right
/// after a matched ASCII prefix, which is always a character boundary.
fn strip_prefix_ignore_ascii_case<'a>(
  text: &'a str,
  prefix: &str,
) -> Option<&'a str> {
  let head = text.as_bytes().get(..prefix.len())?;
  if prefix.is_ascii() && head.eq_ignore_ascii_case(prefix.as_bytes()) {
    text.get(prefix.len()..)
  } else {
    None
  }
}

/// Split `scheme://authority[/path][?query][#fragment]` into
/// `(authority, rest-after-authority)`. `None` if there is no `://`.
fn split_authority(url: &str) -> Option<(&str, &str)> {
  let (_, rest) = url.split_once("://")?;
  let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
  Some((&rest[..end], &rest[end..]))
}

/// The authority (`host[:port]`) of a `scheme://authority/...` URL, if any.
fn authority(url: &str) -> Option<&str> {
  split_authority(url)
    .map(|(auth, _)| auth)
    .filter(|a| !a.is_empty())
}

/// Whether the request URL's authority is exactly the origin's host. The
/// scheme is not re-checked: the webview only delivers URLs on the scheme the
/// handler was registered for.
fn authority_matches(url: &str, origin: &AppOrigin) -> bool {
  authority(url) == Some(origin.host())
}

/// Extract the path-and-query from a `scheme://authority/path?query` URL, the
/// form a hyper request target needs. Everything after the authority is the
/// target; a bare authority is `/`; a query with no path gets a `/` prepended;
/// a fragment is dropped (a browser never sends one, but `SchemeRequest::url`
/// is whatever WebKit saw).
fn path_and_query(url: &str) -> String {
  match split_authority(url) {
    Some((_, rest)) => {
      let rest = match rest.find('#') {
        Some(i) => &rest[..i],
        None => rest,
      };
      if rest.is_empty() {
        "/".to_string()
      } else if rest.starts_with('?') {
        format!("/{rest}")
      } else {
        rest.to_string()
      }
    }
    None => url.to_string(),
  }
}

fn should_skip_request_header(name: &str) -> bool {
  name.eq_ignore_ascii_case(HOST.as_str())
    || is_hop_by_hop_header(name)
    || is_relay_marker(name)
    // The body is forwarded fully buffered (`Full`), and hyper sets the
    // length of what is actually sent. Forwarding the webview's own
    // `content-length` could contradict it (a body the backend read short,
    // or one it was never given), and `expect: 100-continue` would make the
    // server wait for a continuation that is never asked for.
    || name.eq_ignore_ascii_case("content-length")
    || name.eq_ignore_ascii_case("expect")
}

/// Whether a header name is [`RELAY_MARKER_HEADER`] (any case, surrounding
/// whitespace ignored).
fn is_relay_marker(name: &str) -> bool {
  name.trim().eq_ignore_ascii_case(RELAY_MARKER_HEADER)
}

/// Whether a body of `have` bytes plus a chunk of `next` exceeds
/// [`MAX_BRIDGE_REQUEST_BODY`].
fn body_exceeds_limit(have: usize, next: usize) -> bool {
  have.saturating_add(next) > MAX_BRIDGE_REQUEST_BODY
}

fn is_hop_by_hop_header(name: &str) -> bool {
  name.eq_ignore_ascii_case(CONNECTION.as_str())
    || name.eq_ignore_ascii_case("keep-alive")
    || name.eq_ignore_ascii_case(PROXY_AUTHENTICATE.as_str())
    || name.eq_ignore_ascii_case(PROXY_AUTHORIZATION.as_str())
    || name.eq_ignore_ascii_case(TE.as_str())
    || name.eq_ignore_ascii_case(TRAILER.as_str())
    || name.eq_ignore_ascii_case(TRANSFER_ENCODING.as_str())
    || name.eq_ignore_ascii_case(UPGRADE.as_str())
}

// --- WebSocket loopback proxy ------------------------------------------------

/// Bind the WebSocket-only loopback relay on `127.0.0.1:0` as a blocking std
/// listener. Called by `laufey::main!` BEFORE the tokio runtime exists so the
/// chosen port can be published to user code via [`WS_ORIGIN_ENV`] while the
/// process is still single-threaded (`setenv` safety). The listener is handed
/// to [`spawn_ws_loopback_proxy`] once the runtime is up.
pub fn bind_ws_loopback_listener() -> std::io::Result<std::net::TcpListener> {
  std::net::TcpListener::bind("127.0.0.1:0")
}

/// Start the relay's accept loop on a listener from
/// [`bind_ws_loopback_listener`]. Must be called from within the Deno tokio
/// runtime context.
///
/// The proxy is deliberately narrow — see [`classify_upgrade`]: every accepted
/// connection must present a WebSocket handshake whose `Origin` is exactly
/// `origin`, or it is answered with a 400 / 403 and closed. That keeps plain
/// HTTP off the loopback (the memory transport remains the only path in for
/// regular requests) and keeps pages from other origins out.
pub fn spawn_ws_loopback_proxy(
  listener: std::net::TcpListener,
  origin: AppOrigin,
) {
  if let Err(e) = listener.set_nonblocking(true) {
    log::error!("[desktop] ws relay: set_nonblocking failed: {e}");
    return;
  }
  let listener = match TcpListener::from_std(listener) {
    Ok(l) => l,
    Err(e) => {
      log::error!("[desktop] ws relay: tokio listener conversion failed: {e}");
      return;
    }
  };
  if let Ok(addr) = listener.local_addr() {
    log::debug!("[desktop] ws relay listening on {addr} for origin {origin}");
  }
  tokio::spawn(run_ws_loopback_proxy(listener, origin));
}

/// The relay's accept loop: one task per connection.
async fn run_ws_loopback_proxy(listener: TcpListener, origin: AppOrigin) {
  loop {
    match listener.accept().await {
      Ok((stream, _peer)) => {
        let origin = origin.clone();
        tokio::spawn(async move {
          if let Err(e) = proxy_ws_connection(stream, &origin).await {
            log::debug!("[desktop] ws proxy connection error: {e}");
          }
        });
      }
      Err(e) => {
        log::error!("[desktop] ws proxy accept failed: {e}");
        // Backoff a beat so we don't tight-loop on a broken listener.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
      }
    }
  }
}

/// Largest request head the relay will buffer before giving up on a client.
/// WebSocket handshakes are small; 8 KiB is plenty for a real browser upgrade.
const MAX_HEAD_LEN: usize = 8 * 1024;

/// How long the relay waits for a client's complete request head. A browser
/// sends its WebSocket handshake at once, but Chromium also opens idle
/// "preconnect" sockets to origins it has learned and may never write to
/// them; without a deadline each such socket would hold a relay task (and a
/// file descriptor) for the life of the app. The deadline covers the whole
/// head, so a client trickling bytes cannot extend it either.
const HEAD_READ_TIMEOUT: std::time::Duration =
  std::time::Duration::from_secs(10);

/// How long the relay waits for `Deno.serve`'s answer to a forwarded upgrade
/// (the app's handler may do async work before it upgrades).
const UPSTREAM_HEAD_TIMEOUT: std::time::Duration =
  std::time::Duration::from_secs(30);

/// What the relay does with a connection, decided from its request head.
#[derive(Debug, PartialEq, Eq)]
enum RelayDecision {
  /// A WebSocket upgrade from the app origin: proxy it to `Deno.serve`.
  Proxy,
  /// Not a `GET` WebSocket upgrade at all (plain HTTP, a port scan, …).
  NotWebSocket,
  /// A WebSocket upgrade whose `Origin` is missing, repeated, or not the app
  /// origin: a page somewhere else on this machine is trying to reach the
  /// app's server.
  ForbiddenOrigin,
}

/// Classify a request head (everything up to and including the blank line).
///
/// The `Origin` header is the browser-controlled part of a WebSocket
/// handshake: a page cannot set it, and the browser fills it with the page's
/// own origin. Requiring it to equal the app origin byte-for-byte therefore
/// admits only the app's own page. Exactly one `Origin` header is required —
/// a duplicated header is how a proxy or a confused client smuggles a second
/// value past a check that only looks at the first.
fn classify_upgrade(head: &[u8], origin: &AppOrigin) -> RelayDecision {
  if !is_get_request(head) || !is_websocket_upgrade(head) {
    return RelayDecision::NotWebSocket;
  }
  let mut origins = header_values(head, b"origin");
  let (Some(value), None) = (origins.next(), origins.next()) else {
    return RelayDecision::ForbiddenOrigin;
  };
  match std::str::from_utf8(value) {
    Ok(value) if origin.matches_origin_header(value) => RelayDecision::Proxy,
    _ => RelayDecision::ForbiddenOrigin,
  }
}

async fn proxy_ws_connection(
  mut tcp: TcpStream,
  origin: &AppOrigin,
) -> std::io::Result<()> {
  let Some((head, end_of_head)) =
    read_request_head(&mut tcp, HEAD_READ_TIMEOUT).await?
  else {
    return Ok(());
  };

  match classify_upgrade(&head[..end_of_head], origin) {
    RelayDecision::Proxy => {}
    RelayDecision::NotWebSocket => {
      let _ = tcp.write_all(RELAY_400_RESPONSE).await;
      let _ = tcp.shutdown().await;
      return Ok(());
    }
    RelayDecision::ForbiddenOrigin => {
      log::debug!(
        "[desktop] ws relay: refused upgrade with Origin {:?} (expected {origin})",
        header_values(&head[..end_of_head], b"origin")
          .next()
          .map(String::from_utf8_lossy)
      );
      let _ = tcp.write_all(RELAY_403_RESPONSE).await;
      let _ = tcp.shutdown().await;
      return Ok(());
    }
  }

  // Open the in-process connection to Deno.serve and forward the upgrade.
  let mut mem = match connect_memory(DESKTOP_SERVE_NAME) {
    Ok(s) => s,
    Err(e) => {
      log::warn!("[desktop] ws proxy: memory connect failed: {e}");
      return Ok(());
    }
  };
  relay_upgrade(
    &mut tcp,
    &mut mem,
    &head[..end_of_head],
    UPSTREAM_HEAD_TIMEOUT,
  )
  .await
}

/// Forward one admitted upgrade: only the request `head` (rewritten by
/// [`relay_request_head`]; bytes the client pipelined after it are dropped),
/// then the server's response. Only a `101` is passed on, after which bytes
/// are shuttled both ways; any other answer (or none within `timeout`) is
/// replaced with the relay's `502` and both sides are closed, so the client
/// can neither read the app's non-upgrade response nor send a second request
/// on the connection.
async fn relay_upgrade<C, U>(
  client: &mut C,
  upstream: &mut U,
  head: &[u8],
  timeout: std::time::Duration,
) -> std::io::Result<()>
where
  C: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
  U: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
  upstream.write_all(&relay_request_head(head)).await?;
  let response = read_response_head(upstream, timeout).await?;
  match response {
    Some((buf, end)) if is_switching_protocols(&buf[..end]) => {
      // The 101 head and any frames the server already sent after it.
      client.write_all(&buf).await?;
      let _ = tokio::io::copy_bidirectional(client, upstream).await;
    }
    response => {
      log::debug!(
        "[desktop] ws relay: the server did not switch protocols ({:?}); \
         closing the connection",
        response.map(|(buf, end)| {
          let line_end =
            buf[..end].iter().position(|&b| b == b'\r').unwrap_or(end);
          String::from_utf8_lossy(&buf[..line_end]).into_owned()
        })
      );
      let _ = client.write_all(RELAY_502_RESPONSE).await;
      let _ = client.shutdown().await;
      let _ = upstream.shutdown().await;
    }
  }
  Ok(())
}

/// The request head the relay sends upstream: the client's request line and
/// headers, minus every [`RELAY_MARKER_HEADER`] the client sent and header
/// lines without a colon, plus exactly one `x-deno-desktop-relay: 1`.
fn relay_request_head(head: &[u8]) -> Vec<u8> {
  let mut out = Vec::with_capacity(head.len() + 32);
  let mut lines = head
    .split(|&b| b == b'\n')
    .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
    .filter(|line| !line.is_empty());
  if let Some(request_line) = lines.next() {
    out.extend_from_slice(request_line);
    out.extend_from_slice(b"\r\n");
  }
  for line in lines {
    let Some(colon) = line.iter().position(|&b| b == b':') else {
      continue;
    };
    if trim_ascii(&line[..colon])
      .eq_ignore_ascii_case(RELAY_MARKER_HEADER.as_bytes())
    {
      continue;
    }
    out.extend_from_slice(line);
    out.extend_from_slice(b"\r\n");
  }
  out.extend_from_slice(RELAY_MARKER_HEADER.as_bytes());
  out.extend_from_slice(b": 1\r\n\r\n");
  out
}

/// Whether a response head's status is `101` (`HTTP/1.1 101 …`).
fn is_switching_protocols(head: &[u8]) -> bool {
  let Some(rest) = head.strip_prefix(b"HTTP/1.1 101") else {
    return false;
  };
  matches!(rest.first(), Some(b' ' | b'\r'))
}

/// Read `Deno.serve`'s response head, like [`read_request_head`].
async fn read_response_head<R: tokio::io::AsyncRead + Unpin>(
  upstream: &mut R,
  timeout: std::time::Duration,
) -> std::io::Result<Option<(Vec<u8>, usize)>> {
  read_request_head(upstream, timeout).await
}

/// Read the client's request head: the bytes read so far and the offset one
/// past its blank line. Cheaper than fully parsing HTTP, and all the relay
/// needs is the request line and a few headers. `None` when the client closes
/// first, sends a head larger than [`MAX_HEAD_LEN`], or does not finish it
/// within `timeout`; the relay then drops the connection without sending an
/// HTTP response, so a plain-HTTP scan does not learn that `Deno.serve` is
/// behind it.
async fn read_request_head<R: tokio::io::AsyncRead + Unpin>(
  tcp: &mut R,
  timeout: std::time::Duration,
) -> std::io::Result<Option<(Vec<u8>, usize)>> {
  let read = async {
    let mut head = Vec::with_capacity(2048);
    let mut buf = [0u8; 2048];
    loop {
      let n = tcp.read(&mut buf).await?;
      if n == 0 {
        return Ok(None);
      }
      head.extend_from_slice(&buf[..n]);
      if let Some(idx) = find_end_of_head(&head) {
        return Ok(Some((head, idx)));
      }
      if head.len() >= MAX_HEAD_LEN {
        return Ok(None);
      }
    }
  };
  match tokio::time::timeout(timeout, read).await {
    Ok(result) => result,
    Err(_elapsed) => {
      log::debug!(
        "[desktop] ws relay: no complete request head within {timeout:?}; \
         closing the connection"
      );
      Ok(None)
    }
  }
}

const RELAY_400_RESPONSE: &[u8] = b"HTTP/1.1 400 Bad Request\r\n\
  content-type: text/plain; charset=utf-8\r\n\
  content-length: 45\r\n\
  connection: close\r\n\r\n\
  desktop ws relay: WebSocket upgrade required\n";

const RELAY_403_RESPONSE: &[u8] = b"HTTP/1.1 403 Forbidden\r\n\
  content-type: text/plain; charset=utf-8\r\n\
  content-length: 47\r\n\
  connection: close\r\n\r\n\
  desktop ws relay: Origin is not the app origin\n";

const RELAY_502_RESPONSE: &[u8] = b"HTTP/1.1 502 Bad Gateway\r\n\
  content-type: text/plain; charset=utf-8\r\n\
  content-length: 50\r\n\
  connection: close\r\n\r\n\
  desktop ws relay: the server did not upgrade this\n";

/// Byte offset of the request head/body boundary (`\r\n\r\n`) — the index one
/// past the final `\n`.
fn find_end_of_head(buf: &[u8]) -> Option<usize> {
  buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// Whether the request line's method is `GET` — the only method a WebSocket
/// handshake uses (RFC 6455 §4.1).
fn is_get_request(head: &[u8]) -> bool {
  head.starts_with(b"GET ")
}

/// Iterate the `(name, value)` header lines of a request head, skipping the
/// request line. Values have surrounding whitespace trimmed; names are raw.
fn header_lines(head: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
  let mut lines = head
    .split(|&b| b == b'\n')
    .map(|line| line.strip_suffix(b"\r").unwrap_or(line));
  // Drop the request line.
  lines.next();
  lines.filter_map(|line| {
    let colon = line.iter().position(|&b| b == b':')?;
    let name = &line[..colon];
    let value = trim_ascii(&line[colon + 1..]);
    Some((name, value))
  })
}

/// Every value of header `name` (ASCII case-insensitive), in order.
fn header_values<'a>(
  head: &'a [u8],
  name: &'a [u8],
) -> impl Iterator<Item = &'a [u8]> + 'a {
  header_lines(head)
    .filter(move |(n, _)| n.eq_ignore_ascii_case(name))
    .map(|(_, v)| v)
}

/// Cheap header check: does the request head carry `Upgrade: websocket`? Case
/// is normalized because header names are ASCII-insensitive and browsers send
/// the value lowercase in practice but we don't want to depend on that.
fn is_websocket_upgrade(head: &[u8]) -> bool {
  header_values(head, b"upgrade").any(|value| {
    // The Upgrade header can list multiple protocols; a browser only ever
    // sends `websocket`, but be forgiving of extra tokens/whitespace.
    value
      .split(|&b| b == b',')
      .any(|tok| trim_ascii(tok).eq_ignore_ascii_case(b"websocket"))
  })
}

fn trim_ascii(mut b: &[u8]) -> &[u8] {
  while let [b' ' | b'\t', rest @ ..] = b {
    b = rest;
  }
  while let [rest @ .., b' ' | b'\t'] = b {
    b = rest;
  }
  b
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use std::sync::atomic::AtomicUsize;
  use std::sync::atomic::Ordering;
  use std::time::Duration;

  use super::*;

  fn origin() -> AppOrigin {
    AppOrigin::parse("t3code://app").unwrap()
  }

  /// A connected (client, relay-side) TCP pair on loopback.
  async fn tcp_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (client, accepted) =
      tokio::join!(TcpStream::connect(addr), listener.accept());
    (client.unwrap(), accepted.unwrap().0)
  }

  #[test]
  fn request_headers_the_bridge_recomputes_are_not_forwarded() {
    for name in ["Content-Length", "expect", "host", "Connection"] {
      assert!(should_skip_request_header(name), "{name}");
    }
    for name in ["content-type", "cookie", "origin", "accept"] {
      assert!(!should_skip_request_header(name), "{name}");
    }
  }

  #[test]
  fn response_headers_keep_non_ascii_and_point_at_the_app_origin() {
    let o = origin();
    // UTF-8 kept (it used to be dropped), Latin-1 decoded.
    assert_eq!(
      response_header_value(
        "content-disposition",
        "attachment; filename=\"résumé.pdf\"".as_bytes(),
        &o
      ),
      "attachment; filename=\"résumé.pdf\""
    );
    assert_eq!(response_header_value("x-name", b"caf\xe9", &o), "café");
    // A redirect built from request.url lands on the app origin.
    assert_eq!(
      response_header_value("Location", b"http+memory://app/login?x=1", &o),
      "t3code://app/login?x=1"
    );
    assert_eq!(
      response_header_value("content-location", b"HTTP+MEMORY://app/a", &o),
      "t3code://app/a"
    );
    assert_eq!(
      response_header_value("refresh", b"5; url=http+memory://app/next", &o),
      "5; url=t3code://app/next"
    );
    assert_eq!(
      response_header_value("refresh", b"0;URL='http+memory://app/q'", &o),
      "0;URL='t3code://app/q'"
    );
    // Anything else is left alone.
    for (name, value) in [
      ("location", "/relative"),
      ("location", "https://idp.example/authorize"),
      ("refresh", "5"),
      ("link", "<http+memory://app/x>; rel=preload"),
      // A multi-byte character across the prefix length (14 bytes) used to
      // panic the runtime thread (`&str` sliced inside a character).
      ("location", "http+memoré://app/x"),
      ("location", "ééééééééééééééé"),
      ("location", "日本語のパス/x"),
      ("location", "http+memory:/日本"),
      ("content-location", "/données/é"),
      ("content-location", "  ééééééé"),
      ("refresh", "5; url=ééééééééééééééé"),
      ("refresh", "5; url='日本語のパス'"),
      ("refresh", "5; URL=\"http+memoré://app/\""),
      ("refresh", "é; url=/x"),
      ("refresh", "5; url='ééé"),
    ] {
      assert_eq!(response_header_value(name, value.as_bytes(), &o), value);
    }
    // Latin-1 bytes (decoded to two-byte characters) in the same places.
    assert_eq!(
      response_header_value(
        "location",
        b"\xe9\xe9\xe9\xe9\xe9\xe9\xe9\xe9",
        &o
      ),
      "éééééééé"
    );
    assert_eq!(
      response_header_value("refresh", b"0; url=http+memor\xe9://app/", &o),
      "0; url=http+memoré://app/"
    );
    // Non-ASCII after a real prefix is kept as it is.
    assert_eq!(
      response_header_value(
        "location",
        "http+memory://app/données?q=日本".as_bytes(),
        &o
      ),
      "t3code://app/données?q=日本"
    );
    assert_eq!(
      response_header_value(
        "refresh",
        "1; url=\"http+memory://app/é\"".as_bytes(),
        &o
      ),
      "1; url=\"t3code://app/é\""
    );
  }

  #[test]
  fn strip_prefix_ignore_ascii_case_never_splits_a_character() {
    assert_eq!(
      strip_prefix_ignore_ascii_case("HTTP+memory://x", "http+memory://"),
      Some("x")
    );
    assert_eq!(
      strip_prefix_ignore_ascii_case("http+memory:/", "http+memory://"),
      None
    );
    assert_eq!(strip_prefix_ignore_ascii_case("", "http+memory://"), None);
    // Every split position of a string of 2-, 3- and 4-byte characters.
    for text in [
      "éééééééééé",
      "日本語日本語日本語",
      "😀😀😀😀😀",
      "aé日😀aé日😀",
    ] {
      for len in 1..=text.len() {
        let prefix = "x".repeat(len);
        assert_eq!(strip_prefix_ignore_ascii_case(text, &prefix), None);
      }
    }
  }

  struct FakeSink {
    accepted: std::cell::RefCell<Vec<u8>>,
    /// What each write accepts at most, in turn (then everything).
    script: std::cell::RefCell<std::collections::VecDeque<isize>>,
  }

  impl ResponseSink for FakeSink {
    fn write(&self, buf: &[u8]) -> isize {
      let cap = self.script.borrow_mut().pop_front().unwrap_or(isize::MAX);
      if cap < 0 {
        return cap;
      }
      let n = buf.len().min(cap as usize);
      self.accepted.borrow_mut().extend_from_slice(&buf[..n]);
      n as isize
    }
  }

  struct FakeCancel(std::sync::atomic::AtomicBool);

  impl CancelSource for FakeCancel {
    fn is_cancelled(&self) -> bool {
      self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
  }

  #[tokio::test]
  async fn a_cancelled_request_stops_waiting_for_the_app() {
    // An app that never answers (a long poll): the bridge used to wait for
    // it forever after the webview gave up.
    let cancel = Arc::new(FakeCancel(false.into()));
    let flag = cancel.clone();
    tokio::spawn(async move {
      tokio::time::sleep(Duration::from_millis(150)).await;
      flag.0.store(true, Ordering::SeqCst);
    });
    let started = std::time::Instant::now();
    let out =
      until_cancelled(cancel.as_ref(), std::future::pending::<()>()).await;
    assert!(out.is_none());
    assert!(started.elapsed() < Duration::from_secs(5));
    // Not cancelled: the future's output.
    let live = FakeCancel(false.into());
    assert_eq!(until_cancelled(&live, async { 7 }).await, Some(7));
  }

  #[tokio::test]
  async fn short_and_full_writes_deliver_the_whole_chunk() {
    // The consumer takes 3 bytes, is full twice, then takes the rest: every
    // byte arrives (a short write used to lose the remainder).
    let sink = FakeSink {
      accepted: Default::default(),
      script: std::cell::RefCell::new([3, 0, 0, 2].into()),
    };
    assert!(write_all(&sink, b"hello world").await);
    assert_eq!(&*sink.accepted.borrow(), b"hello world");
    // A consumer that went away stops the copy.
    let gone = FakeSink {
      accepted: Default::default(),
      script: std::cell::RefCell::new([4, -1].into()),
    };
    assert!(!write_all(&gone, b"hello world").await);
    assert_eq!(&*gone.accepted.borrow(), b"hell");
  }

  #[tokio::test]
  async fn head_read_times_out_on_an_idle_socket() {
    // A preconnect-style socket that never writes: the read gives up at the
    // deadline instead of holding the task forever.
    let (_client, mut server) = tcp_pair().await;
    let start = std::time::Instant::now();
    let head = read_request_head(&mut server, Duration::from_millis(200))
      .await
      .unwrap();
    assert!(head.is_none());
    let elapsed = start.elapsed();
    assert!(elapsed >= Duration::from_millis(200), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
  }

  #[tokio::test]
  async fn head_read_deadline_covers_a_trickling_client() {
    // Bytes that never complete a head do not extend the deadline.
    let (mut client, mut server) = tcp_pair().await;
    let writer = tokio::spawn(async move {
      for _ in 0..50 {
        if client.write_all(b"G").await.is_err() {
          break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
      }
      client
    });
    let start = std::time::Instant::now();
    let head = read_request_head(&mut server, Duration::from_millis(200))
      .await
      .unwrap();
    assert!(head.is_none());
    assert!(start.elapsed() < Duration::from_millis(900));
    drop(writer.await);
  }

  #[tokio::test]
  async fn head_read_returns_a_complete_head() {
    let (mut client, mut server) = tcp_pair().await;
    client
      .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\nextra")
      .await
      .unwrap();
    let (head, end) = read_request_head(&mut server, Duration::from_secs(5))
      .await
      .unwrap()
      .expect("a complete head");
    assert_eq!(&head[..end], b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    // A client that closes before finishing its head yields nothing.
    let (client, mut server) = tcp_pair().await;
    drop(client);
    assert!(
      read_request_head(&mut server, Duration::from_secs(5))
        .await
        .unwrap()
        .is_none()
    );
    assert_eq!(HEAD_READ_TIMEOUT, Duration::from_secs(10));
  }

  #[test]
  fn ws_relay_origin_is_loopback_with_port() {
    let addr: SocketAddr = "127.0.0.1:4321".parse().unwrap();
    assert_eq!(ws_relay_origin(addr), "ws://127.0.0.1:4321");
  }

  #[test]
  fn path_and_query_extraction() {
    assert_eq!(path_and_query("t3code://app/foo?bar=baz"), "/foo?bar=baz");
    assert_eq!(path_and_query("t3code://app"), "/");
    assert_eq!(path_and_query("t3code://app/"), "/");
    // Authority carrying a port shouldn't leak into the request line.
    assert_eq!(path_and_query("t3code://127.0.0.1:5173/foo"), "/foo");
    assert_eq!(path_and_query("t3code://other.host/x/y"), "/x/y");
    // Query with no path, and fragments, are normalised.
    assert_eq!(path_and_query("t3code://app?x=1"), "/?x=1");
    assert_eq!(path_and_query("t3code://app/p?x=1#frag"), "/p?x=1");
    // Non-URL input is passed through untouched.
    assert_eq!(path_and_query("/relative"), "/relative");
  }

  #[test]
  fn authority_extraction() {
    assert_eq!(authority("t3code://app/foo"), Some("app"));
    assert_eq!(
      authority("t3code://127.0.0.1:5173/"),
      Some("127.0.0.1:5173")
    );
    assert_eq!(authority("t3code://app"), Some("app"));
    assert_eq!(authority("t3code:///nohost"), None);
    assert_eq!(authority("/relative"), None);
  }

  #[test]
  fn bridge_serves_exactly_the_configured_host() {
    let origin = origin();
    assert!(authority_matches("t3code://app/", &origin));
    assert!(authority_matches("t3code://app", &origin));
    assert!(authority_matches("t3code://app/x/y?z=1#f", &origin));
    // Another host on the same scheme is another origin: refused (421).
    assert!(!authority_matches("t3code://evil/", &origin));
    assert!(!authority_matches("t3code://app.evil/", &origin));
    assert!(!authority_matches("t3code://app:8080/", &origin));
    // An opaque host is compared byte-for-byte by the browser too.
    assert!(!authority_matches("t3code://APP/", &origin));
    assert!(!authority_matches("t3code:///", &origin));
    assert!(!authority_matches("/relative", &origin));
  }

  #[test]
  fn bridge_header_filtering() {
    assert!(should_skip_request_header("host"));
    assert!(should_skip_request_header("Connection"));
    assert!(is_hop_by_hop_header("transfer-encoding"));
    assert!(!should_skip_request_header("accept-language"));
    assert!(!is_hop_by_hop_header("content-type"));
  }

  #[test]
  fn find_end_of_head_handles_split() {
    assert_eq!(find_end_of_head(b"GET / HTTP/1.1\r\n\r\n"), Some(18));
    assert_eq!(
      find_end_of_head(b"GET / HTTP/1.1\r\nHost: x\r\n\r\ntail"),
      Some(27),
    );
    assert_eq!(find_end_of_head(b"GET / HTTP/1.1\r\n"), None);
  }

  #[test]
  fn header_values_are_case_insensitive_and_ordered() {
    let head = b"GET / HTTP/1.1\r\n\
      Host: 127.0.0.1:1\r\n\
      ORIGIN: a\r\n\
      X-Other: nope\r\n\
      origin:\t b \r\n\r\n";
    let values: Vec<&[u8]> = header_values(head, b"origin").collect();
    assert_eq!(values, vec![&b"a"[..], &b"b"[..]]);
    assert_eq!(header_values(head, b"host").count(), 1);
    assert_eq!(header_values(head, b"missing").count(), 0);
    // The request line is never mistaken for a header, even with a colon.
    let head = b"GET /x?origin:1 HTTP/1.1\r\n\r\n";
    assert_eq!(header_values(head, b"origin").count(), 0);
  }

  #[test]
  fn recognises_websocket_upgrade() {
    let ok = b"GET /chat HTTP/1.1\r\n\
      Host: 127.0.0.1:1234\r\n\
      Upgrade: websocket\r\n\
      Connection: Upgrade\r\n\
      Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
      Sec-WebSocket-Version: 13\r\n\r\n";
    assert!(is_websocket_upgrade(ok));

    // Case-insensitive header name + mixed-case value.
    let mixed = b"GET / HTTP/1.1\r\nUPGRADE: WebSocket\r\n\r\n";
    assert!(is_websocket_upgrade(mixed));

    // Extra tokens in the upgrade list should still match websocket.
    let list = b"GET / HTTP/1.1\r\nupgrade: h2c, websocket\r\n\r\n";
    assert!(is_websocket_upgrade(list));
  }

  #[test]
  fn rejects_non_upgrade_requests() {
    let plain = b"GET / HTTP/1.1\r\nHost: 127.0.0.1:1234\r\n\r\n";
    assert!(!is_websocket_upgrade(plain));
    let other = b"POST /api HTTP/1.1\r\nUpgrade: h2c\r\n\r\n";
    assert!(!is_websocket_upgrade(other));
  }

  fn upgrade_head(extra_headers: &str) -> Vec<u8> {
    format!(
      "GET /ws HTTP/1.1\r\n\
       Host: 127.0.0.1:1234\r\n\
       Upgrade: websocket\r\n\
       Connection: Upgrade\r\n\
       Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
       Sec-WebSocket-Version: 13\r\n\
       {extra_headers}\r\n"
    )
    .into_bytes()
  }

  #[test]
  fn relay_admits_only_the_app_origin() {
    let origin = origin();
    assert_eq!(
      classify_upgrade(&upgrade_head("Origin: t3code://app\r\n"), &origin),
      RelayDecision::Proxy
    );
    // Header name case and surrounding whitespace are not significant …
    assert_eq!(
      classify_upgrade(&upgrade_head("ORIGIN:   t3code://app \r\n"), &origin),
      RelayDecision::Proxy
    );
    // … but the value is matched byte-for-byte.
    for foreign in [
      "https://evil.example",
      "http://127.0.0.1:1234",
      "t3code://evil",
      "t3code://app.evil",
      "t3code://app/",
      "T3CODE://APP",
      "null",
      "",
    ] {
      assert_eq!(
        classify_upgrade(
          &upgrade_head(&format!("Origin: {foreign}\r\n")),
          &origin
        ),
        RelayDecision::ForbiddenOrigin,
        "Origin {foreign:?} must be refused"
      );
    }
  }

  #[test]
  fn relay_requires_exactly_one_origin_header() {
    let origin = origin();
    // A browser always sends Origin on a WebSocket handshake; a raw client
    // that omits it is not the app's page.
    assert_eq!(
      classify_upgrade(&upgrade_head(""), &origin),
      RelayDecision::ForbiddenOrigin
    );
    // Two Origin headers: the check must not be satisfiable by smuggling the
    // right value next to a wrong one, in either order.
    assert_eq!(
      classify_upgrade(
        &upgrade_head(
          "Origin: t3code://app\r\nOrigin: https://evil.example\r\n"
        ),
        &origin
      ),
      RelayDecision::ForbiddenOrigin
    );
    assert_eq!(
      classify_upgrade(
        &upgrade_head(
          "Origin: https://evil.example\r\nOrigin: t3code://app\r\n"
        ),
        &origin
      ),
      RelayDecision::ForbiddenOrigin
    );
    // Non-UTF-8 header bytes are a mismatch, not a panic.
    let mut head = upgrade_head("");
    head.truncate(head.len() - 2);
    head.extend_from_slice(b"Origin: t3code://\xff\r\n\r\n");
    assert_eq!(
      classify_upgrade(&head, &origin),
      RelayDecision::ForbiddenOrigin
    );
  }

  #[test]
  fn relay_rejects_non_websocket_before_looking_at_origin() {
    let origin = origin();
    // Plain HTTP with the right Origin is still not a WebSocket handshake.
    let plain =
      b"GET / HTTP/1.1\r\nHost: 127.0.0.1:1\r\nOrigin: t3code://app\r\n\r\n";
    assert_eq!(
      classify_upgrade(plain, &origin),
      RelayDecision::NotWebSocket
    );
    // Only GET can open a WebSocket (RFC 6455 §4.1).
    let post = b"POST /ws HTTP/1.1\r\nHost: 127.0.0.1:1\r\n\
      Upgrade: websocket\r\nOrigin: t3code://app\r\n\r\n";
    assert_eq!(classify_upgrade(post, &origin), RelayDecision::NotWebSocket);
    let other_upgrade = b"GET / HTTP/1.1\r\nUpgrade: h2c\r\n\
      Origin: t3code://app\r\n\r\n";
    assert_eq!(
      classify_upgrade(other_upgrade, &origin),
      RelayDecision::NotWebSocket
    );
  }

  #[test]
  fn relay_marks_every_forwarded_head_exactly_once() {
    // Client copies of the marker, in any case and with any value, are
    // dropped; the relay's own is the only one upstream sees.
    let head = upgrade_head(
      "Origin: t3code://app\r\nX-Deno-Desktop-Relay: 0\r\n\
       x-deno-desktop-relay : spoof\r\n",
    );
    let out = relay_request_head(&head);
    let values: Vec<&[u8]> =
      header_values(&out, RELAY_MARKER_HEADER.as_bytes()).collect();
    assert_eq!(values, vec![&b"1"[..]]);
    assert!(out.starts_with(b"GET /ws HTTP/1.1\r\n"));
    assert!(out.ends_with(b"\r\n\r\n"));
    assert_eq!(find_end_of_head(&out), Some(out.len()));
    // Everything else the client sent is kept.
    assert_eq!(header_values(&out, b"origin").count(), 1);
    assert_eq!(header_values(&out, b"sec-websocket-key").count(), 1);
    // And a head without any marker gets one too.
    let out = relay_request_head(&upgrade_head("Origin: t3code://app\r\n"));
    assert_eq!(
      header_values(&out, RELAY_MARKER_HEADER.as_bytes()).count(),
      1
    );
  }

  #[test]
  fn the_scheme_handler_never_forwards_or_returns_the_marker() {
    for name in [
      "x-deno-desktop-relay",
      "X-Deno-Desktop-Relay",
      " X-DENO-DESKTOP-RELAY ",
    ] {
      assert!(should_skip_request_header(name), "{name:?}");
      assert!(is_relay_marker(name), "{name:?}");
    }
    assert!(!is_relay_marker("x-deno-desktop-relay-x"));
  }

  #[test]
  fn bridge_request_bodies_are_capped() {
    assert_eq!(MAX_BRIDGE_REQUEST_BODY, 4 * 1024 * 1024);
    assert!(!body_exceeds_limit(0, MAX_BRIDGE_REQUEST_BODY));
    assert!(body_exceeds_limit(MAX_BRIDGE_REQUEST_BODY, 1));
    assert!(body_exceeds_limit(usize::MAX, 1));
  }

  #[test]
  fn switching_protocols_status_is_exact() {
    assert!(is_switching_protocols(
      b"HTTP/1.1 101 Switching Protocols\r\n\r\n"
    ));
    assert!(is_switching_protocols(b"HTTP/1.1 101\r\n\r\n"));
    for other in [
      &b"HTTP/1.1 200 OK\r\n\r\n"[..],
      b"HTTP/1.1 1010 x\r\n\r\n",
      b"HTTP/1.0 101 x\r\n\r\n",
      b"",
    ] {
      assert!(!is_switching_protocols(other));
    }
  }

  /// Runs [`relay_upgrade`] between in-memory pipes. `server` answers the
  /// forwarded head; returns what the client read and what the server read.
  async fn run_relay(
    client_sends: &[u8],
    server_answer: &'static [u8],
  ) -> (Vec<u8>, Vec<u8>) {
    let (mut client, mut relay_client_side) = tokio::io::duplex(64 * 1024);
    let (mut relay_upstream_side, mut server) = tokio::io::duplex(64 * 1024);
    client.write_all(client_sends).await.unwrap();
    let end = find_end_of_head(client_sends).unwrap();
    let head = client_sends[..end].to_vec();
    let relay = tokio::spawn(async move {
      relay_upgrade(
        &mut relay_client_side,
        &mut relay_upstream_side,
        &head,
        Duration::from_secs(5),
      )
      .await
      .unwrap();
    });
    let server_task = tokio::spawn(async move {
      let (got, _) = read_request_head(&mut server, Duration::from_secs(5))
        .await
        .unwrap()
        .expect("the forwarded head");
      server.write_all(server_answer).await.unwrap();
      // Collect anything else the relay sends until it closes or idles.
      let mut rest = got;
      let mut buf = [0u8; 1024];
      while let Ok(Ok(n)) =
        tokio::time::timeout(Duration::from_millis(300), server.read(&mut buf))
          .await
      {
        if n == 0 {
          break;
        }
        rest.extend_from_slice(&buf[..n]);
      }
      rest
    });
    let mut got = Vec::new();
    let mut buf = [0u8; 1024];
    while let Ok(Ok(n)) =
      tokio::time::timeout(Duration::from_millis(500), client.read(&mut buf))
        .await
    {
      if n == 0 {
        break;
      }
      got.extend_from_slice(&buf[..n]);
    }
    drop(client);
    let upstream = server_task.await.unwrap();
    relay.abort();
    (got, upstream)
  }

  #[tokio::test]
  async fn relay_never_carries_a_non_101_response() {
    let mut sends = upgrade_head("Origin: t3code://app\r\n");
    // A second request pipelined behind the upgrade.
    sends.extend_from_slice(b"GET /second HTTP/1.1\r\nHost: x\r\n\r\n");
    let (client_got, upstream_got) = run_relay(
      &sends,
      b"HTTP/1.1 200 OK\r\ncontent-length: 6\r\n\r\nsecret",
    )
    .await;
    let text = String::from_utf8_lossy(&client_got);
    assert!(text.starts_with("HTTP/1.1 502 "), "{text}");
    assert!(
      !text.contains("200 OK") && !text.contains("secret"),
      "{text}"
    );
    // Only the (marked) upgrade head went upstream; the pipelined request
    // never did.
    assert_eq!(find_end_of_head(&upstream_got), Some(upstream_got.len()));
    assert_eq!(
      header_values(&upstream_got, RELAY_MARKER_HEADER.as_bytes()).count(),
      1
    );
    assert!(!String::from_utf8_lossy(&upstream_got).contains("/second"));
  }

  #[tokio::test]
  async fn relay_passes_a_101_and_what_follows() {
    let sends = upgrade_head("Origin: t3code://app\r\n");
    let (client_got, upstream_got) = run_relay(
      &sends,
      b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n\x81\x02hi",
    )
    .await;
    assert!(client_got.starts_with(b"HTTP/1.1 101 "));
    assert!(client_got.ends_with(b"\x81\x02hi"));
    assert_eq!(
      header_values(&upstream_got, RELAY_MARKER_HEADER.as_bytes()).count(),
      1
    );
  }

  #[test]
  fn canned_responses_have_correct_content_length() {
    for resp in [RELAY_400_RESPONSE, RELAY_403_RESPONSE, RELAY_502_RESPONSE] {
      let text = std::str::from_utf8(resp).unwrap();
      let (head, body) = text.split_once("\r\n\r\n").unwrap();
      let declared: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length: "))
        .unwrap()
        .parse()
        .unwrap();
      assert_eq!(declared, body.len(), "{head}");
    }
  }

  #[test]
  fn ws_relay_binds_loopback_ephemeral() {
    let l = bind_ws_loopback_listener().unwrap();
    let addr = l.local_addr().unwrap();
    assert!(addr.ip().is_loopback());
    assert_ne!(addr.port(), 0);
  }

  /// End-to-end over real sockets: a relay in front of a memory listener that
  /// answers every upgrade with `101`. A spoofed Origin must be turned away
  /// at the relay (403, nothing reaches the server); the app origin must be
  /// proxied through and get the server's 101 back.
  #[test]
  fn ws_relay_end_to_end_enforces_origin() {
    let rt = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()
      .unwrap();
    rt.block_on(async {
      // Stand-in for Deno.serve: accept memory connections, read the head,
      // count them, reply 101.
      let server = deno_net::memory::listen_memory(DESKTOP_SERVE_NAME).unwrap();
      let accepted = Arc::new(AtomicUsize::new(0));
      let accepted_srv = accepted.clone();
      tokio::spawn(async move {
        loop {
          let Ok((mut stream, _)) = server.accept().await else {
            break;
          };
          accepted_srv.fetch_add(1, Ordering::SeqCst);
          let mut buf = vec![0u8; 4096];
          let n = stream.read(&mut buf).await.unwrap();
          assert!(find_end_of_head(&buf[..n]).is_some());
          assert_eq!(
            header_values(&buf[..n], RELAY_MARKER_HEADER.as_bytes()).count(),
            1
          );
          stream
            .write_all(b"HTTP/1.1 101 Switching Protocols\r\n\r\n")
            .await
            .unwrap();
        }
      });

      let listener = bind_ws_loopback_listener().unwrap();
      let addr = listener.local_addr().unwrap();
      spawn_ws_loopback_proxy(listener, origin());

      async fn exchange(addr: SocketAddr, head: &[u8]) -> String {
        let mut tcp = TcpStream::connect(addr).await.unwrap();
        tcp.write_all(head).await.unwrap();
        let mut out = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
          let n = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tcp.read(&mut buf),
          )
          .await
          .expect("relay must answer")
          .unwrap();
          if n == 0 {
            break;
          }
          out.extend_from_slice(&buf[..n]);
          if find_end_of_head(&out).is_some() {
            break;
          }
        }
        String::from_utf8_lossy(&out).into_owned()
      }

      // Spoofed Origin from "another page": refused, never proxied.
      let resp =
        exchange(addr, &upgrade_head("Origin: https://evil.example\r\n")).await;
      assert!(resp.starts_with("HTTP/1.1 403 "), "{resp}");
      assert_eq!(accepted.load(Ordering::SeqCst), 0);

      // No Origin at all (a raw local client): refused too.
      let resp = exchange(addr, &upgrade_head("")).await;
      assert!(resp.starts_with("HTTP/1.1 403 "), "{resp}");
      assert_eq!(accepted.load(Ordering::SeqCst), 0);

      // Plain HTTP: 400, never proxied.
      let resp =
        exchange(addr, b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").await;
      assert!(resp.starts_with("HTTP/1.1 400 "), "{resp}");
      assert_eq!(accepted.load(Ordering::SeqCst), 0);

      // The app's own page: proxied through to the server's 101.
      let resp =
        exchange(addr, &upgrade_head("Origin: t3code://app\r\n")).await;
      assert!(resp.starts_with("HTTP/1.1 101 "), "{resp}");
      assert_eq!(accepted.load(Ordering::SeqCst), 1);
    });
  }
}
