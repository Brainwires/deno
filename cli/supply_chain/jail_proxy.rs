// Copyright 2018-2026 the Deno authors. MIT license.

//! The network of a jailed lifecycle script that may reach only some hosts
//! (DENEXT.md item 111): an allow-listing HTTP proxy on 127.0.0.1, one per
//! script, started by the jail host (the unsandboxed `deno install`).
//!
//! The OS sandbox lets the script's process tree connect only to this proxy's
//! port (`sandbox_macos`, `sandbox_linux`), and its environment points every
//! HTTP client at it (`HTTP_PROXY`, `HTTPS_PROXY`, `npm_config_proxy`, ...).
//! The proxy takes `CONNECT host:port` (HTTPS and any other TCP tunnel) and
//! absolute-URI `http://` requests (plain HTTP forwarding), checks the target
//! against the allow-list (`host` or `host:port`, as `--allow-net` takes), and
//! answers anything else with `403`. It resolves names itself, so the script
//! needs no DNS: a lookup can't carry data out either.
//!
//! When the installer's own environment names an `http://` proxy
//! (`HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY`, minus `NO_PROXY`), allowed
//! connections go through it, so a network that only reaches the internet
//! through a proxy keeps working.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use deno_core::url::Host;
use deno_core::url::Url;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::net::TcpStream;

/// The longest request head the proxy reads.
const MAX_HEAD: usize = 64 * 1024;
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// A host the proxy matches: a domain (lower-case ASCII, IDNA applied, no
/// trailing dot) or an IP address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyHost {
  Domain(String),
  Ip(IpAddr),
}

impl ProxyHost {
  /// Parses a host as a URL would (IDNA, IPv4 forms such as `0x7f.1`,
  /// `[v6]`), or `None` when it isn't one.
  pub fn parse(text: &str) -> Option<Self> {
    if text.is_empty() {
      return None;
    }
    let host = if text.starts_with('[') {
      Host::parse(text).ok()?
    } else if text.contains(':') {
      // a bare IPv6 address
      Host::Ipv6(text.parse().ok()?)
    } else {
      Host::parse(text).ok()?
    };
    Some(match host {
      Host::Domain(domain) => {
        let domain = domain.trim_end_matches('.');
        if domain.is_empty() {
          return None;
        }
        ProxyHost::Domain(domain.to_string())
      }
      Host::Ipv4(ip) => ProxyHost::Ip(IpAddr::V4(ip)),
      Host::Ipv6(ip) => ProxyHost::Ip(IpAddr::V6(ip)),
    })
  }

  /// The host as `TcpStream::connect` takes it.
  fn unbracketed(&self) -> String {
    match self {
      ProxyHost::Domain(domain) => domain.clone(),
      ProxyHost::Ip(ip) => ip.to_string(),
    }
  }

  /// The host as an authority (`[v6]`).
  fn connect_target(&self) -> String {
    match self {
      ProxyHost::Domain(domain) => domain.clone(),
      ProxyHost::Ip(IpAddr::V4(ip)) => ip.to_string(),
      ProxyHost::Ip(IpAddr::V6(ip)) => format!("[{ip}]"),
    }
  }
}

/// Splits `host[:port]` (`[v6]:port`, a bare v6 address without a port).
fn split_host_port(text: &str) -> Option<(&str, Option<u16>)> {
  if let Some(rest) = text.strip_prefix('[') {
    let end = rest.find(']')?;
    let host = &text[..end + 2];
    let after = &rest[end + 1..];
    return match after {
      "" => Some((host, None)),
      _ => Some((host, Some(after.strip_prefix(':')?.parse().ok()?))),
    };
  }
  match text.rfind(':') {
    // more than one colon: a bare IPv6 address
    Some(_) if text.matches(':').count() > 1 => Some((text, None)),
    Some(i) => Some((&text[..i], Some(text[i + 1..].parse().ok()?))),
    None => Some((text, None)),
  }
}

/// One allowed host, with or without a port (without: any port, as
/// `--allow-net=<host>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowEntry {
  pub host: ProxyHost,
  pub port: Option<u16>,
}

/// The hosts a jailed script may reach.
#[derive(Debug, Clone, Default)]
pub struct AllowList(Vec<AllowEntry>);

impl AllowList {
  /// Parses `host` / `host:port` entries; one that isn't a host is an error.
  pub fn parse(entries: &[String]) -> Result<Self, String> {
    let mut list = Vec::with_capacity(entries.len());
    for entry in entries {
      let parsed = split_host_port(entry).and_then(|(host, port)| {
        Some(AllowEntry {
          host: ProxyHost::parse(host)?,
          port,
        })
      });
      match parsed {
        Some(entry) => list.push(entry),
        None => return Err(format!("\"{entry}\" is not a host or host:port")),
      }
    }
    Ok(Self(list))
  }

  pub fn allows(&self, host: &ProxyHost, port: u16) -> bool {
    self
      .0
      .iter()
      .any(|entry| &entry.host == host && entry.port.is_none_or(|p| p == port))
  }
}

/// What a request asks the proxy for.
#[derive(Debug, PartialEq, Eq)]
enum Request {
  /// `CONNECT host:port`
  Tunnel { host: ProxyHost, port: u16 },
  /// `GET http://host[:port]/path`: forwarded with the origin-form target.
  Forward {
    host: ProxyHost,
    port: u16,
    /// The request line and headers to send on (to the origin, or to an
    /// upstream proxy, which takes the absolute form).
    origin_head: Vec<u8>,
    proxy_head: Vec<u8>,
  },
}

#[derive(Debug, PartialEq, Eq)]
enum ParseError {
  BadRequest(&'static str),
}

/// Headers that are about the hop to this proxy, not the request.
fn hop_header(name: &str) -> bool {
  [
    "proxy-connection",
    "proxy-authorization",
    "connection",
    "keep-alive",
  ]
  .iter()
  .any(|h| name.eq_ignore_ascii_case(h))
}

/// Parses a request head (through the blank line).
fn parse_request(head: &[u8]) -> Result<Request, ParseError> {
  let text = std::str::from_utf8(head)
    .map_err(|_| ParseError::BadRequest("the request head isn't UTF-8"))?;
  let mut lines = text.split("\r\n");
  let request_line = lines.next().unwrap_or_default();
  let mut parts = request_line.split(' ');
  let (Some(method), Some(target), Some(version), None) =
    (parts.next(), parts.next(), parts.next(), parts.next())
  else {
    return Err(ParseError::BadRequest("a malformed request line"));
  };
  if !version.starts_with("HTTP/1.") || method.is_empty() {
    return Err(ParseError::BadRequest("a malformed request line"));
  }
  if method == "CONNECT" {
    let (host, port) = split_host_port(target)
      .ok_or(ParseError::BadRequest("a malformed CONNECT target"))?;
    let port = port.ok_or(ParseError::BadRequest("CONNECT without a port"))?;
    let host = ProxyHost::parse(host)
      .ok_or(ParseError::BadRequest("a malformed CONNECT target"))?;
    return Ok(Request::Tunnel { host, port });
  }
  let url = Url::parse(target).map_err(|_| {
    ParseError::BadRequest("expected CONNECT or an absolute http:// URL")
  })?;
  if url.scheme() != "http" {
    return Err(ParseError::BadRequest(
      "only http:// URLs are forwarded; use CONNECT for https://",
    ));
  }
  if !url.username().is_empty() || url.password().is_some() {
    return Err(ParseError::BadRequest("credentials in the URL"));
  }
  let host = url
    .host_str()
    .and_then(ProxyHost::parse)
    .ok_or(ParseError::BadRequest("a URL without a host"))?;
  let port = url.port_or_known_default().unwrap_or(80);
  let mut path = url.path().to_string();
  if let Some(query) = url.query() {
    path.push('?');
    path.push_str(query);
  }
  let mut headers = String::new();
  for line in lines {
    if line.is_empty() {
      break;
    }
    let name = line.split(':').next().unwrap_or_default().trim();
    if hop_header(name) {
      continue;
    }
    headers.push_str(line);
    headers.push_str("\r\n");
  }
  // one request per connection: whatever follows goes to the same host
  headers.push_str("Connection: close\r\n");
  let build = |target: &str| {
    format!("{method} {target} {version}\r\n{headers}").into_bytes()
  };
  Ok(Request::Forward {
    host,
    port,
    origin_head: build(&path),
    proxy_head: build(url.as_str()),
  })
}

/// An `http://` proxy the installer itself goes through.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UpstreamProxy {
  host: String,
  port: u16,
  /// `Proxy-Authorization` value, from the URL's credentials.
  authorization: Option<String>,
}

impl UpstreamProxy {
  fn parse(value: &str) -> Option<Self> {
    let value = value.trim();
    if value.is_empty() {
      return None;
    }
    let url = if value.contains("://") {
      Url::parse(value).ok()?
    } else {
      Url::parse(&format!("http://{value}")).ok()?
    };
    if url.scheme() != "http" {
      static WARNED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
      if WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return None;
      }
      log::warn!(
        "jail-scripts: the {} proxy {} is not used for jailed lifecycle scripts (only http:// proxies are); they connect directly.",
        url.scheme(),
        url.host_str().unwrap_or_default()
      );
      return None;
    }
    let authorization = (!url.username().is_empty()).then(|| {
      let decode = |s: &str| {
        percent_encoding::percent_decode_str(s)
          .decode_utf8_lossy()
          .into_owned()
      };
      let credentials = format!(
        "{}:{}",
        decode(url.username()),
        decode(url.password().unwrap_or_default())
      );
      format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(credentials)
      )
    });
    Some(Self {
      host: url.host_str()?.to_string(),
      port: url.port_or_known_default()?,
      authorization,
    })
  }
}

/// The installer's proxy settings.
#[derive(Debug, Clone, Default)]
pub struct Upstream {
  https: Option<UpstreamProxy>,
  http: Option<UpstreamProxy>,
  no_proxy: Vec<String>,
}

impl Upstream {
  /// Reads the installer's `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` /
  /// `NO_PROXY` (either case).
  pub fn from_env(env: &HashMap<OsString, OsString>) -> Self {
    let get = |names: &[&str]| {
      names.iter().find_map(|name| {
        env
          .iter()
          .find(|(k, _)| k.to_string_lossy() == *name)
          .map(|(_, v)| v.to_string_lossy().into_owned())
          .filter(|v| !v.trim().is_empty())
      })
    };
    let all = get(&["ALL_PROXY", "all_proxy"]);
    let https = get(&["HTTPS_PROXY", "https_proxy"]).or_else(|| all.clone());
    let http = get(&["HTTP_PROXY", "http_proxy"]).or(all);
    Self {
      https: https.as_deref().and_then(UpstreamProxy::parse),
      http: http.as_deref().and_then(UpstreamProxy::parse),
      no_proxy: get(&["NO_PROXY", "no_proxy"])
        .map(|v| {
          v.split(',')
            .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
        })
        .unwrap_or_default(),
    }
  }

  fn for_target(
    &self,
    host: &ProxyHost,
    tunnel: bool,
  ) -> Option<&UpstreamProxy> {
    let proxy = if tunnel { &self.https } else { &self.http };
    let proxy = proxy.as_ref()?;
    let name = host.unbracketed();
    let bypassed = self.no_proxy.iter().any(|entry| {
      entry == "*"
        || name == *entry
        || name
          .strip_suffix(entry.as_str())
          .is_some_and(|rest| rest.ends_with('.'))
    });
    (!bypassed).then_some(proxy)
  }
}

/// The proxy of one jailed script. Stops (and closes its connections) when
/// dropped.
pub struct JailProxy {
  port: u16,
  task: tokio::task::JoinHandle<()>,
}

impl Drop for JailProxy {
  fn drop(&mut self) {
    self.task.abort();
  }
}

impl JailProxy {
  /// Listens on 127.0.0.1 on a free port.
  pub async fn start(allow: AllowList, upstream: Upstream) -> io::Result<Self> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let port = listener.local_addr()?.port();
    let allow = Arc::new(allow);
    let upstream = Arc::new(upstream);
    let task = tokio::spawn(async move {
      let mut connections = tokio::task::JoinSet::new();
      loop {
        tokio::select! {
          accepted = listener.accept() => {
            if let Ok((stream, _)) = accepted {
              let allow = allow.clone();
              let upstream = upstream.clone();
              connections.spawn(async move {
                let _ = handle(stream, &allow, &upstream).await;
              });
            }
          }
          Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
      }
    });
    Ok(Self { port, task })
  }

  pub fn port(&self) -> u16 {
    self.port
  }

  /// `http://127.0.0.1:<port>`
  pub fn url(&self) -> String {
    format!("http://127.0.0.1:{}", self.port)
  }
}

async fn respond(
  stream: &mut TcpStream,
  status: &str,
  message: &str,
) -> io::Result<()> {
  let body = format!("{message}\n");
  let response = format!(
    "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
    body.len()
  );
  stream.write_all(response.as_bytes()).await?;
  stream.shutdown().await
}

/// Reads through the blank line; returns the head and what came after it.
async fn read_head(
  stream: &mut TcpStream,
) -> io::Result<Option<(Vec<u8>, Vec<u8>)>> {
  let mut buf = Vec::with_capacity(4096);
  let mut chunk = [0u8; 4096];
  loop {
    if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
      let rest = buf.split_off(end + 4);
      return Ok(Some((buf, rest)));
    }
    if buf.len() > MAX_HEAD {
      return Ok(None);
    }
    let n = stream.read(&mut chunk).await?;
    if n == 0 {
      return Ok(None);
    }
    buf.extend_from_slice(&chunk[..n]);
  }
}

async fn connect(host: &str, port: u16) -> io::Result<TcpStream> {
  tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connect timed out"))?
}

/// Opens a tunnel to `target` through an upstream proxy; returns it and the
/// bytes that already came through it.
async fn connect_via(
  proxy: &UpstreamProxy,
  target: &str,
) -> io::Result<(TcpStream, Vec<u8>)> {
  let mut stream = connect(&proxy.host, proxy.port).await?;
  let mut request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
  if let Some(auth) = &proxy.authorization {
    request.push_str(&format!("Proxy-Authorization: {auth}\r\n"));
  }
  request.push_str("\r\n");
  stream.write_all(request.as_bytes()).await?;
  let Some((head, rest)) = read_head(&mut stream).await? else {
    return Err(io::Error::other("the upstream proxy closed the connection"));
  };
  let status_ok = head
    .split(|b| *b == b' ')
    .nth(1)
    .is_some_and(|code| code.first() == Some(&b'2') && code.len() == 3);
  if !status_ok {
    return Err(io::Error::other("the upstream proxy refused the tunnel"));
  }
  Ok((stream, rest))
}

async fn handle(
  mut client: TcpStream,
  allow: &AllowList,
  upstream: &Upstream,
) -> io::Result<()> {
  let head = tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut client))
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "head timed out"))??;
  let Some((head, rest)) = head else {
    return respond(&mut client, "400 Bad Request", "a malformed request")
      .await;
  };
  let request = match parse_request(&head) {
    Ok(request) => request,
    Err(ParseError::BadRequest(reason)) => {
      return respond(&mut client, "400 Bad Request", reason).await;
    }
  };
  let (host, port) = match &request {
    Request::Tunnel { host, port } | Request::Forward { host, port, .. } => {
      (host, *port)
    }
  };
  let target = format!("{}:{port}", host.connect_target());
  if !allow.allows(host, port) {
    log::debug!("jail-scripts: refused a connection to {target}");
    return respond(
      &mut client,
      "403 Forbidden",
      &format!(
        "jail-scripts: {target} is not in this lifecycle script's network allow-list"
      ),
    )
    .await;
  }
  let tunnel = matches!(request, Request::Tunnel { .. });
  let via = upstream.for_target(host, tunnel);
  let server = match (&request, via) {
    (Request::Tunnel { .. }, Some(proxy)) => connect_via(proxy, &target).await,
    (Request::Forward { .. }, Some(proxy)) => connect(&proxy.host, proxy.port)
      .await
      .map(|stream| (stream, Vec::new())),
    (_, None) => connect(&host.unbracketed(), port)
      .await
      .map(|stream| (stream, Vec::new())),
  };
  let (mut server, early) = match server {
    Ok(server) => server,
    Err(err) => {
      return respond(
        &mut client,
        "502 Bad Gateway",
        &format!("jail-scripts: couldn't reach {target}: {err}"),
      )
      .await;
    }
  };
  match request {
    Request::Tunnel { .. } => {
      client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
      client.write_all(&early).await?;
    }
    Request::Forward {
      origin_head,
      proxy_head,
      ..
    } => {
      let mut out = match via {
        Some(proxy) => {
          let mut out = proxy_head;
          if let Some(auth) = &proxy.authorization {
            out.extend_from_slice(
              format!("Proxy-Authorization: {auth}\r\n").as_bytes(),
            );
          }
          out
        }
        None => origin_head,
      };
      out.extend_from_slice(b"\r\n");
      server.write_all(&out).await?;
    }
  }
  if !rest.is_empty() {
    server.write_all(&rest).await?;
  }
  tokio::io::copy_bidirectional(&mut client, &mut server).await?;
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn list(entries: &[&str]) -> AllowList {
    AllowList::parse(&entries.iter().map(|s| s.to_string()).collect::<Vec<_>>())
      .unwrap()
  }

  fn host(text: &str) -> ProxyHost {
    ProxyHost::parse(text).unwrap()
  }

  #[test]
  fn allow_list_host_and_port() {
    let allow = list(&["registry.npmjs.org", "localhost:4545"]);
    // a host without a port: any port
    assert!(allow.allows(&host("registry.npmjs.org"), 443));
    assert!(allow.allows(&host("registry.npmjs.org"), 80));
    // host:port: that port only
    assert!(allow.allows(&host("localhost"), 4545));
    assert!(!allow.allows(&host("localhost"), 4546));
    // exact names: no subdomains, no suffix tricks
    assert!(!allow.allows(&host("evil.registry.npmjs.org"), 443));
    assert!(!allow.allows(&host("registry.npmjs.org.evil.com"), 443));
    assert!(!allow.allows(&host("npmjs.org"), 443));
    // names are case-insensitive and a trailing dot is the same name
    assert!(allow.allows(&host("Registry.NPMJS.org"), 443));
    assert!(allow.allows(&host("registry.npmjs.org."), 443));
    // a name doesn't match the address it resolves to, nor the reverse
    assert!(!allow.allows(&host("127.0.0.1"), 4545));
  }

  #[test]
  fn allow_list_idna() {
    let allow = list(&["bücher.example"]);
    assert!(allow.allows(&host("xn--bcher-kva.example"), 443));
    assert!(allow.allows(&host("BÜCHER.example"), 443));
    let allow = list(&["xn--bcher-kva.example:443"]);
    assert!(allow.allows(&host("bücher.example"), 443));
    assert!(!allow.allows(&host("bucher.example"), 443));
  }

  #[test]
  fn allow_list_ip_literals() {
    let allow = list(&["127.0.0.1:8080", "[::1]:443", "10.0.0.1", "fe80::1"]);
    assert!(allow.allows(&host("127.0.0.1"), 8080));
    assert!(!allow.allows(&host("127.0.0.1"), 8081));
    // the URL forms of the same IPv4 address
    assert!(allow.allows(&host("0x7f.0.0.1"), 8080));
    assert!(allow.allows(&host("2130706433"), 8080));
    assert!(allow.allows(&host("[::1]"), 443));
    assert!(allow.allows(&host("[0:0:0:0:0:0:0:1]"), 443));
    assert!(!allow.allows(&host("[::1]"), 80));
    assert!(allow.allows(&host("10.0.0.1"), 22));
    assert!(allow.allows(&host("[fe80::1]"), 1));
    assert!(!allow.allows(&host("localhost"), 8080));
    // an IPv4-mapped IPv6 address is another address
    assert!(!allow.allows(&host("[::ffff:127.0.0.1]"), 8080));
  }

  #[test]
  fn allow_list_rejects_non_hosts() {
    for bad in ["", "host:notaport", "host:70000", "[::1", "a b", ":80"] {
      assert!(
        AllowList::parse(&[bad.to_string()]).is_err(),
        "{bad:?} should be refused"
      );
    }
  }

  #[test]
  fn connect_requests() {
    assert_eq!(
      parse_request(b"CONNECT registry.npmjs.org:443 HTTP/1.1\r\nHost: registry.npmjs.org:443\r\n\r\n"),
      Ok(Request::Tunnel {
        host: host("registry.npmjs.org"),
        port: 443
      })
    );
    assert_eq!(
      parse_request(b"CONNECT [::1]:8443 HTTP/1.1\r\n\r\n"),
      Ok(Request::Tunnel {
        host: host("::1"),
        port: 8443
      })
    );
    assert_eq!(
      parse_request(b"CONNECT B\xc3\xbccher.example:443 HTTP/1.1\r\n\r\n"),
      Ok(Request::Tunnel {
        host: host("xn--bcher-kva.example"),
        port: 443
      })
    );
    // a tunnel names its port
    assert!(parse_request(b"CONNECT example.com HTTP/1.1\r\n\r\n").is_err());
    assert!(parse_request(b"CONNECT /x HTTP/1.1\r\n\r\n").is_err());
  }

  #[test]
  fn absolute_uri_requests() {
    let Ok(Request::Forward {
      host: h,
      port,
      origin_head,
      proxy_head,
    }) = parse_request(
      b"GET http://Example.com:8080/a/b?c=d HTTP/1.1\r\nHost: example.com:8080\r\nProxy-Connection: keep-alive\r\nProxy-Authorization: Basic eDp5\r\nConnection: keep-alive\r\nAccept: */*\r\n\r\n",
    )
    else {
      panic!("not a forward");
    };
    assert_eq!(h, host("example.com"));
    assert_eq!(port, 8080);
    assert_eq!(
      String::from_utf8(origin_head).unwrap(),
      "GET /a/b?c=d HTTP/1.1\r\nHost: example.com:8080\r\nAccept: */*\r\nConnection: close\r\n"
    );
    assert_eq!(
      String::from_utf8(proxy_head).unwrap(),
      "GET http://example.com:8080/a/b?c=d HTTP/1.1\r\nHost: example.com:8080\r\nAccept: */*\r\nConnection: close\r\n"
    );
    // the default port
    let Ok(Request::Forward { port, .. }) =
      parse_request(b"POST http://example.com HTTP/1.0\r\n\r\n")
    else {
      panic!("not a forward");
    };
    assert_eq!(port, 80);
    // origin-form, https:// and other schemes, credentials: refused
    for bad in [
      &b"GET /path HTTP/1.1\r\nHost: example.com\r\n\r\n"[..],
      b"GET https://example.com/ HTTP/1.1\r\n\r\n",
      b"GET ftp://example.com/ HTTP/1.1\r\n\r\n",
      b"GET http://user:pw@example.com/ HTTP/1.1\r\n\r\n",
      b"GET http://example.com/ SPDY/3\r\n\r\n",
      b"GET  http://example.com/ HTTP/1.1\r\n\r\n",
    ] {
      assert!(
        parse_request(bad).is_err(),
        "{}",
        String::from_utf8_lossy(bad)
      );
    }
  }

  #[test]
  fn upstream_from_env() {
    let env = |pairs: &[(&str, &str)]| {
      pairs
        .iter()
        .map(|(k, v)| (OsString::from(*k), OsString::from(*v)))
        .collect::<HashMap<_, _>>()
    };
    let upstream = Upstream::from_env(&env(&[
      ("https_proxy", "http://u%40x:p%3Aw@corp.example:3128"),
      ("NO_PROXY", "internal.example, .local.example"),
    ]));
    let proxy = upstream.https.clone().unwrap();
    assert_eq!(proxy.host, "corp.example");
    assert_eq!(proxy.port, 3128);
    assert_eq!(
      proxy.authorization.as_deref(),
      Some(
        format!(
          "Basic {}",
          base64::engine::general_purpose::STANDARD.encode("u@x:p:w")
        )
        .as_str()
      )
    );
    assert!(upstream.http.is_none());
    let github = host("github.com");
    assert!(upstream.for_target(&github, true).is_some());
    assert!(upstream.for_target(&github, false).is_none());
    assert!(
      upstream
        .for_target(&host("internal.example"), true)
        .is_none()
    );
    assert!(
      upstream
        .for_target(&host("a.local.example"), true)
        .is_none()
    );
    assert!(
      upstream
        .for_target(&host("notlocal.example"), true)
        .is_some()
    );
    // ALL_PROXY covers both; only http:// proxies are used
    let upstream = Upstream::from_env(&env(&[("ALL_PROXY", "corp:8080")]));
    assert_eq!(upstream.http.as_ref().unwrap().port, 8080);
    assert_eq!(upstream.https.as_ref().unwrap().host, "corp");
    let upstream =
      Upstream::from_env(&env(&[("ALL_PROXY", "socks5://corp:1080")]));
    assert!(upstream.http.is_none() && upstream.https.is_none());
  }

  async fn serve_once(
    response: &'static [u8],
  ) -> (u16, tokio::task::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      let (head, _) = read_head(&mut stream).await.unwrap().unwrap();
      stream.write_all(response).await.unwrap();
      stream.shutdown().await.unwrap();
      head
    });
    (port, task)
  }

  async fn roundtrip(proxy_port: u16, request: &str) -> String {
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, proxy_port))
      .await
      .unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    stream.read_to_end(&mut out).await.unwrap();
    String::from_utf8_lossy(&out).into_owned()
  }

  #[tokio::test]
  async fn forwards_allowed_and_refuses_others() {
    let (origin_port, origin) =
      serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;
    let proxy = JailProxy::start(
      list(&[&format!("127.0.0.1:{origin_port}")]),
      Upstream::default(),
    )
    .await
    .unwrap();
    // plain HTTP, forwarded in origin form
    let response = roundtrip(
      proxy.port(),
      &format!(
        "GET http://127.0.0.1:{origin_port}/x HTTP/1.1\r\nHost: 127.0.0.1\r\nProxy-Connection: keep-alive\r\n\r\n"
      ),
    )
    .await;
    assert!(response.ends_with("ok"), "{response}");
    let head = String::from_utf8(origin.await.unwrap()).unwrap();
    assert!(head.starts_with("GET /x HTTP/1.1\r\n"), "{head}");
    assert!(!head.contains("Proxy-Connection"), "{head}");
    // another port, a name for the same address, a CONNECT elsewhere
    for request in [
      format!(
        "GET http://127.0.0.1:{}/ HTTP/1.1\r\n\r\n",
        origin_port.wrapping_add(1)
      ),
      format!("GET http://localhost:{origin_port}/ HTTP/1.1\r\n\r\n"),
      "CONNECT example.com:443 HTTP/1.1\r\n\r\n".to_string(),
    ] {
      let response = roundtrip(proxy.port(), &request).await;
      assert!(response.starts_with("HTTP/1.1 403 "), "{response}");
    }
    let response =
      roundtrip(proxy.port(), "GET /relative HTTP/1.1\r\n\r\n").await;
    assert!(response.starts_with("HTTP/1.1 400 "), "{response}");
  }

  #[tokio::test]
  async fn tunnels_allowed() {
    let (origin_port, origin) =
      serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\ntunnel").await;
    let proxy = JailProxy::start(
      list(&[&format!("127.0.0.1:{origin_port}")]),
      Upstream::default(),
    )
    .await
    .unwrap();
    // the request after the CONNECT head goes through the tunnel as is
    let response = roundtrip(
      proxy.port(),
      &format!(
        "CONNECT 127.0.0.1:{origin_port} HTTP/1.1\r\n\r\nGET /t HTTP/1.1\r\nHost: x\r\n\r\n"
      ),
    )
    .await;
    assert!(
      response.starts_with("HTTP/1.1 200 Connection Established\r\n\r\n"),
      "{response}"
    );
    assert!(response.ends_with("tunnel"), "{response}");
    let head = String::from_utf8(origin.await.unwrap()).unwrap();
    assert!(head.starts_with("GET /t HTTP/1.1\r\n"), "{head}");
  }

  #[tokio::test]
  async fn chains_through_upstream_proxy() {
    // the "upstream proxy" answers the CONNECT, then serves the tunnel
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let upstream_port = listener.local_addr().unwrap().port();
    let upstream_task = tokio::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      let (head, _) = read_head(&mut stream).await.unwrap().unwrap();
      stream
        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
        .await
        .unwrap();
      stream.write_all(b"through").await.unwrap();
      stream.shutdown().await.unwrap();
      String::from_utf8(head).unwrap()
    });
    let mut env = HashMap::new();
    env.insert(
      OsString::from("HTTPS_PROXY"),
      OsString::from(format!("http://me:pw@127.0.0.1:{upstream_port}")),
    );
    let proxy =
      JailProxy::start(list(&["registry.example"]), Upstream::from_env(&env))
        .await
        .unwrap();
    let response = roundtrip(
      proxy.port(),
      "CONNECT registry.example:443 HTTP/1.1\r\n\r\n",
    )
    .await;
    assert!(response.ends_with("through"), "{response}");
    let head = upstream_task.await.unwrap();
    assert!(
      head.starts_with("CONNECT registry.example:443 HTTP/1.1\r\n"),
      "{head}"
    );
    assert!(
      head.contains("Proxy-Authorization: Basic bWU6cHc=\r\n"),
      "{head}"
    );
  }
}
