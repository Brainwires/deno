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
//! A NAME is allowed, not where it leads: a package's declared host or a
//! listed name whose DNS an attacker controls (rebinding) could resolve to
//! this machine or its network. So the proxy refuses (`403`) a name that
//! resolves to any internal address (loopback, RFC 1918 / ULA private,
//! link-local with cloud metadata's `169.254.169.254`, CGNAT, unspecified;
//! [`is_internal`]) and connects to exactly the addresses it checked, never
//! resolving again. An allow-list entry that is itself an IP literal, the name
//! `localhost`, and a package entry with `"allowPrivate": true` in deno.json
//! are exempt.
//!
//! When the installer's own environment names an `http://` proxy
//! (`HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY`, minus `NO_PROXY`), allowed
//! connections go through it, so a network that only reaches the internet
//! through a proxy keeps working. The upstream is asked for the address the
//! jail checked, so it can't resolve the name differently; with
//! `supplyChain.jailScripts.upstreamByName` (a proxy that only takes host
//! names) it is asked for the name, after the same check.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::Ipv6Addr;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use deno_core::futures::future::BoxFuture;
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

  /// The host as `NO_PROXY` entries match it.
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
pub struct AllowList {
  entries: Vec<AllowEntry>,
  /// Whether a listed name may resolve to an internal address (the
  /// package's `"allowPrivate": true` in deno.json).
  allow_private: bool,
}

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
    Ok(Self {
      entries: list,
      allow_private: false,
    })
  }

  /// Lets listed names resolve to internal addresses (a mirror on the
  /// local network).
  pub fn with_allow_private(mut self, allow_private: bool) -> Self {
    self.allow_private = allow_private;
    self
  }

  pub fn allows(&self, host: &ProxyHost, port: u16) -> bool {
    self
      .entries
      .iter()
      .any(|entry| &entry.host == host && entry.port.is_none_or(|p| p == port))
  }
}

/// Whether an IPv4 address is on this machine or its networks: `0.0.0.0/8`
/// (unspecified, "this network"), loopback `127/8`, RFC 1918 private
/// (`10/8`, `172.16/12`, `192.168/16`), link-local `169.254/16` (cloud
/// metadata services), CGNAT `100.64/10`.
fn is_internal_v4(ip: Ipv4Addr) -> bool {
  let [a, b, ..] = ip.octets();
  a == 0
    || a == 10
    || a == 127
    || (a == 169 && b == 254)
    || (a == 172 && (16..=31).contains(&b))
    || (a == 192 && b == 168)
    || (a == 100 && (64..=127).contains(&b))
}

/// Whether an address is one a listed NAME must not lead to: the internal
/// IPv4 ranges ([`is_internal_v4`]), also inside IPv6 (IPv4-mapped
/// `::ffff:a.b.c.d`, IPv4-compatible `::a.b.c.d`, NAT64 `64:ff9b::/96`),
/// and IPv6 unspecified `::`, loopback `::1`, unique-local `fc00::/7`,
/// link-local `fe80::/10` and the retired site-local `fec0::/10`.
pub(crate) fn is_internal(ip: IpAddr) -> bool {
  let v6 = match ip {
    IpAddr::V4(v4) => return is_internal_v4(v4),
    IpAddr::V6(v6) => v6,
  };
  let s = v6.segments();
  let embedded = || Ipv4Addr::from(((s[6] as u32) << 16) | s[7] as u32);
  if let Some(v4) = v6.to_ipv4_mapped() {
    return is_internal_v4(v4);
  }
  // `::` and `::1` are in here too (`0.0.0.0` and `0.0.0.1`)
  if s[..6] == [0; 6] || s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
    return is_internal_v4(embedded());
  }
  v6 == Ipv6Addr::UNSPECIFIED
    || v6 == Ipv6Addr::LOCALHOST
    || (s[0] & 0xfe00) == 0xfc00
    || (s[0] & 0xffc0) == 0xfe80
    || (s[0] & 0xffc0) == 0xfec0
}

/// Resolves a name to the addresses to connect to.
pub(crate) type Resolver = Arc<
  dyn Fn(String, u16) -> BoxFuture<'static, io::Result<Vec<SocketAddr>>>
    + Send
    + Sync,
>;

fn system_resolver() -> Resolver {
  Arc::new(|host, port| {
    Box::pin(async move {
      Ok(
        tokio::net::lookup_host((host.as_str(), port))
          .await?
          .collect(),
      )
    })
  })
}

/// Why the proxy won't connect to an allowed target.
#[derive(Debug)]
enum Refusal {
  /// The name resolves to this internal address.
  Internal(IpAddr),
  /// The name doesn't resolve.
  Unresolved(io::Error),
}

/// The addresses an allowed target may be reached at, each checked: an IP
/// literal (it is in the allow-list as itself) as is, `localhost` as it
/// resolves, and any other name as it resolves when none of its addresses
/// is internal (one is enough to refuse: a rebinding answer mixes them), or
/// when the package allows private addresses. The caller connects to these
/// and never resolves the name again.
async fn resolve_target(
  host: &ProxyHost,
  port: u16,
  allow_private: bool,
  resolve: &Resolver,
) -> Result<Vec<SocketAddr>, Refusal> {
  let name = match host {
    ProxyHost::Ip(ip) => return Ok(vec![SocketAddr::new(*ip, port)]),
    ProxyHost::Domain(name) => name,
  };
  let mut addrs = resolve(name.clone(), port)
    .await
    .map_err(Refusal::Unresolved)?;
  addrs.dedup();
  if addrs.is_empty() {
    return Err(Refusal::Unresolved(io::Error::new(
      io::ErrorKind::NotFound,
      "no addresses",
    )));
  }
  if !allow_private
    && name != "localhost"
    && let Some(addr) = addrs.iter().find(|addr| is_internal(addr.ip()))
  {
    return Err(Refusal::Internal(addr.ip()));
  }
  Ok(addrs)
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
    head: ForwardHead,
  },
}

/// The request line and headers of a forwarded request, minus the hop
/// headers, plus `Connection: close`.
#[derive(Debug, PartialEq, Eq)]
struct ForwardHead {
  method: String,
  version: String,
  url: Url,
  headers: String,
}

impl ForwardHead {
  fn build(&self, target: &str) -> Vec<u8> {
    let ForwardHead {
      method,
      version,
      headers,
      ..
    } = self;
    format!("{method} {target} {version}\r\n{headers}").into_bytes()
  }

  /// The head an origin server takes: the origin-form target.
  fn origin_head(&self) -> Vec<u8> {
    let mut path = self.url.path().to_string();
    if let Some(query) = self.url.query() {
      path.push('?');
      path.push_str(query);
    }
    self.build(&path)
  }

  /// The head an upstream proxy takes: the absolute form, its host the
  /// address the jail checked (the proxy must not resolve the name again),
  /// or with `None` the name as the client sent it (`upstreamByName`), the
  /// client's `Host` header unchanged.
  fn proxy_head(&self, pinned: Option<IpAddr>) -> Vec<u8> {
    let mut url = self.url.clone();
    if let Some(pinned) = pinned {
      let _ = url.set_ip_host(pinned);
    }
    self.build(url.as_str())
  }
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
  Ok(Request::Forward {
    host,
    port,
    head: ForwardHead {
      method: method.to_string(),
      version: version.to_string(),
      url,
      headers,
    },
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
  /// Ask the upstream for a checked target by name rather than by address
  /// (`supplyChain.jailScripts.upstreamByName`).
  by_name: bool,
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
      by_name: false,
    }
  }

  /// Asks the upstream proxy for a target by its name (after the jail has
  /// checked where it resolves) instead of by the checked address: for
  /// proxies that only accept host names. The upstream then resolves the
  /// name itself.
  pub fn with_by_name(mut self, by_name: bool) -> Self {
    self.by_name = by_name;
    self
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
    Self::start_with(allow, upstream, system_resolver()).await
  }

  async fn start_with(
    allow: AllowList,
    upstream: Upstream,
    resolver: Resolver,
  ) -> io::Result<Self> {
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
              let resolver = resolver.clone();
              connections.spawn(async move {
                let _ = handle(stream, &allow, &upstream, &resolver).await;
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

/// Connects to the first of the checked addresses that answers.
async fn connect_pinned(addrs: &[SocketAddr]) -> io::Result<TcpStream> {
  let mut last = io::Error::new(io::ErrorKind::NotFound, "no addresses");
  for addr in addrs {
    match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await
    {
      Ok(Ok(stream)) => return Ok(stream),
      Ok(Err(err)) => last = err,
      Err(_) => {
        last = io::Error::new(io::ErrorKind::TimedOut, "connect timed out")
      }
    }
  }
  Err(last)
}

/// What the jail says when the upstream proxy refuses a checked address in a
/// way that suggests it only takes host names (it is also logged, once).
const BY_NAME_HINT: &str = "if this network's proxy only accepts host names, set \"upstreamByName\": true in supplyChain.jailScripts in deno.json (the jail still refuses names that resolve to internal addresses, but the proxy then resolves the name again itself)";

fn warn_by_name_hint(what: &str) {
  static WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
  if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
    log::warn!("jail-scripts: {what}; {BY_NAME_HINT}.");
  }
}

/// The status code of a response head (`HTTP/1.1 403 Forbidden`).
fn response_status(head: &[u8]) -> Option<u16> {
  let line = head.split(|b| *b == b'\r').next()?;
  let code = line.split(|b| *b == b' ').nth(1)?;
  if code.len() != 3 {
    return None;
  }
  std::str::from_utf8(code).ok()?.parse().ok()
}

/// Whether an upstream proxy's answer to a request for an ADDRESS suggests it
/// wants a host name instead: `400`, `403`, a `502` (it couldn't or wouldn't
/// connect), or a `407` although credentials were sent (a destination rule,
/// not missing authentication).
fn suggests_name(status: u16, sent_credentials: bool) -> bool {
  matches!(status, 400 | 403 | 502) || (status == 407 && sent_credentials)
}

/// Why a tunnel through the upstream proxy failed.
#[derive(Debug)]
enum ViaError {
  Io(io::Error),
  /// The upstream answered the `CONNECT` with this status (`None`: a
  /// malformed status line).
  Refused(Option<u16>),
}

impl From<io::Error> for ViaError {
  fn from(err: io::Error) -> Self {
    ViaError::Io(err)
  }
}

impl std::fmt::Display for ViaError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      ViaError::Io(err) => err.fmt(f),
      ViaError::Refused(Some(status)) => {
        write!(f, "the upstream proxy refused the tunnel ({status})")
      }
      ViaError::Refused(None) => {
        write!(f, "the upstream proxy refused the tunnel")
      }
    }
  }
}

/// Opens a tunnel to `target` through an upstream proxy; returns it and the
/// bytes that already came through it.
async fn connect_via(
  proxy: &UpstreamProxy,
  target: &str,
) -> Result<(TcpStream, Vec<u8>), ViaError> {
  let mut stream = connect(&proxy.host, proxy.port).await?;
  let mut request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
  if let Some(auth) = &proxy.authorization {
    request.push_str(&format!("Proxy-Authorization: {auth}\r\n"));
  }
  request.push_str("\r\n");
  stream.write_all(request.as_bytes()).await?;
  let Some((head, rest)) = read_head(&mut stream).await? else {
    return Err(
      io::Error::other("the upstream proxy closed the connection").into(),
    );
  };
  match response_status(&head) {
    Some(200..=299) => Ok((stream, rest)),
    status => Err(ViaError::Refused(status)),
  }
}

/// Copies a forwarded request's response from an upstream proxy, logging the
/// `upstreamByName` hint when its status suggests the upstream refused the
/// checked address (it may also be the server's own answer, so it is passed
/// through unchanged).
async fn relay_via_upstream(
  client: &mut TcpStream,
  server: &mut TcpStream,
  target: &str,
  sent_credentials: bool,
) -> io::Result<()> {
  let (mut client_read, mut client_write) = client.split();
  let (mut server_read, mut server_write) = server.split();
  let up = async {
    tokio::io::copy(&mut client_read, &mut server_write).await?;
    server_write.shutdown().await
  };
  let down = async {
    // enough of the response for its status line
    let mut first = Vec::with_capacity(512);
    let mut chunk = [0u8; 512];
    while !first.contains(&b'\n') && first.len() < 4096 {
      let n = server_read.read(&mut chunk).await?;
      if n == 0 {
        break;
      }
      first.extend_from_slice(&chunk[..n]);
    }
    if let Some(status) = response_status(&first)
      && suggests_name(status, sent_credentials)
    {
      warn_by_name_hint(&format!(
        "the upstream proxy (or the server) answered {status} to a request for {target} by its checked address"
      ));
    }
    client_write.write_all(&first).await?;
    tokio::io::copy(&mut server_read, &mut client_write).await?;
    client_write.shutdown().await
  };
  // the response is what matters: an upstream that answers before reading a
  // request body, then closes, fails the upload direction only
  let (_, down) = tokio::join!(up, down);
  down
}

async fn handle(
  mut client: TcpStream,
  allow: &AllowList,
  upstream: &Upstream,
  resolver: &Resolver,
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
  // the target's addresses, checked; every path below connects to these
  // (or hands them to the upstream proxy), never to the name
  let addrs = match resolve_target(host, port, allow.allow_private, resolver)
    .await
  {
    Ok(addrs) => addrs,
    Err(Refusal::Internal(ip)) => {
      log::debug!("jail-scripts: refused {target}, which resolves to {ip}");
      return respond(
          &mut client,
          "403 Forbidden",
          &format!(
            "jail-scripts: {target} resolves to {ip}, an address on this machine or its network (loopback, private, link-local or CGNAT), which a listed name may not lead to; list the address itself, or set \"allowPrivate\": true on the package's entry in supplyChain.jailScripts.packages"
          ),
        )
        .await;
    }
    Err(Refusal::Unresolved(err)) => {
      return respond(
        &mut client,
        "502 Bad Gateway",
        &format!("jail-scripts: couldn't resolve {target}: {err}"),
      )
      .await;
    }
  };
  let tunnel = matches!(request, Request::Tunnel { .. });
  let via = upstream.for_target(host, tunnel);
  // `upstreamByName`: the upstream is asked for the name, which the checks
  // above have passed; it resolves the name itself
  let by_name = upstream.by_name;
  // whether the upstream's refusal of a checked address suggests it wants
  // the name (a listed IP literal has none to send)
  let name_may_help = !by_name && matches!(host, ProxyHost::Domain(_));
  let mut wants_name = false;
  let server = match (&request, via) {
    (Request::Tunnel { .. }, Some(proxy)) if by_name => {
      connect_via(proxy, &target)
        .await
        .map(|(stream, early)| (stream, early, None))
    }
    (Request::Tunnel { .. }, Some(proxy)) => {
      // the user's own proxy may be on a private address; the target it is
      // asked for is the checked address
      let mut result = Err(ViaError::Refused(None));
      for addr in &addrs {
        result = connect_via(proxy, &addr.to_string()).await;
        match &result {
          Ok(_) => break,
          Err(ViaError::Refused(Some(status)))
            if suggests_name(*status, proxy.authorization.is_some()) =>
          {
            wants_name = name_may_help;
          }
          Err(_) => {}
        }
      }
      result.map(|(stream, early)| (stream, early, None))
    }
    (Request::Forward { .. }, Some(proxy)) => connect(&proxy.host, proxy.port)
      .await
      .map_err(ViaError::Io)
      .map(|stream| (stream, Vec::new(), (!by_name).then(|| addrs[0].ip()))),
    (_, None) => connect_pinned(&addrs)
      .await
      .map_err(ViaError::Io)
      .map(|stream| (stream, Vec::new(), None)),
  };
  let (mut server, early, pinned) = match server {
    Ok(server) => server,
    Err(err) => {
      let mut message = format!("jail-scripts: couldn't reach {target}: {err}");
      if wants_name {
        warn_by_name_hint(&format!(
          "the upstream proxy refused a tunnel to {target} by its checked address ({err})"
        ));
        message.push_str("; ");
        message.push_str(BY_NAME_HINT);
      }
      return respond(&mut client, "502 Bad Gateway", &message).await;
    }
  };
  match request {
    Request::Tunnel { .. } => {
      client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
      client.write_all(&early).await?;
    }
    Request::Forward { ref head, .. } => {
      let mut out = match via {
        Some(proxy) => {
          let mut out = head.proxy_head(pinned);
          if let Some(auth) = &proxy.authorization {
            out.extend_from_slice(
              format!("Proxy-Authorization: {auth}\r\n").as_bytes(),
            );
          }
          out
        }
        None => head.origin_head(),
      };
      out.extend_from_slice(b"\r\n");
      server.write_all(&out).await?;
    }
  }
  if !rest.is_empty() {
    server.write_all(&rest).await?;
  }
  if let (Request::Forward { .. }, Some(proxy)) = (&request, via)
    && name_may_help
  {
    return relay_via_upstream(
      &mut client,
      &mut server,
      &target,
      proxy.authorization.is_some(),
    )
    .await;
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
    let Ok(Request::Forward { host: h, port, head }) = parse_request(
      b"GET http://Example.com:8080/a/b?c=d HTTP/1.1\r\nHost: example.com:8080\r\nProxy-Connection: keep-alive\r\nProxy-Authorization: Basic eDp5\r\nConnection: keep-alive\r\nAccept: */*\r\n\r\n",
    )
    else {
      panic!("not a forward");
    };
    assert_eq!(h, host("example.com"));
    assert_eq!(port, 8080);
    assert_eq!(
      String::from_utf8(head.origin_head()).unwrap(),
      "GET /a/b?c=d HTTP/1.1\r\nHost: example.com:8080\r\nAccept: */*\r\nConnection: close\r\n"
    );
    // an upstream proxy gets the checked address, the Host header as sent
    assert_eq!(
      String::from_utf8(
        head.proxy_head(Some("93.184.215.14".parse().unwrap()))
      )
      .unwrap(),
      "GET http://93.184.215.14:8080/a/b?c=d HTTP/1.1\r\nHost: example.com:8080\r\nAccept: */*\r\nConnection: close\r\n"
    );
    assert_eq!(
      String::from_utf8(head.proxy_head(Some("2001:db8::1".parse().unwrap())))
        .unwrap()
        .lines()
        .next()
        .unwrap(),
      "GET http://[2001:db8::1]:8080/a/b?c=d HTTP/1.1"
    );
    // `upstreamByName`: the name, as sent
    assert_eq!(
      String::from_utf8(head.proxy_head(None)).unwrap(),
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
    let (resolver, _) =
      stub_resolver(&[("registry.example", &["93.184.215.14"])]);
    let proxy = JailProxy::start_with(
      list(&["registry.example"]),
      Upstream::from_env(&env),
      resolver,
    )
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
      head.starts_with("CONNECT 93.184.215.14:443 HTTP/1.1\r\n"),
      "{head}"
    );
    assert!(
      head.contains("Proxy-Authorization: Basic bWU6cHc=\r\n"),
      "{head}"
    );
  }
  #[test]
  fn internal_addresses() {
    let internal = |text: &str| is_internal(text.parse().unwrap());
    for ip in [
      // IPv4: unspecified / this network, loopback, RFC 1918, link-local
      // (cloud metadata), CGNAT, with their edges
      "0.0.0.0",
      "0.255.255.255",
      "127.0.0.1",
      "127.255.255.254",
      "10.0.0.1",
      "10.255.255.255",
      "172.16.0.1",
      "172.31.255.255",
      "192.168.0.1",
      "192.168.255.255",
      "169.254.169.254",
      "169.254.0.1",
      "100.64.0.1",
      "100.127.255.255",
      // IPv6: unspecified, loopback, unique-local, link-local, site-local
      "::",
      "::1",
      "fc00::1",
      "fd12:3456::1",
      "fe80::1",
      "febf::1",
      "fec0::1",
      // IPv4 inside IPv6: mapped, compatible, NAT64
      "::ffff:127.0.0.1",
      "::ffff:10.1.2.3",
      "::ffff:169.254.169.254",
      "::ffff:0.0.0.0",
      "::127.0.0.1",
      "64:ff9b::a9fe:a9fe",
      "64:ff9b::192.168.1.1",
    ] {
      assert!(internal(ip), "{ip} should be internal");
    }
    for ip in [
      "1.1.1.1",
      "8.8.8.8",
      "9.255.255.255",
      "11.0.0.1",
      "100.63.255.255",
      "100.128.0.1",
      "126.255.255.255",
      "128.0.0.1",
      "169.253.255.255",
      "169.255.0.1",
      "172.15.255.255",
      "172.32.0.1",
      "192.167.255.255",
      "192.169.0.1",
      "2606:4700::1111",
      "2001:db8::1",
      "fbff::1",
      "ff02::1",
      "::ffff:8.8.8.8",
      "::8.8.8.8",
      "64:ff9b::808:808",
    ] {
      assert!(!internal(ip), "{ip} should not be internal");
    }
  }

  /// A resolver answering from a table (a name missing from it doesn't
  /// resolve), counting its lookups.
  fn stub_resolver(
    table: &[(&str, &[&str])],
  ) -> (Resolver, Arc<std::sync::atomic::AtomicUsize>) {
    let table: HashMap<String, Vec<IpAddr>> = table
      .iter()
      .map(|(name, ips)| {
        (
          name.to_string(),
          ips.iter().map(|ip| ip.parse().unwrap()).collect(),
        )
      })
      .collect();
    let lookups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = lookups.clone();
    let resolver: Resolver = Arc::new(move |name, port| {
      counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
      let found = table
        .get(&name)
        .map(|ips| ips.iter().map(|ip| SocketAddr::new(*ip, port)).collect());
      Box::pin(async move {
        found.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "nxdomain"))
      })
    });
    (resolver, lookups)
  }

  #[tokio::test]
  async fn resolve_then_refuse() {
    let (resolver, lookups) = stub_resolver(&[
      ("public.example", &["93.184.215.14", "2606:2800::1"]),
      ("rebound.example", &["127.0.0.1"]),
      ("metadata.example", &["169.254.169.254"]),
      // a rebinding answer that mixes a public address with an internal one
      ("mixed.example", &["93.184.215.14", "10.0.0.5"]),
      ("mapped.example", &["::ffff:127.0.0.1"]),
      ("ula.example", &["fd00::5"]),
      ("localhost", &["127.0.0.1", "::1"]),
    ]);
    let target = |name: &str| ProxyHost::parse(name).unwrap();
    let ok = resolve_target(&target("public.example"), 443, false, &resolver)
      .await
      .unwrap();
    assert_eq!(
      ok,
      vec![
        "93.184.215.14:443".parse::<SocketAddr>().unwrap(),
        "[2606:2800::1]:443".parse().unwrap()
      ]
    );
    for (name, ip) in [
      ("rebound.example", "127.0.0.1"),
      ("metadata.example", "169.254.169.254"),
      ("mixed.example", "10.0.0.5"),
      ("mapped.example", "::ffff:127.0.0.1"),
      ("ula.example", "fd00::5"),
    ] {
      match resolve_target(&target(name), 443, false, &resolver).await {
        Err(Refusal::Internal(got)) => {
          assert_eq!(got, ip.parse::<IpAddr>().unwrap(), "{name}")
        }
        other => panic!("{name}: {other:?}"),
      }
    }
    // the package's allowPrivate: internal answers pass
    assert_eq!(
      resolve_target(&target("mixed.example"), 80, true, &resolver)
        .await
        .unwrap()
        .len(),
      2
    );
    // `localhost` is exempt, as resolved
    assert_eq!(
      resolve_target(&target("localhost"), 4545, false, &resolver)
        .await
        .unwrap()[0],
      "127.0.0.1:4545".parse().unwrap()
    );
    // a name that doesn't resolve
    assert!(matches!(
      resolve_target(&target("nowhere.example"), 443, false, &resolver).await,
      Err(Refusal::Unresolved(_))
    ));
    // an IP literal (listed as itself) is never looked up
    let before = lookups.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
      resolve_target(&target("[::1]"), 8, false, &resolver)
        .await
        .unwrap(),
      vec!["[::1]:8".parse::<SocketAddr>().unwrap()]
    );
    assert_eq!(lookups.load(std::sync::atomic::Ordering::SeqCst), before);
  }

  #[tokio::test]
  async fn refuses_names_that_resolve_internal() {
    let (origin_port, origin) =
      serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;
    // a listed name whose DNS answers this machine (rebinding, or a
    // package's declared host), next to the literal address
    let (resolver, lookups) =
      stub_resolver(&[("rebound.example", &["127.0.0.1"])]);
    let proxy = JailProxy::start_with(
      list(&["rebound.example", &format!("127.0.0.1:{origin_port}")]),
      Upstream::default(),
      resolver,
    )
    .await
    .unwrap();
    for request in [
      format!("GET http://rebound.example:{origin_port}/ HTTP/1.1\r\n\r\n"),
      format!("CONNECT rebound.example:{origin_port} HTTP/1.1\r\n\r\n"),
    ] {
      let response = roundtrip(proxy.port(), &request).await;
      assert!(response.starts_with("HTTP/1.1 403 "), "{response}");
      assert!(response.contains("resolves to 127.0.0.1"), "{response}");
    }
    assert_eq!(lookups.load(std::sync::atomic::Ordering::SeqCst), 2);
    // the literal entry still reaches the same server
    let response = roundtrip(
      proxy.port(),
      &format!("GET http://127.0.0.1:{origin_port}/x HTTP/1.1\r\n\r\n"),
    )
    .await;
    assert!(response.ends_with("ok"), "{response}");
    origin.await.unwrap();
  }

  #[tokio::test]
  async fn allow_private_and_pinned_address() {
    let (origin_port, origin) =
      serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;
    // the package opted in: the name may lead to this machine, and the proxy
    // connects to the address it resolved (one lookup), whatever the name
    // would answer later
    let (resolver, lookups) =
      stub_resolver(&[("mirror.internal", &["127.0.0.1"])]);
    let proxy = JailProxy::start_with(
      list(&["mirror.internal"]).with_allow_private(true),
      Upstream::default(),
      resolver,
    )
    .await
    .unwrap();
    let response = roundtrip(
      proxy.port(),
      &format!(
        "GET http://mirror.internal:{origin_port}/x HTTP/1.1\r\nHost: mirror.internal\r\n\r\n"
      ),
    )
    .await;
    assert!(response.ends_with("ok"), "{response}");
    assert_eq!(lookups.load(std::sync::atomic::Ordering::SeqCst), 1);
    let head = String::from_utf8(origin.await.unwrap()).unwrap();
    assert!(head.contains("Host: mirror.internal\r\n"), "{head}");
  }

  #[tokio::test]
  async fn upstream_gets_the_checked_address() {
    // the user's proxy is on loopback (allowed); the target is checked, and
    // the proxy is asked for the address, not the name
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let upstream_port = listener.local_addr().unwrap().port();
    let upstream_task = tokio::spawn(async move {
      let mut heads = Vec::new();
      for _ in 0..2 {
        let (mut stream, _) = listener.accept().await.unwrap();
        let (head, _) = read_head(&mut stream).await.unwrap().unwrap();
        stream
          .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
          .await
          .unwrap();
        stream.shutdown().await.unwrap();
        heads.push(String::from_utf8(head).unwrap());
      }
      heads
    });
    let mut env = HashMap::new();
    for name in ["HTTPS_PROXY", "HTTP_PROXY"] {
      env.insert(
        OsString::from(name),
        OsString::from(format!("http://127.0.0.1:{upstream_port}")),
      );
    }
    let (resolver, _) = stub_resolver(&[
      ("registry.example", &["93.184.215.14"]),
      ("rebound.example", &["192.168.1.1"]),
    ]);
    let proxy = JailProxy::start_with(
      list(&["registry.example", "rebound.example"]),
      Upstream::from_env(&env),
      resolver,
    )
    .await
    .unwrap();
    let response = roundtrip(
      proxy.port(),
      "CONNECT registry.example:443 HTTP/1.1\r\n\r\n",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
    roundtrip(
      proxy.port(),
      "GET http://registry.example/p HTTP/1.1\r\nHost: registry.example\r\n\r\n",
    )
    .await;
    for request in [
      "CONNECT rebound.example:443 HTTP/1.1\r\n\r\n",
      "GET http://rebound.example/ HTTP/1.1\r\n\r\n",
    ] {
      let response = roundtrip(proxy.port(), request).await;
      assert!(response.starts_with("HTTP/1.1 403 "), "{response}");
    }
    let heads = upstream_task.await.unwrap();
    assert!(
      heads[0].starts_with("CONNECT 93.184.215.14:443 HTTP/1.1\r\n"),
      "{}",
      heads[0]
    );
    assert!(
      heads[1].starts_with("GET http://93.184.215.14/p HTTP/1.1\r\n")
        && heads[1].contains("Host: registry.example\r\n"),
      "{}",
      heads[1]
    );
  }

  /// An upstream proxy that only takes host names: a request for an address
  /// (`CONNECT 93.184.215.14:443`, `GET http://93.184.215.14/`) gets
  /// `status`, a name `200` and `through`. Returns its port and the request
  /// lines it saw.
  async fn names_only_upstream(
    status: &'static str,
  ) -> (
    u16,
    Arc<std::sync::Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
  ) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    let task = tokio::spawn(async move {
      loop {
        let (mut stream, _) = listener.accept().await.unwrap();
        let (head, _) = read_head(&mut stream).await.unwrap().unwrap();
        let head = String::from_utf8(head).unwrap();
        let line = head.lines().next().unwrap().to_string();
        let target = line.split(' ').nth(1).unwrap();
        let host = target.strip_prefix("http://").unwrap_or(target);
        let by_address = host.starts_with(|c: char| c.is_ascii_digit())
          || host.starts_with('[');
        log.lock().unwrap().push(line.clone());
        let response = if by_address {
          format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n")
        } else if line.starts_with("CONNECT ") {
          "HTTP/1.1 200 Connection established\r\n\r\nthrough".to_string()
        } else {
          "HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\nthrough".to_string()
        };
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.shutdown().await.unwrap();
      }
    });
    (port, seen, task)
  }

  fn proxy_env(upstream_port: u16, credentials: &str) -> Upstream {
    let mut env = HashMap::new();
    for name in ["HTTPS_PROXY", "HTTP_PROXY"] {
      env.insert(
        OsString::from(name),
        OsString::from(format!(
          "http://{credentials}127.0.0.1:{upstream_port}"
        )),
      );
    }
    Upstream::from_env(&env)
  }

  #[tokio::test]
  async fn upstream_refusing_addresses_hints_by_name() {
    for status in ["403 Forbidden", "400 Bad Request", "502 Bad Gateway"] {
      let (upstream_port, seen, upstream_task) =
        names_only_upstream(status).await;
      let (resolver, _) = stub_resolver(&[(
        "registry.example",
        &["93.184.215.14", "93.184.215.15"],
      )]);
      let proxy = JailProxy::start_with(
        list(&["registry.example", "93.184.215.16"]),
        proxy_env(upstream_port, ""),
        resolver,
      )
      .await
      .unwrap();
      // by default the upstream is asked for each checked address; its
      // refusal fails the tunnel with a 502 that names the setting
      let response = roundtrip(
        proxy.port(),
        "CONNECT registry.example:443 HTTP/1.1\r\n\r\n",
      )
      .await;
      assert!(
        response.starts_with("HTTP/1.1 502 "),
        "{status}: {response}"
      );
      assert!(
        response.contains("\"upstreamByName\": true"),
        "{status}: {response}"
      );
      assert_eq!(
        *seen.lock().unwrap(),
        vec![
          "CONNECT 93.184.215.14:443 HTTP/1.1",
          "CONNECT 93.184.215.15:443 HTTP/1.1"
        ]
      );
      // a listed address has no name to send: no hint
      let response =
        roundtrip(proxy.port(), "CONNECT 93.184.215.16:443 HTTP/1.1\r\n\r\n")
          .await;
      assert!(response.starts_with("HTTP/1.1 502 "), "{response}");
      assert!(!response.contains("upstreamByName"), "{response}");
      // a forwarded request: the answer is passed through (it may be the
      // server's own), the hint is logged
      let response = roundtrip(
        proxy.port(),
        "GET http://registry.example/p HTTP/1.1\r\nHost: registry.example\r\n\r\n",
      )
      .await;
      assert!(
        response.starts_with(&format!("HTTP/1.1 {status}\r\n")),
        "{response}"
      );
      upstream_task.abort();
    }
  }

  #[tokio::test]
  async fn upstream_missing_credentials_is_not_a_name_problem() {
    // a 407 without credentials asks for them, not for a name; with
    // credentials sent it is a destination rule
    for (credentials, hint) in [("", false), ("me:pw@", true)] {
      let (upstream_port, _, upstream_task) =
        names_only_upstream("407 Proxy Authentication Required").await;
      let (resolver, _) =
        stub_resolver(&[("registry.example", &["93.184.215.14"])]);
      let proxy = JailProxy::start_with(
        list(&["registry.example"]),
        proxy_env(upstream_port, credentials),
        resolver,
      )
      .await
      .unwrap();
      let response = roundtrip(
        proxy.port(),
        "CONNECT registry.example:443 HTTP/1.1\r\n\r\n",
      )
      .await;
      assert!(response.starts_with("HTTP/1.1 502 "), "{response}");
      assert!(response.contains("(407)"), "{response}");
      assert_eq!(response.contains("upstreamByName"), hint, "{response}");
      upstream_task.abort();
    }
  }

  #[tokio::test]
  async fn upstream_by_name() {
    let (upstream_port, seen, upstream_task) =
      names_only_upstream("403 Forbidden").await;
    let (resolver, lookups) = stub_resolver(&[
      ("registry.example", &["93.184.215.14"]),
      ("rebound.example", &["127.0.0.1"]),
      ("metadata.example", &["169.254.169.254"]),
    ]);
    let proxy = JailProxy::start_with(
      list(&["registry.example", "rebound.example", "metadata.example"]),
      proxy_env(upstream_port, "me:pw@").with_by_name(true),
      resolver,
    )
    .await
    .unwrap();
    // the name goes to the upstream, which takes it
    let response = roundtrip(
      proxy.port(),
      "CONNECT registry.example:443 HTTP/1.1\r\n\r\n",
    )
    .await;
    assert!(
      response.starts_with("HTTP/1.1 200 Connection Established\r\n\r\n"),
      "{response}"
    );
    assert!(response.ends_with("through"), "{response}");
    let response = roundtrip(
      proxy.port(),
      "GET http://registry.example/p HTTP/1.1\r\nHost: registry.example\r\n\r\n",
    )
    .await;
    assert!(response.ends_with("through"), "{response}");
    // the name is still checked first: one that resolves internal is
    // refused, and the upstream never hears of it
    for request in [
      "CONNECT rebound.example:443 HTTP/1.1\r\n\r\n",
      "GET http://rebound.example/ HTTP/1.1\r\n\r\n",
      "CONNECT metadata.example:80 HTTP/1.1\r\n\r\n",
    ] {
      let response = roundtrip(proxy.port(), request).await;
      assert!(response.starts_with("HTTP/1.1 403 "), "{response}");
      assert!(response.contains("resolves to"), "{response}");
    }
    // and so is the allow-list
    let response =
      roundtrip(proxy.port(), "CONNECT other.example:443 HTTP/1.1\r\n\r\n")
        .await;
    assert!(response.starts_with("HTTP/1.1 403 "), "{response}");
    assert_eq!(lookups.load(std::sync::atomic::Ordering::SeqCst), 5);
    assert_eq!(
      *seen.lock().unwrap(),
      vec![
        "CONNECT registry.example:443 HTTP/1.1",
        "GET http://registry.example/p HTTP/1.1"
      ]
    );
    upstream_task.abort();
  }
}
