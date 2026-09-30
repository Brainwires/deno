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
//! handler or the WebSocket relay — have a `request.url` whose scheme is
//! `http+memory:` (`new URL(request.url).protocol === "http+memory:"`), and
//! `info.remoteAddr` is `{ transport: "memory", name: "deno-desktop" }`. That
//! is the check a framework should gate privileged, desktop-only endpoints
//! on: nothing that came over real TCP can carry it. Within that class:
//!
//! * a page request from the scheme handler has `Host: <origin host>` (so
//!   `request.url` is `http+memory://<host>/path`) and, for same-origin
//!   fetches, NO `Origin` header — the browser omits it as it does for any
//!   same-origin `GET`;
//! * a relayed WebSocket upgrade has `Host: 127.0.0.1:<relay port>` and an
//!   `Origin` header equal to the app origin (the relay guarantees it).

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

  let response = sender.send_request(request).await?;

  let status = response.status().as_u16() as i32;
  let mut resp_headers = Vec::with_capacity(response.headers().len());
  for (name, value) in response.headers() {
    if is_hop_by_hop_header(name.as_str()) {
      continue;
    }
    if let Ok(v) = value.to_str() {
      resp_headers.push((name.as_str().to_string(), v.to_string()));
    }
  }
  exchange.begin(status, &resp_headers);
  *began = true;

  let mut body = response.into_body();
  while let Some(frame) = body.frame().await {
    let frame = frame?;
    if let Some(chunk) = frame.data_ref() {
      // Negative return means the webview cancelled / went away.
      if exchange.write(chunk.as_ref()) < 0 {
        break;
      }
    }
  }

  Ok(())
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
  name.eq_ignore_ascii_case(HOST.as_str()) || is_hop_by_hop_header(name)
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
  // Peek the client's request head. Cheaper than fully parsing HTTP, and all
  // we need is the request line and a few headers.
  let mut head = Vec::with_capacity(2048);
  let mut buf = [0u8; 2048];
  let end_of_head = loop {
    let n = tcp.read(&mut buf).await?;
    if n == 0 {
      return Ok(());
    }
    head.extend_from_slice(&buf[..n]);
    if let Some(idx) = find_end_of_head(&head) {
      break idx;
    }
    if head.len() >= MAX_HEAD_LEN {
      // Request head too large — bail without sending an HTTP response so we
      // don't leak that Deno.serve is behind us on plain-HTTP scans.
      return Ok(());
    }
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

  // Open the in-process connection to Deno.serve and replay everything we
  // already read from the client, then plain-shuttle bytes in both directions.
  let mut mem = match connect_memory(DESKTOP_SERVE_NAME) {
    Ok(s) => s,
    Err(e) => {
      log::warn!("[desktop] ws proxy: memory connect failed: {e}");
      return Ok(());
    }
  };
  mem.write_all(&head).await?;

  let _ = tokio::io::copy_bidirectional(&mut tcp, &mut mem).await;
  Ok(())
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

  use super::*;

  fn origin() -> AppOrigin {
    AppOrigin::parse("t3code://app").unwrap()
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
  fn canned_responses_have_correct_content_length() {
    for resp in [RELAY_400_RESPONSE, RELAY_403_RESPONSE] {
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
