// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: an embedder's web-origin mode for `fetch()`.
//!
//! Deno's `fetch()` is a server-side one: no request has an origin, every
//! response is readable, and the request headers a browser reserves for
//! itself can be set. An embedder that runs web pages puts an
//! [`EmbedderWebOrigin`] in the `OpState`, and the fetch of that runtime then
//! follows the Fetch Standard's origin rules
//! (<https://fetch.spec.whatwg.org/>): request modes and credentials modes,
//! forbidden methods and request headers, `Origin` and `Referer`, the CORS
//! check, the CORS-preflight fetch and its cache, the rules of a redirect,
//! and response tainting (the `basic`, `cors`, `opaque` and `opaqueredirect`
//! filtered responses).
//!
//! Without one nothing changes: `26_fetch.js` asks once
//! ([`op_fetch_web_mode`]) and never calls the other ops.
//!
//! Where what runs:
//!
//! - the decisions are here, in Rust, as functions of the request (its
//!   method, URL list, header list and modes) and of the response's header
//!   list: [`op_fetch_web_request`] before a request leaves (every hop of a
//!   redirect chain), [`op_fetch_web_preflight`] (the CORS-preflight fetch,
//!   which this module sends itself), [`op_fetch_web_response`] when a
//!   response arrived (the CORS check, and the header list the page may
//!   see), [`op_fetch_web_redirect`] before a redirect is followed;
//! - `26_fetch.js` (`mainFetch`, `httpRedirectFetch`) calls them in the
//!   order the standard gives and builds the filtered response;
//! - `20_headers.js` asks [`op_fetch_web_header_blocked`] for the `Headers`
//!   guards (`request`, `request-no-cors`, `response`), so that a page sees
//!   what a browser shows. Nothing rests on the guards: the header list is
//!   sanitized again when the request is made.
//!
//! # Credentials
//!
//! There is no cookie store and no HTTP authentication cache: a request
//! never carries credentials the page did not write itself, whatever its
//! credentials mode, and credentials in a URL are dropped. The credentials
//! mode still decides what the standard makes it decide (the
//! `Access-Control-Allow-Origin: *` and `Access-Control-Allow-Credentials`
//! rules, the wildcards of a preflight response, the credentials flag of a
//! preflight cache entry). [`attach_credentials`] and
//! [`store_response_credentials`] are where a cookie store plugs in.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use std::time::Instant;

use deno_core::CancelFuture;
use deno_core::CancelHandle;
use deno_core::FromV8;
use deno_core::OpState;
use deno_core::ResourceId;
use deno_core::ToV8;
use deno_core::convert::ByteString;
use deno_core::op2;
use deno_core::url::Host;
use deno_core::url::Origin;
use deno_core::url::Url;
use deno_permissions::PermissionCheckError;
use deno_permissions::PermissionsContainer;
use http::HeaderValue;
use http::Method;
use http::Uri;
use http::header::ACCEPT;
use http::header::ACCESS_CONTROL_REQUEST_HEADERS;
use http::header::ACCESS_CONTROL_REQUEST_METHOD;
use http::header::ORIGIN;
use http::header::REFERER;

use crate::HttpClientCreateError;
use crate::Options;
use crate::ReqBody;
use crate::get_or_create_client_from_state;

/// A header as the ops carry it: a name and a value, both byte strings.
pub type Header = (ByteString, ByteString);

/// The longest a preflight response is cached for, whatever its
/// `Access-Control-Max-Age` (Chrome's limit: two hours).
pub const PREFLIGHT_MAX_AGE_LIMIT: Duration = Duration::from_secs(7200);

/// How long a preflight response without a usable `Access-Control-Max-Age`
/// is cached for.
pub const PREFLIGHT_MAX_AGE_DEFAULT: Duration = Duration::from_secs(5);

/// The most entries a preflight cache keeps; the oldest are dropped first.
const PREFLIGHT_CACHE_CAPACITY: usize = 1024;

/// The longest serialized URL sent as a referrer; a longer one is cut down
/// to its origin.
const MAX_REFERRER_LENGTH: usize = 4096;

/// The opt-in of an embedder that runs web pages: with one of these in the
/// `OpState`, `fetch()`, `Request`, `Headers` and `EventSource` of that
/// runtime follow the Fetch Standard's origin rules for a page at this URL,
/// and `WebSocket` handshakes carry its origin.
///
/// The origin is the URL's: a tuple origin for `http:` and `https:`, an
/// opaque one (serialized `null`, same origin with nothing) for `data:`,
/// `about:`, `file:` and the like.
///
/// Scripts can neither read nor replace it.
#[derive(Clone, Debug)]
pub struct EmbedderWebOrigin {
  url: Url,
  origin: Origin,
  on_blocked: Option<fn(&mut OpState, &str)>,
}

impl EmbedderWebOrigin {
  /// The web-origin mode of a page whose document is at `url`.
  pub fn new(url: Url) -> Self {
    let origin = url.origin();
    Self {
      url,
      origin,
      on_blocked: None,
    }
  }

  /// Calls `report` with the reason whenever a request is refused or a
  /// response is withheld from the page (what a browser writes to its
  /// console; the page itself only gets `TypeError: Failed to fetch`).
  pub fn with_blocked_reporter(
    mut self,
    report: fn(&mut OpState, &str),
  ) -> Self {
    self.on_blocked = Some(report);
    self
  }

  /// The page's origin.
  pub fn origin(&self) -> &Origin {
    &self.origin
  }

  /// The page's origin as an `Origin` header carries it (`null` when it is
  /// opaque).
  pub fn serialized_origin(&self) -> String {
    self.origin.ascii_serialization()
  }

  /// The embedder's web origin in `state`, when it has one.
  pub fn from_state(state: &OpState) -> Option<Self> {
    state.try_borrow::<EmbedderWebOrigin>().cloned()
  }

  /// The request headers of a WebSocket handshake a page opens: what the
  /// caller passed without the headers a page may not set, and the page's
  /// `Origin` (<https://websockets.spec.whatwg.org/>: the request's mode is
  /// "websocket", so the origin is always sent).
  pub fn websocket_headers(&self, headers: Option<Vec<Header>>) -> Vec<Header> {
    let mut headers: Vec<Header> = headers
      .unwrap_or_default()
      .into_iter()
      .filter(|(name, value)| !is_forbidden_request_header(name, value))
      .collect();
    headers.push(("origin".into(), self.serialized_origin().into()));
    headers
  }

  /// The URL a request's referrer is taken from when the request says
  /// "client": the document's, unless its origin is opaque.
  fn referrer_source(&self) -> Option<&Url> {
    self.origin.is_tuple().then_some(&self.url)
  }

  fn report_blocked(&self, state: &mut OpState, url: &str, reason: &str) {
    if let Some(report) = self.on_blocked {
      report(
        state,
        &format!(
          "Access to fetch at '{url}' from origin '{}' has been blocked: {reason}",
          self.serialized_origin()
        ),
      );
    }
  }
}

#[derive(Debug, thiserror::Error, deno_error::JsError)]
pub enum WebOriginError {
  #[class(type)]
  #[error("fetch is not in web-origin mode")]
  NotEnabled,
  #[class(type)]
  #[error("Invalid web-origin request: {0}")]
  Invalid(&'static str),
  #[class(inherit)]
  #[error(transparent)]
  Permission(#[from] PermissionCheckError),
  #[class(inherit)]
  #[error(transparent)]
  Resource(#[from] deno_core::error::ResourceError),
  #[class(inherit)]
  #[error(transparent)]
  ClientCreate(#[from] HttpClientCreateError),
}

// --- bytes ---------------------------------------------------------------

fn trim_http_tab_or_space(mut bytes: &[u8]) -> &[u8] {
  while let [b' ' | b'\t', rest @ ..] = bytes {
    bytes = rest;
  }
  while let [rest @ .., b' ' | b'\t'] = bytes {
    bytes = rest;
  }
  bytes
}

fn trim_http_whitespace(mut bytes: &[u8]) -> &[u8] {
  while let [b' ' | b'\t' | b'\n' | b'\r', rest @ ..] = bytes {
    bytes = rest;
  }
  while let [rest @ .., b' ' | b'\t' | b'\n' | b'\r'] = bytes {
    bytes = rest;
  }
  bytes
}

/// <https://mimesniff.spec.whatwg.org/#http-token-code-point>
fn is_http_token_byte(byte: u8) -> bool {
  byte.is_ascii_alphanumeric()
    || matches!(
      byte,
      b'!'
        | b'#'
        | b'$'
        | b'%'
        | b'&'
        | b'\''
        | b'*'
        | b'+'
        | b'-'
        | b'.'
        | b'^'
        | b'_'
        | b'`'
        | b'|'
        | b'~'
    )
}

fn is_token(bytes: &[u8]) -> bool {
  !bytes.is_empty() && bytes.iter().all(|byte| is_http_token_byte(*byte))
}

/// The values of every header named `name` (lowercase), in order.
fn values_of<'a>(
  headers: &'a [Header],
  name: &'a str,
) -> impl Iterator<Item = &'a [u8]> {
  headers
    .iter()
    .filter(move |(header, _)| header.eq_ignore_ascii_case(name.as_bytes()))
    .map(|(_, value)| value.as_slice())
}

/// <https://fetch.spec.whatwg.org/#concept-header-list-get>: the values of
/// the headers named `name` joined with `, `, or `None` without one.
pub fn get_header(headers: &[Header], name: &str) -> Option<Vec<u8>> {
  let mut combined: Option<Vec<u8>> = None;
  for value in values_of(headers, name) {
    match &mut combined {
      Some(combined) => {
        combined.extend_from_slice(b", ");
        combined.extend_from_slice(value);
      }
      None => combined = Some(value.to_vec()),
    }
  }
  combined
}

/// What extracting a list-valued header gives
/// (<https://fetch.spec.whatwg.org/#extract-header-list-values>).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Extracted {
  /// No header of that name.
  Missing,
  /// A value that is not a list of tokens.
  Failure,
  /// The tokens of every header of that name.
  Values(Vec<String>),
}

/// Extracts the values of the headers named `name` whose ABNF is a
/// comma-separated list of tokens (`Access-Control-Allow-Methods`,
/// `-Allow-Headers`, `-Expose-Headers`, `Referrer-Policy`). Empty list
/// elements are skipped, as the list rule of HTTP has it.
pub fn extract_token_list(headers: &[Header], name: &str) -> Extracted {
  let mut found = false;
  let mut values = Vec::new();
  for value in values_of(headers, name) {
    found = true;
    for part in value.split(|byte| *byte == b',') {
      let part = trim_http_tab_or_space(part);
      if part.is_empty() {
        continue;
      }
      if !is_token(part) {
        return Extracted::Failure;
      }
      values.push(String::from_utf8_lossy(part).into_owned());
    }
  }
  if found {
    Extracted::Values(values)
  } else {
    Extracted::Missing
  }
}

// --- methods and request headers ------------------------------------------

/// <https://fetch.spec.whatwg.org/#forbidden-method>
pub fn is_forbidden_method(method: &[u8]) -> bool {
  method.eq_ignore_ascii_case(b"CONNECT")
    || method.eq_ignore_ascii_case(b"TRACE")
    || method.eq_ignore_ascii_case(b"TRACK")
}

/// <https://fetch.spec.whatwg.org/#cors-safelisted-method>
pub fn is_cors_safelisted_method(method: &[u8]) -> bool {
  matches!(method, b"GET" | b"HEAD" | b"POST")
}

const FORBIDDEN_REQUEST_HEADER_NAMES: &[&str] = &[
  "accept-charset",
  "accept-encoding",
  "access-control-request-headers",
  "access-control-request-method",
  "connection",
  "content-length",
  "cookie",
  "cookie2",
  "date",
  "dnt",
  "expect",
  "host",
  "keep-alive",
  "origin",
  "referer",
  "set-cookie",
  "te",
  "trailer",
  "transfer-encoding",
  "upgrade",
  "via",
];

const METHOD_OVERRIDE_HEADER_NAMES: &[&str] = &[
  "x-http-method",
  "x-http-method-override",
  "x-method-override",
];

/// <https://fetch.spec.whatwg.org/#forbidden-request-header>
pub fn is_forbidden_request_header(name: &[u8], value: &[u8]) -> bool {
  let name = name.to_ascii_lowercase();
  if FORBIDDEN_REQUEST_HEADER_NAMES
    .iter()
    .any(|forbidden| forbidden.as_bytes() == name)
  {
    return true;
  }
  if name.starts_with(b"proxy-") || name.starts_with(b"sec-") {
    return true;
  }
  if METHOD_OVERRIDE_HEADER_NAMES
    .iter()
    .any(|header| header.as_bytes() == name)
  {
    return value
      .split(|byte| *byte == b',')
      .any(|method| is_forbidden_method(trim_http_tab_or_space(method)));
  }
  false
}

/// <https://fetch.spec.whatwg.org/#cors-unsafe-request-header-byte>
fn is_cors_unsafe_request_header_byte(byte: u8) -> bool {
  (byte < 0x20 && byte != b'\t')
    || matches!(
      byte,
      b'"'
        | b'('
        | b')'
        | b':'
        | b'<'
        | b'>'
        | b'?'
        | b'@'
        | b'['
        | b'\\'
        | b']'
        | b'{'
        | b'}'
        | 0x7f
    )
}

/// The essence of a MIME type
/// (<https://mimesniff.spec.whatwg.org/#parse-a-mime-type>), lowercase, or
/// `None` when `value` is not one. Parameters are not looked at: they never
/// make the parsing fail.
fn mime_essence(value: &[u8]) -> Option<String> {
  let value = trim_http_whitespace(value);
  let slash = value.iter().position(|byte| *byte == b'/')?;
  let kind = &value[..slash];
  if !is_token(kind) {
    return None;
  }
  let rest = &value[slash + 1..];
  let end = rest
    .iter()
    .position(|byte| *byte == b';')
    .unwrap_or(rest.len());
  let subtype = trim_http_whitespace(&rest[..end]);
  if !is_token(subtype) {
    return None;
  }
  let mut essence = String::with_capacity(kind.len() + 1 + subtype.len());
  essence.push_str(&String::from_utf8_lossy(kind).to_ascii_lowercase());
  essence.push('/');
  essence.push_str(&String::from_utf8_lossy(subtype).to_ascii_lowercase());
  Some(essence)
}

/// Whether `value` is a `Range` value a simple request may carry: a single
/// `bytes=<first>-` or `bytes=<first>-<last>` range, without white space
/// (<https://fetch.spec.whatwg.org/#simple-range-header-value>, as the
/// CORS-safelisted request-header check uses it).
fn is_simple_range(value: &[u8]) -> bool {
  let Some(rest) = value.strip_prefix(b"bytes=") else {
    return false;
  };
  let Some(dash) = rest.iter().position(|byte| *byte == b'-') else {
    return false;
  };
  let (first, last) = (&rest[..dash], &rest[dash + 1..]);
  if first.is_empty() || !first.iter().all(u8::is_ascii_digit) {
    return false;
  }
  if !last.iter().all(u8::is_ascii_digit) {
    return false;
  }
  if last.is_empty() {
    return true;
  }
  // first <= last, compared as decimal numbers of any length
  let first = strip_leading_zeros(first);
  let last = strip_leading_zeros(last);
  first.len() < last.len() || (first.len() == last.len() && first <= last)
}

fn strip_leading_zeros(mut digits: &[u8]) -> &[u8] {
  while let [b'0', rest @ ..] = digits {
    if rest.is_empty() {
      break;
    }
    digits = rest;
  }
  digits
}

/// <https://fetch.spec.whatwg.org/#cors-safelisted-request-header>
pub fn is_cors_safelisted_request_header(name: &[u8], value: &[u8]) -> bool {
  if value.len() > 128 {
    return false;
  }
  match name.to_ascii_lowercase().as_slice() {
    b"accept" => !value
      .iter()
      .any(|byte| is_cors_unsafe_request_header_byte(*byte)),
    b"accept-language" | b"content-language" => value.iter().all(|byte| {
      byte.is_ascii_alphanumeric()
        || matches!(byte, b' ' | b'*' | b',' | b'-' | b'.' | b';' | b'=')
    }),
    b"content-type" => {
      if value
        .iter()
        .any(|byte| is_cors_unsafe_request_header_byte(*byte))
      {
        return false;
      }
      matches!(
        mime_essence(value).as_deref(),
        Some(
          "application/x-www-form-urlencoded"
            | "multipart/form-data"
            | "text/plain"
        )
      )
    }
    b"range" => is_simple_range(value),
    _ => false,
  }
}

/// <https://fetch.spec.whatwg.org/#no-cors-safelisted-request-header-name>
fn is_no_cors_safelisted_request_header_name(name: &[u8]) -> bool {
  matches!(
    name.to_ascii_lowercase().as_slice(),
    b"accept" | b"accept-language" | b"content-language" | b"content-type"
  )
}

/// <https://fetch.spec.whatwg.org/#no-cors-safelisted-request-header>
pub fn is_no_cors_safelisted_request_header(name: &[u8], value: &[u8]) -> bool {
  is_no_cors_safelisted_request_header_name(name)
    && is_cors_safelisted_request_header(name, value)
}

/// <https://fetch.spec.whatwg.org/#privileged-no-cors-request-header-name>
fn is_privileged_no_cors_request_header_name(name: &[u8]) -> bool {
  name.eq_ignore_ascii_case(b"range")
}

/// <https://fetch.spec.whatwg.org/#cors-non-wildcard-request-header-name>
pub fn is_cors_non_wildcard_request_header_name(name: &[u8]) -> bool {
  name.eq_ignore_ascii_case(b"authorization")
}

/// <https://fetch.spec.whatwg.org/#cors-unsafe-request-header-names>: the
/// names a preflight asks for, lowercase, sorted, each once.
pub fn cors_unsafe_request_header_names(headers: &[Header]) -> Vec<String> {
  let mut unsafe_names: Vec<String> = Vec::new();
  let mut potentially_unsafe_names: Vec<String> = Vec::new();
  let mut safelist_value_size = 0usize;
  for (name, value) in headers {
    let lowercase = String::from_utf8_lossy(name).to_ascii_lowercase();
    if is_cors_safelisted_request_header(name, value) {
      potentially_unsafe_names.push(lowercase);
      safelist_value_size += value.len();
    } else {
      unsafe_names.push(lowercase);
    }
  }
  if safelist_value_size > 1024 {
    unsafe_names.append(&mut potentially_unsafe_names);
  }
  unsafe_names.sort();
  unsafe_names.dedup();
  unsafe_names
}

// --- response headers -----------------------------------------------------

/// <https://fetch.spec.whatwg.org/#forbidden-response-header-name>
pub fn is_forbidden_response_header_name(name: &[u8]) -> bool {
  name.eq_ignore_ascii_case(b"set-cookie")
    || name.eq_ignore_ascii_case(b"set-cookie2")
}

/// <https://fetch.spec.whatwg.org/#cors-safelisted-response-header-name>,
/// without the response's own exposed names.
fn is_cors_safelisted_response_header_name(name: &[u8]) -> bool {
  matches!(
    name.to_ascii_lowercase().as_slice(),
    b"cache-control"
      | b"content-language"
      | b"content-length"
      | b"content-type"
      | b"expires"
      | b"last-modified"
      | b"pragma"
  )
}

/// How much of a response the page may see
/// (<https://fetch.spec.whatwg.org/#concept-request-response-tainting>).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tainting {
  Basic,
  Cors,
  Opaque,
}

impl Tainting {
  fn parse(value: &str) -> Result<Self, WebOriginError> {
    Ok(match value {
      "basic" => Self::Basic,
      "cors" => Self::Cors,
      "opaque" => Self::Opaque,
      _ => return Err(WebOriginError::Invalid("response tainting")),
    })
  }

  fn as_str(self) -> &'static str {
    match self {
      Self::Basic => "basic",
      Self::Cors => "cors",
      Self::Opaque => "opaque",
    }
  }
}

/// The header list of the filtered response a page gets
/// (<https://fetch.spec.whatwg.org/#concept-filtered-response>): without
/// `Set-Cookie` for a basic one; the CORS-safelisted names and the ones
/// `Access-Control-Expose-Headers` lists for a CORS one (`*` exposes every
/// name when the request's credentials mode is not "include"); none for an
/// opaque one.
pub fn filter_response_headers(
  tainting: Tainting,
  headers: Vec<Header>,
  credentials_included: bool,
) -> Vec<Header> {
  match tainting {
    Tainting::Basic => headers
      .into_iter()
      .filter(|(name, _)| !is_forbidden_response_header_name(name))
      .collect(),
    Tainting::Cors => {
      let (all, exposed) =
        match extract_token_list(&headers, "access-control-expose-headers") {
          Extracted::Values(names)
            if !credentials_included
              && names.iter().any(|name| name == "*") =>
          {
            (true, Vec::new())
          }
          Extracted::Values(names) => (false, names),
          Extracted::Missing | Extracted::Failure => (false, Vec::new()),
        };
      headers
        .into_iter()
        .filter(|(name, _)| {
          !is_forbidden_response_header_name(name)
            && (all
              || is_cors_safelisted_response_header_name(name)
              || exposed
                .iter()
                .any(|exposed| exposed.as_bytes().eq_ignore_ascii_case(name)))
        })
        .collect()
    }
    Tainting::Opaque => Vec::new(),
  }
}

/// <https://fetch.spec.whatwg.org/#concept-cors-check>: whether a response
/// with these headers may be shared with a request whose serialized origin
/// is `origin`. The error is the reason, for the embedder's console.
pub fn cors_check(
  response_headers: &[Header],
  origin: &str,
  credentials_included: bool,
) -> Result<(), String> {
  let Some(allowed) =
    get_header(response_headers, "access-control-allow-origin")
  else {
    return Err(
      "no 'Access-Control-Allow-Origin' header is present on the requested resource"
        .to_string(),
    );
  };
  if !credentials_included && allowed == b"*" {
    return Ok(());
  }
  if allowed != origin.as_bytes() {
    return Err(if allowed == b"*" {
      "the 'Access-Control-Allow-Origin' header must not be the wildcard '*' when the request's credentials mode is 'include'".to_string()
    } else {
      format!(
        "the 'Access-Control-Allow-Origin' header has the value '{}', which is not the origin '{origin}'",
        String::from_utf8_lossy(&allowed)
      )
    });
  }
  if !credentials_included {
    return Ok(());
  }
  match get_header(response_headers, "access-control-allow-credentials") {
    Some(value) if value == b"true" => Ok(()),
    _ => Err(
      "the 'Access-Control-Allow-Credentials' header must be 'true' when the request's credentials mode is 'include'"
        .to_string(),
    ),
  }
}

// --- the preflight --------------------------------------------------------

/// What a successful preflight response allows, and for how long.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightGrant {
  pub methods: Vec<String>,
  pub header_names: Vec<String>,
  pub max_age: Duration,
}

/// `Access-Control-Max-Age`: a single header of digits; anything else is the
/// default. Capped at [`PREFLIGHT_MAX_AGE_LIMIT`].
fn preflight_max_age(response_headers: &[Header]) -> Duration {
  let mut values = values_of(response_headers, "access-control-max-age");
  let (Some(value), None) = (values.next(), values.next()) else {
    return PREFLIGHT_MAX_AGE_DEFAULT;
  };
  let value = trim_http_tab_or_space(value);
  if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
    return PREFLIGHT_MAX_AGE_DEFAULT;
  }
  let seconds = std::str::from_utf8(value)
    .ok()
    .and_then(|digits| digits.parse::<u64>().ok())
    // more digits than a u64 holds: beyond any limit
    .unwrap_or(u64::MAX);
  Duration::from_secs(seconds).min(PREFLIGHT_MAX_AGE_LIMIT)
}

/// Steps 7 and 8 of the CORS-preflight fetch
/// (<https://fetch.spec.whatwg.org/#cors-preflight-fetch>): whether the
/// response to a preflight lets the request through.
///
/// `request_headers` is the request's header list (without the headers the
/// user agent adds), `unsafe_names` its CORS-unsafe request-header names.
pub fn check_preflight_response(
  status: u16,
  response_headers: &[Header],
  origin: &str,
  credentials_included: bool,
  method: &str,
  request_headers: &[Header],
  unsafe_names: &[String],
) -> Result<PreflightGrant, String> {
  cors_check(response_headers, origin, credentials_included)?;
  if !(200..=299).contains(&status) {
    return Err(format!(
      "the response to the preflight request has the status {status}, not an ok status"
    ));
  }
  let methods = match extract_token_list(
    response_headers,
    "access-control-allow-methods",
  ) {
    Extracted::Failure => {
      return Err(
          "the 'Access-Control-Allow-Methods' header of the preflight response is not a list of methods"
            .to_string(),
        );
    }
    Extracted::Missing => Vec::new(),
    Extracted::Values(methods) => methods,
  };
  let header_names = match extract_token_list(
    response_headers,
    "access-control-allow-headers",
  ) {
    Extracted::Failure => {
      return Err(
          "the 'Access-Control-Allow-Headers' header of the preflight response is not a list of header names"
            .to_string(),
        );
    }
    Extracted::Missing => Vec::new(),
    Extracted::Values(names) => names,
  };

  if !methods.iter().any(|allowed| allowed == method)
    && !is_cors_safelisted_method(method.as_bytes())
    && (credentials_included || !methods.iter().any(|allowed| allowed == "*"))
  {
    return Err(format!(
      "the method {method} is not allowed by 'Access-Control-Allow-Methods' in the preflight response"
    ));
  }

  let allows = |name: &[u8]| {
    header_names
      .iter()
      .any(|allowed| allowed.as_bytes().eq_ignore_ascii_case(name))
  };
  // `Authorization` is never covered by `*`.
  for (name, _) in request_headers {
    if is_cors_non_wildcard_request_header_name(name) && !allows(name) {
      return Err(format!(
        "the request header {} is not allowed by 'Access-Control-Allow-Headers' in the preflight response",
        String::from_utf8_lossy(name)
      ));
    }
  }
  let wildcard = header_names.iter().any(|allowed| allowed == "*");
  for name in unsafe_names {
    if !allows(name.as_bytes()) && (credentials_included || !wildcard) {
      return Err(format!(
        "the request header {name} is not allowed by 'Access-Control-Allow-Headers' in the preflight response"
      ));
    }
  }

  Ok(PreflightGrant {
    methods,
    header_names,
    max_age: preflight_max_age(response_headers),
  })
}

#[derive(Debug, Clone)]
struct PreflightCacheEntry {
  origin: String,
  url: String,
  expires: Instant,
  credentials: bool,
  /// Exactly one of the two is set.
  method: Option<String>,
  header_name: Option<String>,
}

/// <https://fetch.spec.whatwg.org/#concept-cache>: the CORS-preflight cache
/// of one runtime (which is the network partition: a page runtime is one
/// page). An entry is keyed by the serialized origin of the request and its
/// URL, holds one method or one header name, and is for requests without
/// credentials unless it was made by one with.
#[derive(Debug, Default)]
pub struct PreflightCache {
  entries: Vec<PreflightCacheEntry>,
}

impl PreflightCache {
  fn matching<'a>(
    &'a self,
    now: Instant,
    origin: &'a str,
    url: &'a str,
    credentials_included: bool,
  ) -> impl Iterator<Item = &'a PreflightCacheEntry> {
    self.entries.iter().filter(move |entry| {
      entry.expires > now
        && entry.origin == origin
        && entry.url == url
        && (entry.credentials || !credentials_included)
    })
  }

  /// <https://fetch.spec.whatwg.org/#concept-cache-match-method>
  pub fn method_match(
    &self,
    now: Instant,
    origin: &str,
    url: &str,
    credentials_included: bool,
    method: &str,
  ) -> bool {
    self
      .matching(now, origin, url, credentials_included)
      .any(|entry| {
        entry
          .method
          .as_deref()
          .is_some_and(|allowed| allowed == method || allowed == "*")
      })
  }

  /// <https://fetch.spec.whatwg.org/#concept-cache-match-header>
  pub fn header_name_match(
    &self,
    now: Instant,
    origin: &str,
    url: &str,
    credentials_included: bool,
    header_name: &str,
  ) -> bool {
    self
      .matching(now, origin, url, credentials_included)
      .any(|entry| {
        entry.header_name.as_deref().is_some_and(|allowed| {
          allowed.eq_ignore_ascii_case(header_name)
            || (allowed == "*"
              && !is_cors_non_wildcard_request_header_name(
                header_name.as_bytes(),
              ))
        })
      })
  }

  /// Whether a request has to be preceded by a preflight (step 4.2 of
  /// <https://fetch.spec.whatwg.org/#concept-http-fetch>).
  pub fn needs_preflight(
    &self,
    now: Instant,
    origin: &str,
    url: &str,
    credentials_included: bool,
    method: &str,
    unsafe_names: &[String],
  ) -> bool {
    (!is_cors_safelisted_method(method.as_bytes())
      && !self.method_match(now, origin, url, credentials_included, method))
      || unsafe_names.iter().any(|name| {
        !self.header_name_match(now, origin, url, credentials_included, name)
      })
  }

  /// Remembers what a preflight response granted: an entry per method and
  /// per header name, or a new lifetime for the entry there already is.
  pub fn store(
    &mut self,
    now: Instant,
    origin: &str,
    url: &str,
    credentials_included: bool,
    grant: &PreflightGrant,
  ) {
    self.entries.retain(|entry| entry.expires > now);
    let expires = now + grant.max_age.min(PREFLIGHT_MAX_AGE_LIMIT);
    let items = grant
      .methods
      .iter()
      .map(|method| (Some(method), None))
      .chain(grant.header_names.iter().map(|name| (None, Some(name))));
    for (method, header_name) in items {
      let existing = self.entries.iter_mut().find(|entry| {
        entry.origin == origin
          && entry.url == url
          && (entry.credentials || !credentials_included)
          && entry.method.as_ref() == method
          && match (&entry.header_name, header_name) {
            (Some(entry), Some(name)) => entry.eq_ignore_ascii_case(name),
            (None, None) => true,
            _ => false,
          }
      });
      match existing {
        Some(entry) => entry.expires = expires,
        None => self.entries.push(PreflightCacheEntry {
          origin: origin.to_string(),
          url: url.to_string(),
          expires,
          credentials: credentials_included,
          method: method.cloned(),
          header_name: header_name.cloned(),
        }),
      }
    }
    if self.entries.len() > PREFLIGHT_CACHE_CAPACITY {
      let excess = self.entries.len() - PREFLIGHT_CACHE_CAPACITY;
      self.entries.drain(..excess);
    }
  }

  /// <https://fetch.spec.whatwg.org/#concept-cache-clear>
  pub fn clear(&mut self, origin: &str, url: &str) {
    self
      .entries
      .retain(|entry| entry.origin != origin || entry.url != url);
  }

  /// How many entries are kept (expired ones included until the next
  /// `store`).
  pub fn len(&self) -> usize {
    self.entries.len()
  }

  pub fn is_empty(&self) -> bool {
    self.entries.is_empty()
  }
}

// --- origins and referrers -------------------------------------------------

/// <https://fetch.spec.whatwg.org/#concept-request-tainted-origin>: whether
/// a redirect chain left the request's origin and then crossed origins
/// again, after which the request's origin is serialized as `null`.
pub fn has_redirect_tainted_origin(urls: &[Url], origin: &Origin) -> bool {
  let mut last: Option<Origin> = None;
  for url in urls {
    let url_origin = url.origin();
    if let Some(last) = &last
      && url_origin != *last
      && origin != last
    {
      return true;
    }
    last = Some(url_origin);
  }
  false
}

/// <https://fetch.spec.whatwg.org/#serializing-a-request-origin>
pub fn serialize_request_origin(urls: &[Url], origin: &Origin) -> String {
  if has_redirect_tainted_origin(urls, origin) {
    "null".to_string()
  } else {
    origin.ascii_serialization()
  }
}

/// <https://w3c.github.io/webappsec-referrer-policy/#referrer-policy>
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferrerPolicy {
  NoReferrer,
  NoReferrerWhenDowngrade,
  SameOrigin,
  Origin,
  StrictOrigin,
  OriginWhenCrossOrigin,
  StrictOriginWhenCrossOrigin,
  UnsafeUrl,
}

impl ReferrerPolicy {
  /// The policy of a document that states none.
  pub const DEFAULT: Self = Self::StrictOriginWhenCrossOrigin;

  /// The policy a token names; `None` for the empty string and for anything
  /// that is not a policy.
  pub fn parse(token: &str) -> Option<Self> {
    Some(match token.to_ascii_lowercase().as_str() {
      "no-referrer" => Self::NoReferrer,
      "no-referrer-when-downgrade" => Self::NoReferrerWhenDowngrade,
      "same-origin" => Self::SameOrigin,
      "origin" => Self::Origin,
      "strict-origin" => Self::StrictOrigin,
      "origin-when-cross-origin" => Self::OriginWhenCrossOrigin,
      "strict-origin-when-cross-origin" => Self::StrictOriginWhenCrossOrigin,
      "unsafe-url" => Self::UnsafeUrl,
      _ => return None,
    })
  }

  pub fn as_str(self) -> &'static str {
    match self {
      Self::NoReferrer => "no-referrer",
      Self::NoReferrerWhenDowngrade => "no-referrer-when-downgrade",
      Self::SameOrigin => "same-origin",
      Self::Origin => "origin",
      Self::StrictOrigin => "strict-origin",
      Self::OriginWhenCrossOrigin => "origin-when-cross-origin",
      Self::StrictOriginWhenCrossOrigin => "strict-origin-when-cross-origin",
      Self::UnsafeUrl => "unsafe-url",
    }
  }

  /// <https://w3c.github.io/webappsec-referrer-policy/#parse-referrer-policy-from-header>:
  /// the last policy a response's `Referrer-Policy` headers name.
  pub fn from_response_headers(headers: &[Header]) -> Option<Self> {
    match extract_token_list(headers, "referrer-policy") {
      Extracted::Values(tokens) => {
        tokens.iter().rev().find_map(|token| Self::parse(token))
      }
      Extracted::Missing | Extracted::Failure => None,
    }
  }
}

/// <https://w3c.github.io/webappsec-secure-contexts/#is-url-trustworthy>,
/// for the URLs a request or a referrer has.
pub fn is_potentially_trustworthy(url: &Url) -> bool {
  if matches!(url.scheme(), "https" | "wss" | "file") {
    return true;
  }
  match url.host() {
    Some(Host::Ipv4(address)) => address.is_loopback(),
    Some(Host::Ipv6(address)) => address.is_loopback(),
    Some(Host::Domain(domain)) => {
      let domain = domain.strip_suffix('.').unwrap_or(domain);
      domain.eq_ignore_ascii_case("localhost")
        || domain.to_ascii_lowercase().ends_with(".localhost")
    }
    None => false,
  }
}

/// <https://w3c.github.io/webappsec-referrer-policy/#strip-url>
fn strip_url_for_referrer(url: &Url, origin_only: bool) -> Option<Url> {
  if matches!(url.scheme(), "about" | "blob" | "data") {
    return None;
  }
  let mut url = url.clone();
  let _ = url.set_username("");
  let _ = url.set_password(None);
  url.set_fragment(None);
  if origin_only {
    url.set_path("/");
    url.set_query(None);
  }
  Some(url)
}

/// <https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer>,
/// from step 4: the referrer a request to `current` sends when its referrer
/// is taken from `source` under `policy`; `None` for no referrer.
pub fn determine_referrer(
  source: &Url,
  policy: ReferrerPolicy,
  current: &Url,
) -> Option<Url> {
  let referrer_origin = strip_url_for_referrer(source, true)?;
  let mut referrer_url = strip_url_for_referrer(source, false)?;
  if referrer_url.as_str().len() > MAX_REFERRER_LENGTH {
    referrer_url = referrer_origin.clone();
  }
  let same_origin = referrer_url.origin() == current.origin();
  let downgrade = is_potentially_trustworthy(&referrer_url)
    && !is_potentially_trustworthy(current);
  match policy {
    ReferrerPolicy::NoReferrer => None,
    ReferrerPolicy::Origin => Some(referrer_origin),
    ReferrerPolicy::UnsafeUrl => Some(referrer_url),
    ReferrerPolicy::StrictOrigin => (!downgrade).then_some(referrer_origin),
    ReferrerPolicy::StrictOriginWhenCrossOrigin => {
      if same_origin {
        Some(referrer_url)
      } else {
        (!downgrade).then_some(referrer_origin)
      }
    }
    ReferrerPolicy::SameOrigin => same_origin.then_some(referrer_url),
    ReferrerPolicy::OriginWhenCrossOrigin => Some(if same_origin {
      referrer_url
    } else {
      referrer_origin
    }),
    ReferrerPolicy::NoReferrerWhenDowngrade => {
      (!downgrade).then_some(referrer_url)
    }
  }
}

// --- a request -------------------------------------------------------------

/// <https://fetch.spec.whatwg.org/#concept-request-mode>, without
/// "navigate" and "websocket", which `fetch()` cannot ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
  SameOrigin,
  NoCors,
  Cors,
}

/// <https://fetch.spec.whatwg.org/#concept-request-credentials-mode>
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Credentials {
  Omit,
  SameOrigin,
  Include,
}

/// <https://fetch.spec.whatwg.org/#concept-request-redirect-mode>
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redirect {
  Follow,
  Error,
  Manual,
}

/// <https://fetch.spec.whatwg.org/#concept-request-referrer>
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Referrer {
  NoReferrer,
  Client,
  Url(Url),
}

/// A request as `26_fetch.js` describes it to the ops of this module. The
/// header list travels next to it.
#[derive(Debug, FromV8)]
pub struct WebRequestArgs {
  method: String,
  /// The request's URL list: the URL it was made for, then one for every
  /// redirect followed. The last is the current URL.
  url_list: Vec<String>,
  mode: String,
  credentials: String,
  redirect: String,
  /// `client`, `no-referrer` or a URL.
  referrer: String,
  /// A policy, or the empty string for the document's.
  referrer_policy: String,
  /// The request's response tainting so far: `basic` for a new request,
  /// then what the last [`op_fetch_web_request`] answered.
  tainting: String,
  /// The unsafe-request flag: set for `fetch()`, not for `EventSource`.
  unsafe_request: bool,
}

/// [`WebRequestArgs`], parsed.
#[derive(Debug, Clone)]
pub struct WebRequest {
  pub method: String,
  pub urls: Vec<Url>,
  pub mode: Mode,
  pub credentials: Credentials,
  pub redirect: Redirect,
  pub referrer: Referrer,
  pub referrer_policy: Option<ReferrerPolicy>,
  pub tainting: Tainting,
  pub unsafe_request: bool,
}

impl WebRequest {
  fn parse(args: WebRequestArgs) -> Result<Self, WebOriginError> {
    let urls = args
      .url_list
      .iter()
      .map(|url| Url::parse(url.as_str()))
      .collect::<Result<Vec<Url>, _>>()
      .map_err(|_| WebOriginError::Invalid("URL list"))?;
    if urls.is_empty() {
      return Err(WebOriginError::Invalid("URL list"));
    }
    let mode = match args.mode.as_str() {
      "same-origin" => Mode::SameOrigin,
      "no-cors" => Mode::NoCors,
      "cors" => Mode::Cors,
      _ => return Err(WebOriginError::Invalid("mode")),
    };
    let credentials = match args.credentials.as_str() {
      "omit" => Credentials::Omit,
      "same-origin" => Credentials::SameOrigin,
      "include" => Credentials::Include,
      _ => return Err(WebOriginError::Invalid("credentials mode")),
    };
    let redirect = match args.redirect.as_str() {
      "follow" => Redirect::Follow,
      "error" => Redirect::Error,
      "manual" => Redirect::Manual,
      _ => return Err(WebOriginError::Invalid("redirect mode")),
    };
    let referrer = match args.referrer.as_str() {
      "no-referrer" => Referrer::NoReferrer,
      "client" => Referrer::Client,
      url => Referrer::Url(
        Url::parse(url).map_err(|_| WebOriginError::Invalid("referrer"))?,
      ),
    };
    let referrer_policy = match args.referrer_policy.as_str() {
      "" => None,
      policy => Some(
        ReferrerPolicy::parse(policy)
          .ok_or(WebOriginError::Invalid("referrer policy"))?,
      ),
    };
    Ok(Self {
      method: args.method,
      urls,
      mode,
      credentials,
      redirect,
      referrer,
      referrer_policy,
      tainting: Tainting::parse(&args.tainting)?,
      unsafe_request: args.unsafe_request,
    })
  }

  /// The request's current URL.
  pub fn current_url(&self) -> &Url {
    self.urls.last().expect("a request has a URL")
  }

  fn credentials_included(&self) -> bool {
    self.credentials == Credentials::Include
  }
}

/// What a request becomes on the wire, and how its response is to be
/// treated.
#[derive(Debug, Clone)]
pub struct PreparedRequest {
  /// The response tainting of this hop.
  pub tainting: Tainting,
  /// The request is one main fetch makes "with a CORS preflight": it is
  /// preceded by a preflight unless the cache has what it needs.
  pub with_preflight: bool,
  /// The request's header list without what a page may not set.
  pub sanitized_headers: Vec<Header>,
  /// The CORS-unsafe request-header names of that list.
  pub unsafe_names: Vec<String>,
  /// The headers to send: the sanitized list, then `Referer` and `Origin`.
  pub headers: Vec<Header>,
  /// The current URL, without credentials.
  pub url: Url,
  /// The serialized origin of the request (`null` once a redirect chain
  /// tainted it).
  pub origin: String,
  /// `Referer`, when one is sent.
  pub referrer: Option<Url>,
  /// The policy the referrer was determined under.
  pub referrer_policy: ReferrerPolicy,
  /// `Origin`, when one is sent.
  pub origin_header: Option<String>,
  /// Whether the user agent's credentials go with the request.
  pub include_credentials: bool,
}

/// The headers of `headers` a request made in `mode` keeps: none a page is
/// forbidden to set, and for "no-cors" only the no-CORS-safelisted ones
/// (looking at the combined value of each name, as the `Headers` guard
/// does when a header is appended).
fn sanitize_request_headers(headers: &[Header], mode: Mode) -> Vec<Header> {
  let kept: Vec<Header> = headers
    .iter()
    .filter(|(name, value)| !is_forbidden_request_header(name, value))
    .cloned()
    .collect();
  if mode != Mode::NoCors {
    return kept;
  }
  kept
    .iter()
    .filter(|(name, _)| {
      let lowercase = String::from_utf8_lossy(name).to_ascii_lowercase();
      get_header(&kept, &lowercase).is_some_and(|combined| {
        is_no_cors_safelisted_request_header(name, &combined)
      })
    })
    .cloned()
    .collect()
}

/// Main fetch, up to the request leaving
/// (<https://fetch.spec.whatwg.org/#concept-main-fetch> steps 7 to 11, and
/// the header steps of
/// <https://fetch.spec.whatwg.org/#concept-http-network-or-cache-fetch>):
/// the response tainting of a request to its current URL, the headers it is
/// sent with, or the reason it is a network error.
pub fn prepare_request(
  web: &EmbedderWebOrigin,
  request: &WebRequest,
  headers: &[Header],
) -> Result<PreparedRequest, String> {
  let current = request.current_url();
  let method = request.method.as_bytes();
  if is_forbidden_method(method) {
    return Err(format!("the method {} is forbidden", request.method));
  }

  // Main fetch, steps 7 and 8.
  let referrer_policy =
    request.referrer_policy.unwrap_or(ReferrerPolicy::DEFAULT);
  let referrer = match &request.referrer {
    Referrer::NoReferrer => None,
    // The Request constructor's rule, held here too: a referrer of another
    // origin is the client's.
    Referrer::Url(url) if url.origin() == web.origin => Some(url),
    Referrer::Url(_) | Referrer::Client => web.referrer_source(),
  }
  .and_then(|source| determine_referrer(source, referrer_policy, current));

  // Main fetch, step 11.
  let is_http = matches!(current.scheme(), "http" | "https");
  let same_origin = current.origin() == web.origin;
  let tainting = if (same_origin && request.tainting == Tainting::Basic)
    || current.scheme() == "data"
  {
    Tainting::Basic
  } else {
    let tainting = match request.mode {
      Mode::SameOrigin => {
        return Err(
          "the request's mode is 'same-origin' and the URL is of another origin"
            .to_string(),
        );
      }
      Mode::NoCors => {
        if request.redirect != Redirect::Follow {
          return Err(
            "a 'no-cors' request to another origin must have the redirect mode 'follow'"
              .to_string(),
          );
        }
        Tainting::Opaque
      }
      Mode::Cors => Tainting::Cors,
    };
    if !is_http {
      return Err(format!(
        "cross-origin requests are only supported for HTTP and HTTPS, not for '{}:'",
        current.scheme()
      ));
    }
    tainting
  };

  let sanitized_headers = sanitize_request_headers(headers, request.mode);
  let unsafe_names = cors_unsafe_request_header_names(&sanitized_headers);
  let with_preflight = tainting == Tainting::Cors
    && request.unsafe_request
    && (!is_cors_safelisted_method(method) || !unsafe_names.is_empty());

  let origin = serialize_request_origin(&request.urls, &web.origin);

  // https://fetch.spec.whatwg.org/#append-a-request-origin-header
  let origin_header = if !is_http {
    None
  } else if tainting == Tainting::Cors {
    Some(origin.clone())
  } else if method != b"GET" && method != b"HEAD" {
    let mut serialized = origin.clone();
    if request.mode != Mode::Cors {
      let hidden = match referrer_policy {
        ReferrerPolicy::NoReferrer => true,
        ReferrerPolicy::NoReferrerWhenDowngrade
        | ReferrerPolicy::StrictOrigin
        | ReferrerPolicy::StrictOriginWhenCrossOrigin => {
          matches!(&web.origin, Origin::Tuple(scheme, _, _) if scheme == "https")
            && current.scheme() != "https"
        }
        ReferrerPolicy::SameOrigin => !same_origin,
        ReferrerPolicy::Origin
        | ReferrerPolicy::OriginWhenCrossOrigin
        | ReferrerPolicy::UnsafeUrl => false,
      };
      if hidden {
        serialized = "null".to_string();
      }
    }
    Some(serialized)
  } else {
    None
  };

  let mut url = current.clone();
  if is_http {
    // Credentials in a URL only get here through a redirect (the Request
    // constructor refuses them), and a browser uses them to answer an
    // authentication challenge, never before one.
    let _ = url.set_username("");
    let _ = url.set_password(None);
  }

  // HTTP-network-or-cache fetch, step 8.4.
  let include_credentials = request.credentials == Credentials::Include
    || (request.credentials == Credentials::SameOrigin
      && tainting == Tainting::Basic);

  let mut headers = sanitized_headers.clone();
  if is_http {
    if let Some(referrer) = &referrer {
      headers.push(("referer".into(), referrer.as_str().into()));
    }
    if let Some(origin) = &origin_header {
      headers.push(("origin".into(), origin.as_str().into()));
    }
    attach_credentials(web, include_credentials, &url, &mut headers);
  }

  Ok(PreparedRequest {
    tainting,
    with_preflight,
    sanitized_headers,
    unsafe_names,
    headers,
    url,
    origin,
    referrer,
    referrer_policy,
    origin_header,
    include_credentials,
  })
}

/// The hook of a cookie store and of an HTTP authentication cache
/// (HTTP-network-or-cache fetch, step 8.21): when `include_credentials` is
/// set, the `Cookie` header of `url` and, without an `Authorization` header
/// of the page's, the authentication entry of `url` are appended here.
///
/// TODO(denext): Thalora's cookie store. There is none yet, so no request
/// carries a credential the page did not write itself. The store comes in
/// with the [`EmbedderWebOrigin`] (it is the embedder's), is asked here on
/// every hop and for every `fetch()` path, and never for a preflight (whose
/// credentials mode is "same-origin" on a cross-origin URL).
fn attach_credentials(
  _web: &EmbedderWebOrigin,
  _include_credentials: bool,
  _url: &Url,
  _headers: &mut Vec<Header>,
) {
}

/// The other half of the hook (HTTP-network fetch, step 14: "parse and
/// store" the `Set-Cookie` headers of a response to a request made with
/// credentials). Called with the response's full header list, before it is
/// filtered for the page.
///
/// TODO(denext): Thalora's cookie store; see [`attach_credentials`].
fn store_response_credentials(
  _web: &EmbedderWebOrigin,
  _include_credentials: bool,
  _url: &Url,
  _response_headers: &[Header],
) {
}

/// Steps 8 and 9 of <https://fetch.spec.whatwg.org/#concept-http-redirect-fetch>:
/// whether `request` may follow a redirect to `location`.
pub fn check_redirect(
  web: &EmbedderWebOrigin,
  request: &WebRequest,
  location: &Url,
) -> Result<(), String> {
  let has_credentials =
    !location.username().is_empty() || location.password().is_some();
  if !has_credentials {
    return Ok(());
  }
  if request.mode == Mode::Cors && location.origin() != web.origin {
    return Err(
      "a 'cors' request was redirected to a URL of another origin that includes credentials"
        .to_string(),
    );
  }
  if request.tainting == Tainting::Cors {
    return Err(
      "a cross-origin request was redirected to a URL that includes credentials"
        .to_string(),
    );
  }
  Ok(())
}

// --- ops -------------------------------------------------------------------

/// Whether the runtime's `fetch()` is in web-origin mode. Asked once per
/// realm; Deno's answer is `false`, and none of the other ops is called.
#[op2(fast)]
pub fn op_fetch_web_mode(state: &mut OpState) -> bool {
  state.has::<EmbedderWebOrigin>()
}

const HEADERS_APPEND: u32 = 0;
const HEADERS_SET: u32 = 1;
const HEADERS_DELETE: u32 = 2;

/// Whether a `Headers` object with `guard` ignores an operation
/// (<https://fetch.spec.whatwg.org/#headers-validate> and the guard steps
/// of `append`, `set` and `delete`). For `append` on a "request-no-cors"
/// guard, `value` is the combined value the header would have.
pub fn headers_guard_blocks(
  guard: &str,
  operation: u32,
  name: &[u8],
  value: &[u8],
) -> bool {
  match guard {
    "request" => is_forbidden_request_header(name, value),
    "request-no-cors" => {
      if is_forbidden_request_header(name, value) {
        return true;
      }
      if operation == HEADERS_DELETE {
        !is_no_cors_safelisted_request_header_name(name)
          && !is_privileged_no_cors_request_header_name(name)
      } else {
        !is_no_cors_safelisted_request_header(name, value)
      }
    }
    "response" => is_forbidden_response_header_name(name),
    _ => false,
  }
}

#[op2]
pub fn op_fetch_web_header_blocked(
  #[string] guard: String,
  #[smi] operation: u32,
  #[scoped] name: ByteString,
  #[scoped] value: ByteString,
) -> bool {
  debug_assert!(matches!(
    operation,
    HEADERS_APPEND | HEADERS_SET | HEADERS_DELETE
  ));
  headers_guard_blocks(&guard, operation, &name, &value)
}

/// What [`op_fetch_web_request`] answers.
#[derive(Debug, ToV8)]
pub struct WebRequestPlan {
  /// The reason the request is a network error, when it is one.
  error: Option<String>,
  /// The response tainting of this hop.
  tainting: String,
  /// A CORS-preflight fetch has to succeed first.
  preflight: bool,
  /// The header list to send.
  headers: Vec<Header>,
  /// The URL to send to.
  url: String,
  /// The request's referrer from here on: `no-referrer` or a URL.
  referrer: String,
  /// The request's referrer policy from here on.
  referrer_policy: String,
}

fn preflight_cache(state: &mut OpState) -> &mut PreflightCache {
  if !state.has::<PreflightCache>() {
    state.put(PreflightCache::default());
  }
  state.borrow_mut::<PreflightCache>()
}

fn web_origin(state: &OpState) -> Result<EmbedderWebOrigin, WebOriginError> {
  EmbedderWebOrigin::from_state(state).ok_or(WebOriginError::NotEnabled)
}

/// Before a request leaves, on every hop.
#[op2]
pub fn op_fetch_web_request(
  state: &mut OpState,
  #[scoped] args: WebRequestArgs,
  #[scoped] headers: Vec<Header>,
) -> Result<WebRequestPlan, WebOriginError> {
  let web = web_origin(state)?;
  let request = WebRequest::parse(args)?;
  let prepared = match prepare_request(&web, &request, &headers) {
    Ok(prepared) => prepared,
    Err(reason) => {
      web.report_blocked(state, request.current_url().as_str(), &reason);
      return Ok(WebRequestPlan {
        error: Some(reason),
        tainting: request.tainting.as_str().to_string(),
        preflight: false,
        headers: Vec::new(),
        url: String::new(),
        referrer: "no-referrer".to_string(),
        referrer_policy: String::new(),
      });
    }
  };
  let preflight = prepared.with_preflight
    && preflight_cache(state).needs_preflight(
      Instant::now(),
      &prepared.origin,
      request.current_url().as_str(),
      request.credentials_included(),
      &request.method,
      &prepared.unsafe_names,
    );
  Ok(WebRequestPlan {
    error: None,
    tainting: prepared.tainting.as_str().to_string(),
    preflight,
    headers: prepared.headers,
    url: prepared.url.into(),
    referrer: match prepared.referrer {
      Some(referrer) => referrer.into(),
      None => "no-referrer".to_string(),
    },
    referrer_policy: prepared.referrer_policy.as_str().to_string(),
  })
}

/// What the ops that only pass or fail answer: the reason of the network
/// error, or `null`.
#[derive(Debug, ToV8)]
pub struct WebVerdict {
  error: Option<String>,
}

/// <https://fetch.spec.whatwg.org/#cors-preflight-fetch>, steps 1 to 5: the
/// preflight of a prepared request.
fn build_preflight_request(
  prepared: &PreparedRequest,
  method: &str,
) -> Result<http::Request<ReqBody>, WebOriginError> {
  let invalid = || WebOriginError::Invalid("preflight request");
  let uri = prepared
    .url
    .as_str()
    .parse::<Uri>()
    .map_err(|_| WebOriginError::Invalid("URL list"))?;
  let mut preflight = http::Request::new(ReqBody::empty());
  *preflight.method_mut() = Method::OPTIONS;
  *preflight.uri_mut() = uri;
  let headers = preflight.headers_mut();
  headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
  headers.insert(
    ACCESS_CONTROL_REQUEST_METHOD,
    HeaderValue::from_str(method).map_err(|_| invalid())?,
  );
  if !prepared.unsafe_names.is_empty() {
    headers.insert(
      ACCESS_CONTROL_REQUEST_HEADERS,
      HeaderValue::from_str(&prepared.unsafe_names.join(","))
        .map_err(|_| invalid())?,
    );
  }
  headers.insert(
    ORIGIN,
    HeaderValue::from_str(&prepared.origin).map_err(|_| invalid())?,
  );
  if let Some(referrer) = &prepared.referrer {
    headers.insert(
      REFERER,
      HeaderValue::from_str(referrer.as_str()).map_err(|_| invalid())?,
    );
  }
  Ok(preflight)
}

/// The CORS-preflight fetch of a request [`op_fetch_web_request`] answered
/// `preflight` for: an `OPTIONS` request without credentials, sent through
/// the runtime's client (so under the same permissions and address deny
/// list as the request itself), whose response has to pass the CORS check
/// and allow the method and the header names. What it allows is cached.
///
/// A redirect is not followed: its status is not an ok status.
#[op2]
pub async fn op_fetch_web_preflight(
  state: Rc<RefCell<OpState>>,
  #[scoped] args: WebRequestArgs,
  #[scoped] headers: Vec<Header>,
  #[smi] cancel_rid: Option<ResourceId>,
) -> Result<WebVerdict, WebOriginError> {
  let request = WebRequest::parse(args)?;
  let current = request.current_url().to_string();
  let (web, prepared, client, cancel, preflight) = {
    let mut state = state.borrow_mut();
    let web = web_origin(&state)?;
    let prepared = match prepare_request(&web, &request, &headers) {
      Ok(prepared) => prepared,
      Err(reason) => {
        web.report_blocked(&mut state, &current, &reason);
        return Ok(WebVerdict {
          error: Some(reason),
        });
      }
    };
    state
      .borrow_mut::<PermissionsContainer>()
      .check_net_connect_url(&prepared.url, "fetch()")?;
    let mut preflight = build_preflight_request(&prepared, &request.method)?;
    if let Some(request_builder_hook) =
      state.borrow::<Options>().request_builder_hook
    {
      request_builder_hook(&mut preflight)
        .map_err(|_| WebOriginError::Invalid("preflight request"))?;
    }
    let client = get_or_create_client_from_state(&mut state)?;
    let cancel = match cancel_rid {
      Some(rid) => Some(state.resource_table.get::<CancelHandle>(rid)?),
      None => None,
    };
    (web, prepared, client, cancel, preflight)
  };

  let sent = client.send(preflight);
  let response = match cancel {
    Some(cancel) => match sent.or_cancel(cancel).await {
      Ok(response) => response,
      Err(_canceled) => {
        return Ok(WebVerdict {
          error: Some("the preflight request was aborted".to_string()),
        });
      }
    },
    None => sent.await,
  };

  let outcome = match response {
    Ok(response) => {
      let status = response.status().as_u16();
      let response_headers: Vec<Header> = response
        .headers()
        .iter()
        .map(|(name, value)| (name.as_str().into(), value.as_bytes().into()))
        .collect();
      // The body of a preflight response is never read.
      drop(response);
      check_preflight_response(
        status,
        &response_headers,
        &prepared.origin,
        request.credentials_included(),
        &request.method,
        &prepared.sanitized_headers,
        &prepared.unsafe_names,
      )
    }
    Err(error) => Err(format!("the preflight request failed: {error}")),
  };

  let mut state = state.borrow_mut();
  match outcome {
    Ok(grant) => {
      preflight_cache(&mut state).store(
        Instant::now(),
        &prepared.origin,
        &current,
        request.credentials_included(),
        &grant,
      );
      Ok(WebVerdict { error: None })
    }
    Err(reason) => {
      web.report_blocked(&mut state, &current, &reason);
      Ok(WebVerdict {
        error: Some(reason),
      })
    }
  }
}

/// What [`op_fetch_web_response`] answers.
#[derive(Debug, ToV8)]
pub struct WebResponse {
  /// The reason the response is a network error, when it is one.
  error: Option<String>,
  /// The header list of the filtered response.
  headers: Vec<Header>,
}

/// When a response arrived, redirects included: the CORS check of a request
/// whose response tainting is "cors" (step 4.5 of
/// <https://fetch.spec.whatwg.org/#concept-http-fetch>), and the header list
/// the page may see (main fetch, steps 12 to 14).
#[op2]
pub fn op_fetch_web_response(
  state: &mut OpState,
  #[scoped] args: WebRequestArgs,
  #[scoped] headers: Vec<Header>,
) -> Result<WebResponse, WebOriginError> {
  let web = web_origin(state)?;
  let request = WebRequest::parse(args)?;
  let current = request.current_url();
  if request.tainting == Tainting::Cors {
    let origin = serialize_request_origin(&request.urls, &web.origin);
    if let Err(reason) =
      cors_check(&headers, &origin, request.credentials_included())
    {
      preflight_cache(state).clear(&origin, current.as_str());
      web.report_blocked(state, current.as_str(), &reason);
      return Ok(WebResponse {
        error: Some(reason),
        headers: Vec::new(),
      });
    }
  }
  let include_credentials = request.credentials == Credentials::Include
    || (request.credentials == Credentials::SameOrigin
      && request.tainting == Tainting::Basic);
  store_response_credentials(&web, include_credentials, current, &headers);
  Ok(WebResponse {
    error: None,
    headers: filter_response_headers(
      request.tainting,
      headers,
      request.credentials_included(),
    ),
  })
}

/// What [`op_fetch_web_redirect`] answers.
#[derive(Debug, ToV8)]
pub struct WebRedirect {
  /// The reason the redirect is a network error, when it is one.
  error: Option<String>,
  /// The request's referrer policy from here on (the redirect response's
  /// `Referrer-Policy`, when it names one).
  referrer_policy: String,
}

/// Before a redirect is followed: the credentials rules of steps 8 and 9 of
/// <https://fetch.spec.whatwg.org/#concept-http-redirect-fetch>, and its
/// step 18 (the referrer policy a redirect response sets). `args` is the
/// request before the redirect, `headers` the redirect response's.
#[op2]
pub fn op_fetch_web_redirect(
  state: &mut OpState,
  #[scoped] args: WebRequestArgs,
  #[string] location: String,
  #[scoped] headers: Vec<Header>,
) -> Result<WebRedirect, WebOriginError> {
  let web = web_origin(state)?;
  let request = WebRequest::parse(args)?;
  let referrer_policy = ReferrerPolicy::from_response_headers(&headers)
    .or(request.referrer_policy)
    .map(|policy| policy.as_str().to_string())
    .unwrap_or_default();
  let location = Url::parse(&location)
    .map_err(|_| WebOriginError::Invalid("redirect location"))?;
  let error = check_redirect(&web, &request, &location).err();
  if let Some(reason) = &error {
    web.report_blocked(state, request.current_url().as_str(), reason);
  }
  Ok(WebRedirect {
    error,
    referrer_policy,
  })
}

/// A request whose response tainting is "cors" ended in a network error:
/// what the preflight cache holds for it is dropped (main fetch, step 11,
/// "clear cache entries").
#[op2]
pub fn op_fetch_web_failed(
  state: &mut OpState,
  #[scoped] args: WebRequestArgs,
) -> Result<(), WebOriginError> {
  let web = web_origin(state)?;
  let request = WebRequest::parse(args)?;
  let origin = serialize_request_origin(&request.urls, &web.origin);
  preflight_cache(state).clear(&origin, request.current_url().as_str());
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn headers(list: &[(&str, &str)]) -> Vec<Header> {
    list
      .iter()
      .map(|(name, value)| ((*name).into(), (*value).into()))
      .collect()
  }

  fn names(list: &[Header]) -> Vec<String> {
    list
      .iter()
      .map(|(name, _)| String::from_utf8_lossy(name).into_owned())
      .collect()
  }

  fn value_of(list: &[Header], name: &str) -> Option<String> {
    get_header(list, name)
      .map(|value| String::from_utf8_lossy(&value).into_owned())
  }

  fn page(url: &str) -> EmbedderWebOrigin {
    EmbedderWebOrigin::new(Url::parse(url).unwrap())
  }

  fn request(method: &str, urls: &[&str]) -> WebRequest {
    WebRequest {
      method: method.to_string(),
      urls: urls.iter().map(|url| Url::parse(url).unwrap()).collect(),
      mode: Mode::Cors,
      credentials: Credentials::SameOrigin,
      redirect: Redirect::Follow,
      referrer: Referrer::Client,
      referrer_policy: None,
      tainting: Tainting::Basic,
      unsafe_request: true,
    }
  }

  const PAGE: &str = "https://app.example/dir/page.html?q=1#frag";

  #[test]
  fn forbidden_methods_and_request_headers() {
    for method in ["CONNECT", "trace", "Track"] {
      assert!(is_forbidden_method(method.as_bytes()), "{method}");
    }
    for method in ["GET", "POST", "OPTIONS", "PATCH", "CONNECTX"] {
      assert!(!is_forbidden_method(method.as_bytes()), "{method}");
    }

    for name in [
      "Host",
      "cookie",
      "Cookie2",
      "ORIGIN",
      "Referer",
      "Content-Length",
      "Accept-Encoding",
      "Access-Control-Request-Method",
      "Sec-Fetch-Site",
      "sec-anything",
      "Proxy-Authorization",
      "proxy-x",
      "Set-Cookie",
      "Via",
      "DNT",
      "TE",
    ] {
      assert!(is_forbidden_request_header(name.as_bytes(), b"x"), "{name}");
    }
    for name in [
      "Authorization",
      "User-Agent",
      "Content-Type",
      "X-Custom",
      "Accept",
      "Security",
      "Proxy",
    ] {
      assert!(
        !is_forbidden_request_header(name.as_bytes(), b"x"),
        "{name}"
      );
    }

    // A method override is forbidden when it names a forbidden method.
    assert!(is_forbidden_request_header(
      b"X-HTTP-Method-Override",
      b"TRACE"
    ));
    assert!(is_forbidden_request_header(
      b"x-method-override",
      b"GET, connect"
    ));
    assert!(!is_forbidden_request_header(b"X-HTTP-Method", b"PUT"));
    assert!(!is_forbidden_request_header(b"X-HTTP-Method", b""));
  }

  #[test]
  fn cors_safelisted_request_headers() {
    let safe = is_cors_safelisted_request_header;
    assert!(safe(b"Accept", b"text/html, */*;q=0.8"));
    assert!(!safe(b"accept", b"text/html(comment)"));
    assert!(safe(b"Accept-Language", b"en-US,en;q=0.9, *"));
    assert!(!safe(b"Accept-Language", b"en_US"));
    assert!(safe(b"Content-Language", b"de"));
    assert!(safe(b"Content-Type", b"text/plain;charset=UTF-8"));
    assert!(safe(b"content-type", b"Multipart/Form-Data; boundary=x"));
    assert!(safe(b"content-type", b"application/x-www-form-urlencoded"));
    assert!(!safe(b"content-type", b"application/json"));
    assert!(!safe(b"content-type", b"text/plain; a=\"b\""));
    assert!(!safe(b"content-type", b"text"));
    assert!(safe(b"Range", b"bytes=0-"));
    assert!(safe(b"range", b"bytes=10-200"));
    assert!(!safe(b"range", b"bytes=-5"));
    assert!(!safe(b"range", b"bytes=200-10"));
    assert!(!safe(b"range", b"bytes=0-1,5-6"));
    assert!(!safe(b"range", b"bytes = 0-"));
    assert!(!safe(b"range", b"items=0-"));
    assert!(!safe(b"X-Custom", b"1"));
    assert!(!safe(b"Authorization", b"Bearer x"));
    // A value longer than 128 bytes is never safelisted.
    assert!(safe(b"accept", "a".repeat(128).as_bytes()));
    assert!(!safe(b"accept", "a".repeat(129).as_bytes()));

    assert!(is_cors_safelisted_method(b"GET"));
    assert!(is_cors_safelisted_method(b"HEAD"));
    assert!(is_cors_safelisted_method(b"POST"));
    assert!(!is_cors_safelisted_method(b"PUT"));
    assert!(!is_cors_safelisted_method(b"get"));

    assert!(is_no_cors_safelisted_request_header(b"Accept", b"*/*"));
    assert!(!is_no_cors_safelisted_request_header(b"Range", b"bytes=0-"));
    assert!(!is_no_cors_safelisted_request_header(
      b"Content-Type",
      b"application/json"
    ));
  }

  #[test]
  fn cors_unsafe_names_are_sorted_lowercase_and_unique() {
    let list = headers(&[
      ("X-B", "1"),
      ("Accept", "*/*"),
      ("x-a", "2"),
      ("X-b", "3"),
      ("Content-Type", "application/json"),
    ]);
    assert_eq!(
      cors_unsafe_request_header_names(&list),
      ["content-type", "x-a", "x-b"]
    );
    assert!(
      cors_unsafe_request_header_names(&headers(&[
        ("Accept", "*/*"),
        ("Accept-Language", "*"),
        ("Content-Type", "text/plain"),
      ]))
      .is_empty()
    );

    // More than 1024 bytes of safelisted values make all of them unsafe.
    let long = "a".repeat(120);
    let many: Vec<(&str, &str)> =
      (0..9).map(|_| ("Accept", long.as_str())).collect();
    assert_eq!(
      cors_unsafe_request_header_names(&headers(&many)),
      ["accept"]
    );
  }

  #[test]
  fn headers_guards() {
    let blocks = headers_guard_blocks;
    assert!(blocks("request", HEADERS_APPEND, b"Cookie", b"a=b"));
    assert!(blocks("request", HEADERS_SET, b"Host", b"evil"));
    assert!(blocks("request", HEADERS_DELETE, b"Origin", b""));
    assert!(!blocks("request", HEADERS_APPEND, b"X-Custom", b"1"));
    assert!(!blocks("request", HEADERS_DELETE, b"X-HTTP-Method", b""));

    assert!(blocks("request-no-cors", HEADERS_APPEND, b"X-Custom", b"1"));
    assert!(blocks(
      "request-no-cors",
      HEADERS_SET,
      b"Content-Type",
      b"application/json"
    ));
    assert!(!blocks(
      "request-no-cors",
      HEADERS_SET,
      b"Content-Type",
      b"text/plain"
    ));
    assert!(blocks(
      "request-no-cors",
      HEADERS_APPEND,
      b"Range",
      b"bytes=0-"
    ));
    assert!(!blocks("request-no-cors", HEADERS_DELETE, b"Accept", b""));
    assert!(!blocks("request-no-cors", HEADERS_DELETE, b"Range", b""));
    assert!(blocks("request-no-cors", HEADERS_DELETE, b"X-Custom", b""));

    assert!(blocks("response", HEADERS_APPEND, b"Set-Cookie", b"a=b"));
    assert!(blocks("response", HEADERS_SET, b"set-cookie2", b"a=b"));
    assert!(!blocks("response", HEADERS_APPEND, b"Content-Type", b"a/b"));

    // Deno's guards never block.
    assert!(!blocks("none", HEADERS_APPEND, b"Cookie", b"a=b"));
    assert!(!blocks("immutable", HEADERS_APPEND, b"Cookie", b"a=b"));
  }

  #[test]
  fn list_extraction() {
    let list = headers(&[
      ("Access-Control-Allow-Methods", "PUT, DELETE ,,"),
      ("access-control-allow-methods", "PATCH"),
      ("Access-Control-Allow-Headers", "x-a, bad value"),
    ]);
    assert_eq!(
      extract_token_list(&list, "access-control-allow-methods"),
      Extracted::Values(vec![
        "PUT".to_string(),
        "DELETE".to_string(),
        "PATCH".to_string()
      ])
    );
    assert_eq!(
      extract_token_list(&list, "access-control-allow-headers"),
      Extracted::Failure
    );
    assert_eq!(
      extract_token_list(&list, "access-control-expose-headers"),
      Extracted::Missing
    );
    assert_eq!(
      get_header(&list, "access-control-allow-methods").unwrap(),
      b"PUT, DELETE ,,, PATCH"
    );
    assert_eq!(get_header(&list, "x-missing"), None);
  }

  #[test]
  fn cors_check_matches_the_origin() {
    let origin = "https://app.example";
    let check = |list: &[(&str, &str)], include: bool| {
      cors_check(&headers(list), origin, include).is_ok()
    };
    // No header: failure.
    assert!(!check(&[], false));
    assert!(!check(&[("Content-Type", "text/plain")], false));
    // The wildcard, without credentials only.
    assert!(check(&[("Access-Control-Allow-Origin", "*")], false));
    assert!(!check(&[("Access-Control-Allow-Origin", "*")], true));
    assert!(!check(
      &[
        ("Access-Control-Allow-Origin", "*"),
        ("Access-Control-Allow-Credentials", "true"),
      ],
      true
    ));
    // The exact origin, byte for byte.
    assert!(check(&[("access-control-allow-origin", origin)], false));
    assert!(!check(
      &[("Access-Control-Allow-Origin", "https://APP.example")],
      false
    ));
    assert!(!check(
      &[("Access-Control-Allow-Origin", "https://app.example/")],
      false
    ));
    assert!(!check(
      &[("Access-Control-Allow-Origin", "https://other.example")],
      false
    ));
    assert!(!check(
      &[("Access-Control-Allow-Origin", "https://app.example:8443")],
      false
    ));
    // Two headers are one combined value, which is no origin.
    assert!(!check(
      &[
        ("Access-Control-Allow-Origin", origin),
        ("Access-Control-Allow-Origin", origin),
      ],
      false
    ));
    assert!(!check(
      &[("Access-Control-Allow-Origin", "https://app.example, *")],
      false
    ));
    // Credentials need `Access-Control-Allow-Credentials: true`.
    assert!(!check(&[("Access-Control-Allow-Origin", origin)], true));
    assert!(check(
      &[
        ("Access-Control-Allow-Origin", origin),
        ("Access-Control-Allow-Credentials", "true"),
      ],
      true
    ));
    assert!(!check(
      &[
        ("Access-Control-Allow-Origin", origin),
        ("Access-Control-Allow-Credentials", "True"),
      ],
      true
    ));
    // An opaque origin is `null`, which a server can name.
    assert!(
      cors_check(
        &headers(&[("Access-Control-Allow-Origin", "null")]),
        "null",
        false
      )
      .is_ok()
    );
    assert!(
      cors_check(
        &headers(&[("Access-Control-Allow-Origin", origin)]),
        "null",
        false
      )
      .is_err()
    );
  }

  #[test]
  fn response_header_filters() {
    let response = || {
      headers(&[
        ("Content-Type", "text/plain"),
        ("Set-Cookie", "a=b"),
        ("set-cookie2", "c=d"),
        ("X-Secret", "1"),
        ("X-Shown", "2"),
        ("Cache-Control", "no-store"),
        ("Access-Control-Expose-Headers", "x-shown, Set-Cookie"),
        ("Access-Control-Allow-Origin", "*"),
      ])
    };
    assert_eq!(
      names(&filter_response_headers(Tainting::Basic, response(), false)),
      [
        "Content-Type",
        "X-Secret",
        "X-Shown",
        "Cache-Control",
        "Access-Control-Expose-Headers",
        "Access-Control-Allow-Origin",
      ]
    );
    // The safelisted names and the exposed ones; never `Set-Cookie`.
    assert_eq!(
      names(&filter_response_headers(Tainting::Cors, response(), false)),
      ["Content-Type", "X-Shown", "Cache-Control"]
    );
    assert!(
      filter_response_headers(Tainting::Opaque, response(), false).is_empty()
    );

    // `*` exposes everything but `Set-Cookie`, without credentials only.
    let wildcard = || {
      headers(&[
        ("Content-Length", "2"),
        ("X-Secret", "1"),
        ("Set-Cookie", "a=b"),
        ("Access-Control-Expose-Headers", "*"),
      ])
    };
    assert_eq!(
      names(&filter_response_headers(Tainting::Cors, wildcard(), false)),
      [
        "Content-Length",
        "X-Secret",
        "Access-Control-Expose-Headers"
      ]
    );
    assert_eq!(
      names(&filter_response_headers(Tainting::Cors, wildcard(), true)),
      ["Content-Length"]
    );
    // An unparsable list exposes nothing more.
    assert_eq!(
      names(&filter_response_headers(
        Tainting::Cors,
        headers(&[
          ("Expires", "0"),
          ("X-Secret", "1"),
          ("Access-Control-Expose-Headers", "x secret"),
        ]),
        false
      )),
      ["Expires"]
    );
  }

  #[test]
  fn preflight_response_rules() {
    let origin = "https://app.example";
    let check = |status: u16,
                 response: &[(&str, &str)],
                 include: bool,
                 method: &str,
                 request_headers: &[(&str, &str)]| {
      let request_headers = headers(request_headers);
      let unsafe_names = cors_unsafe_request_header_names(&request_headers);
      check_preflight_response(
        status,
        &headers(response),
        origin,
        include,
        method,
        &request_headers,
        &unsafe_names,
      )
    };
    let custom = [("X-Custom", "1")];

    let grant = check(
      204,
      &[
        ("Access-Control-Allow-Origin", origin),
        ("Access-Control-Allow-Methods", "PUT, DELETE"),
        ("Access-Control-Allow-Headers", "X-CUSTOM"),
        ("Access-Control-Max-Age", "600"),
      ],
      false,
      "PUT",
      &custom,
    )
    .unwrap();
    assert_eq!(grant.methods, ["PUT", "DELETE"]);
    assert_eq!(grant.header_names, ["X-CUSTOM"]);
    assert_eq!(grant.max_age, Duration::from_secs(600));

    // The CORS check comes first, then the status.
    assert!(
      check(
        200,
        &[("Access-Control-Allow-Methods", "PUT")],
        false,
        "PUT",
        &[]
      )
      .is_err()
    );
    for status in [301, 404, 500] {
      assert!(
        check(
          status,
          &[
            ("Access-Control-Allow-Origin", "*"),
            ("Access-Control-Allow-Methods", "PUT"),
          ],
          false,
          "PUT",
          &[]
        )
        .is_err(),
        "{status}"
      );
    }

    // A method that is not safelisted has to be listed, or covered by `*`
    // when the request has no credentials.
    let allow = |methods: &'static str, include: bool| {
      let mut response = vec![
        ("Access-Control-Allow-Origin", origin),
        ("Access-Control-Allow-Credentials", "true"),
      ];
      if !methods.is_empty() {
        response.push(("Access-Control-Allow-Methods", methods));
      }
      check(200, &response, include, "PUT", &[]).is_ok()
    };
    assert!(allow("PUT", false));
    assert!(allow("PUT", true));
    assert!(!allow("", false));
    assert!(!allow("DELETE", false));
    assert!(!allow("put", false));
    assert!(allow("*", false));
    assert!(!allow("*", true));
    // A safelisted method needs no listing.
    assert!(
      check(
        200,
        &[
          ("Access-Control-Allow-Origin", "*"),
          ("Access-Control-Allow-Headers", "x-custom"),
        ],
        false,
        "POST",
        &custom
      )
      .is_ok()
    );

    // Header names: listed without regard to case, or `*` without
    // credentials.
    let allow_headers =
      |names: &'static str, include: bool, request: &[(&str, &str)]| {
        let mut response = vec![
          ("Access-Control-Allow-Origin", origin),
          ("Access-Control-Allow-Credentials", "true"),
        ];
        if !names.is_empty() {
          response.push(("Access-Control-Allow-Headers", names));
        }
        check(200, &response, include, "GET", request).is_ok()
      };
    assert!(allow_headers("x-custom", true, &custom));
    assert!(!allow_headers("", false, &custom));
    assert!(!allow_headers("x-other", false, &custom));
    assert!(allow_headers("*", false, &custom));
    assert!(!allow_headers("*", true, &custom));
    // `Authorization` is never covered by `*`.
    let authorization = [("Authorization", "Bearer t")];
    assert!(!allow_headers("*", false, &authorization));
    assert!(allow_headers("authorization", false, &authorization));
    assert!(allow_headers("*, Authorization", false, &authorization));
    // A list that is not one fails the preflight.
    assert!(!allow_headers("x custom", false, &custom));

    // Max-Age: 5 seconds by default, capped at two hours.
    let max_age = |values: &[&'static str]| {
      let mut response = vec![("Access-Control-Allow-Origin", "*")];
      for value in values {
        response.push(("Access-Control-Max-Age", *value));
      }
      check(200, &response, false, "GET", &[]).unwrap().max_age
    };
    assert_eq!(max_age(&[]), PREFLIGHT_MAX_AGE_DEFAULT);
    assert_eq!(max_age(&["60"]), Duration::from_secs(60));
    assert_eq!(max_age(&["0"]), Duration::ZERO);
    assert_eq!(max_age(&["86400"]), PREFLIGHT_MAX_AGE_LIMIT);
    assert_eq!(
      max_age(&["99999999999999999999999999"]),
      PREFLIGHT_MAX_AGE_LIMIT
    );
    assert_eq!(max_age(&["-1"]), PREFLIGHT_MAX_AGE_DEFAULT);
    assert_eq!(max_age(&["1.5"]), PREFLIGHT_MAX_AGE_DEFAULT);
    assert_eq!(max_age(&["60", "60"]), PREFLIGHT_MAX_AGE_DEFAULT);
  }

  #[test]
  fn preflight_cache_entries() {
    let origin = "https://app.example";
    let url = "https://api.example/items";
    let now = Instant::now();
    let later = |seconds: u64| now + Duration::from_secs(seconds);
    let custom = vec!["x-custom".to_string()];
    let grant = PreflightGrant {
      methods: vec!["PUT".to_string()],
      header_names: vec!["X-Custom".to_string()],
      max_age: Duration::from_secs(60),
    };

    let mut cache = PreflightCache::default();
    assert!(cache.needs_preflight(now, origin, url, false, "PUT", &[]));
    assert!(cache.needs_preflight(now, origin, url, false, "GET", &custom));
    // A simple request needs none, cache or not.
    assert!(!cache.needs_preflight(now, origin, url, false, "POST", &[]));

    cache.store(now, origin, url, false, &grant);
    assert_eq!(cache.len(), 2);
    assert!(!cache.needs_preflight(now, origin, url, false, "PUT", &custom));
    assert!(cache.method_match(now, origin, url, false, "PUT"));
    assert!(cache.header_name_match(now, origin, url, false, "X-CUSTOM"));
    // Another method, header name, URL or origin is not covered.
    assert!(cache.needs_preflight(now, origin, url, false, "DELETE", &[]));
    assert!(cache.needs_preflight(
      now,
      origin,
      url,
      false,
      "PUT",
      &["x-other".to_string()]
    ));
    assert!(cache.needs_preflight(
      now,
      origin,
      "https://api.example/other",
      false,
      "PUT",
      &[]
    ));
    assert!(cache.needs_preflight(
      now,
      "https://evil.example",
      url,
      false,
      "PUT",
      &[]
    ));
    assert!(cache.needs_preflight(now, "null", url, false, "PUT", &[]));
    // An entry made without credentials is not for a request with them.
    assert!(cache.needs_preflight(now, origin, url, true, "PUT", &[]));
    // It expires.
    assert!(!cache.needs_preflight(later(59), origin, url, false, "PUT", &[]));
    assert!(cache.needs_preflight(later(60), origin, url, false, "PUT", &[]));

    // An entry made with credentials serves both.
    cache.store(now, origin, url, true, &grant);
    assert!(!cache.needs_preflight(now, origin, url, true, "PUT", &custom));
    assert!(!cache.needs_preflight(now, origin, url, false, "PUT", &custom));

    // Storing again renews the entries instead of adding to them.
    let count = cache.len();
    cache.store(later(30), origin, url, true, &grant);
    assert_eq!(cache.len(), count);
    assert!(!cache.needs_preflight(later(80), origin, url, true, "PUT", &[]));

    // A lifetime beyond the limit is the limit.
    let forever = PreflightGrant {
      max_age: Duration::from_secs(1_000_000),
      ..grant.clone()
    };
    let other = "https://api.example/long";
    cache.store(now, origin, other, false, &forever);
    assert!(!cache.needs_preflight(
      later(7199),
      origin,
      other,
      false,
      "PUT",
      &[]
    ));
    assert!(cache.needs_preflight(
      later(7200),
      origin,
      other,
      false,
      "PUT",
      &[]
    ));

    // Clearing drops what is held for that origin and URL only.
    cache.clear(origin, url);
    assert!(cache.needs_preflight(now, origin, url, true, "PUT", &[]));
    assert!(!cache.needs_preflight(now, origin, other, false, "PUT", &[]));

    // Wildcards: any method; any header name but `Authorization`.
    let mut cache = PreflightCache::default();
    cache.store(
      now,
      origin,
      url,
      false,
      &PreflightGrant {
        methods: vec!["*".to_string()],
        header_names: vec!["*".to_string()],
        max_age: Duration::from_secs(60),
      },
    );
    assert!(!cache.needs_preflight(now, origin, url, false, "DELETE", &custom));
    assert!(cache.needs_preflight(
      now,
      origin,
      url,
      false,
      "DELETE",
      &["authorization".to_string()]
    ));

    // The cache is bounded.
    let mut cache = PreflightCache::default();
    for index in 0..(PREFLIGHT_CACHE_CAPACITY + 10) {
      cache.store(
        now,
        origin,
        &format!("https://api.example/{index}"),
        false,
        &grant,
      );
    }
    assert_eq!(cache.len(), PREFLIGHT_CACHE_CAPACITY);
    assert!(!cache.is_empty());
  }

  #[test]
  fn redirect_tainted_origins() {
    let origin = Url::parse("https://app.example/").unwrap().origin();
    let urls = |list: &[&str]| -> Vec<Url> {
      list.iter().map(|url| Url::parse(url).unwrap()).collect()
    };
    let serialized =
      |list: &[&str]| serialize_request_origin(&urls(list), &origin);
    assert_eq!(
      serialized(&["https://app.example/a"]),
      "https://app.example"
    );
    assert_eq!(serialized(&["https://b.example/a"]), "https://app.example");
    // Leaving the request's origin does not taint it.
    assert_eq!(
      serialized(&["https://app.example/a", "https://b.example/"]),
      "https://app.example"
    );
    // Crossing origins from another origin does: to a third origin...
    assert_eq!(
      serialized(&["https://b.example/", "https://c.example/"]),
      "null"
    );
    // ...and back to the request's own.
    assert_eq!(
      serialized(&["https://b.example/", "https://app.example/"]),
      "null"
    );
    // A redirect within the other origin does not.
    assert_eq!(
      serialized(&["https://b.example/1", "https://b.example/2"]),
      "https://app.example"
    );
    // A port is another origin.
    assert_eq!(
      serialized(&["https://b.example/", "https://b.example:8443/"]),
      "null"
    );
  }

  #[test]
  fn referrers() {
    let source =
      Url::parse("https://user:pw@app.example/dir/page.html?q=1#f").unwrap();
    let referrer = |policy: ReferrerPolicy, current: &str| {
      determine_referrer(&source, policy, &Url::parse(current).unwrap())
        .map(String::from)
    };
    let full = Some("https://app.example/dir/page.html?q=1".to_string());
    let origin = Some("https://app.example/".to_string());
    let same = "https://app.example/api";
    let cross = "https://api.example/";
    let downgrade = "http://api.example/";

    use ReferrerPolicy::*;
    assert_eq!(referrer(StrictOriginWhenCrossOrigin, same), full);
    assert_eq!(referrer(StrictOriginWhenCrossOrigin, cross), origin);
    assert_eq!(referrer(StrictOriginWhenCrossOrigin, downgrade), None);
    assert_eq!(referrer(NoReferrer, same), None);
    assert_eq!(referrer(Origin, same), origin);
    assert_eq!(referrer(Origin, downgrade), origin);
    assert_eq!(referrer(UnsafeUrl, downgrade), full);
    assert_eq!(referrer(StrictOrigin, same), origin);
    assert_eq!(referrer(StrictOrigin, downgrade), None);
    assert_eq!(referrer(SameOrigin, same), full);
    assert_eq!(referrer(SameOrigin, cross), None);
    assert_eq!(referrer(OriginWhenCrossOrigin, same), full);
    assert_eq!(referrer(OriginWhenCrossOrigin, downgrade), origin);
    assert_eq!(referrer(NoReferrerWhenDowngrade, cross), full);
    assert_eq!(referrer(NoReferrerWhenDowngrade, downgrade), None);
    // Loopback is as trustworthy as https: no downgrade.
    assert_eq!(
      referrer(StrictOriginWhenCrossOrigin, "http://127.0.0.1:8080/"),
      origin
    );
    assert_eq!(
      referrer(StrictOriginWhenCrossOrigin, "http://localhost/"),
      origin
    );

    // A local scheme is never a referrer, and a long URL is its origin.
    assert_eq!(
      determine_referrer(
        &Url::parse("data:text/html,x").unwrap(),
        UnsafeUrl,
        &Url::parse(same).unwrap()
      ),
      None
    );
    let long = Url::parse(&format!(
      "https://app.example/{}",
      "a".repeat(MAX_REFERRER_LENGTH)
    ))
    .unwrap();
    assert_eq!(
      determine_referrer(&long, UnsafeUrl, &Url::parse(same).unwrap())
        .map(String::from),
      origin
    );

    assert_eq!(
      ReferrerPolicy::from_response_headers(&headers(&[(
        "Referrer-Policy",
        "no-referrer, bogus, Same-Origin, nonsense"
      )])),
      Some(SameOrigin)
    );
    assert_eq!(
      ReferrerPolicy::from_response_headers(&headers(&[(
        "Referrer-Policy",
        "bogus"
      )])),
      None
    );
    assert_eq!(ReferrerPolicy::from_response_headers(&[]), None);
    for policy in [
      NoReferrer,
      NoReferrerWhenDowngrade,
      SameOrigin,
      Origin,
      StrictOrigin,
      OriginWhenCrossOrigin,
      StrictOriginWhenCrossOrigin,
      UnsafeUrl,
    ] {
      assert_eq!(ReferrerPolicy::parse(policy.as_str()), Some(policy));
    }
    assert_eq!(ReferrerPolicy::parse(""), None);
  }

  #[test]
  fn same_origin_requests_are_basic() {
    let web = page(PAGE);
    let get = request("GET", &["https://app.example/api?x#y"]);
    let prepared = prepare_request(
      &web,
      &get,
      &headers(&[
        ("Accept", "*/*"),
        ("Cookie", "stolen=1"),
        ("Host", "evil.example"),
        ("Origin", "https://evil.example"),
        ("Sec-Fetch-Site", "none"),
        ("X-Custom", "1"),
      ]),
    )
    .unwrap();
    assert_eq!(prepared.tainting, Tainting::Basic);
    assert!(!prepared.with_preflight);
    assert!(prepared.include_credentials);
    // No forbidden header, no `Origin` on a GET, the full referrer.
    assert_eq!(names(&prepared.headers), ["Accept", "X-Custom", "referer"]);
    assert_eq!(
      value_of(&prepared.headers, "referer").unwrap(),
      "https://app.example/dir/page.html?q=1"
    );
    assert_eq!(prepared.url.as_str(), "https://app.example/api?x#y");

    // A same-origin POST carries the origin.
    let post = request("POST", &["https://app.example/api"]);
    let prepared = prepare_request(&web, &post, &[]).unwrap();
    assert_eq!(prepared.tainting, Tainting::Basic);
    assert_eq!(
      value_of(&prepared.headers, "origin").unwrap(),
      "https://app.example"
    );

    // Forbidden methods never leave.
    for method in ["CONNECT", "TRACE", "TRACK"] {
      let request = request(method, &["https://app.example/api"]);
      assert!(prepare_request(&web, &request, &[]).is_err(), "{method}");
    }

    // A `data:` URL is basic in every mode.
    let mut data = request("GET", &["data:text/plain,hello"]);
    data.mode = Mode::SameOrigin;
    let prepared = prepare_request(&web, &data, &[]).unwrap();
    assert_eq!(prepared.tainting, Tainting::Basic);
    assert!(prepared.headers.is_empty());
  }

  #[test]
  fn cross_origin_requests_by_mode() {
    let web = page(PAGE);
    let url = "https://api.example/items";

    // cors: the origin, the referrer's origin, no credentials.
    let get = request("GET", &[url]);
    let prepared =
      prepare_request(&web, &get, &headers(&[("Accept", "*/*")])).unwrap();
    assert_eq!(prepared.tainting, Tainting::Cors);
    assert!(!prepared.with_preflight);
    assert!(!prepared.include_credentials);
    assert_eq!(
      value_of(&prepared.headers, "origin").unwrap(),
      "https://app.example"
    );
    assert_eq!(
      value_of(&prepared.headers, "referer").unwrap(),
      "https://app.example/"
    );

    // cors with credentials "include".
    let mut include = request("GET", &[url]);
    include.credentials = Credentials::Include;
    assert!(
      prepare_request(&web, &include, &[])
        .unwrap()
        .include_credentials
    );

    // A method or a header that is not safelisted asks for a preflight.
    let put = request("PUT", &[url]);
    let prepared = prepare_request(&web, &put, &[]).unwrap();
    assert!(prepared.with_preflight);
    assert!(prepared.unsafe_names.is_empty());
    let custom = prepare_request(
      &web,
      &get,
      &headers(&[
        ("X-Custom", "1"),
        ("Content-Type", "application/json"),
        ("Cookie", "a=b"),
      ]),
    )
    .unwrap();
    assert!(custom.with_preflight);
    assert_eq!(custom.unsafe_names, ["content-type", "x-custom"]);
    // Without the unsafe-request flag (EventSource) there is none.
    let mut event_source = request("GET", &[url]);
    event_source.unsafe_request = false;
    assert!(
      !prepare_request(
        &web,
        &event_source,
        &headers(&[("Last-Event-ID", "7")])
      )
      .unwrap()
      .with_preflight
    );

    // same-origin: a network error.
    let mut same_origin = request("GET", &[url]);
    same_origin.mode = Mode::SameOrigin;
    assert!(prepare_request(&web, &same_origin, &[]).is_err());

    // no-cors: opaque, only safelisted headers, and redirect "follow".
    let mut no_cors = request("POST", &[url]);
    no_cors.mode = Mode::NoCors;
    let prepared = prepare_request(
      &web,
      &no_cors,
      &headers(&[
        ("Accept", "*/*"),
        ("X-Custom", "1"),
        ("Content-Type", "application/json"),
        ("Authorization", "Bearer t"),
      ]),
    )
    .unwrap();
    assert_eq!(prepared.tainting, Tainting::Opaque);
    assert!(!prepared.with_preflight);
    assert_eq!(names(&prepared.headers), ["Accept", "referer", "origin"]);
    no_cors.redirect = Redirect::Manual;
    assert!(prepare_request(&web, &no_cors, &[]).is_err());

    // Only HTTP(S) crosses origins.
    for other in ["file:///etc/hosts", "about:blank", "ftp://api.example/x"] {
      let request = request("GET", &[other]);
      assert!(prepare_request(&web, &request, &[]).is_err(), "{other}");
    }

    // Another port or scheme is another origin.
    for other in ["https://app.example:8443/", "http://app.example/"] {
      let request = request("GET", &[other]);
      assert_eq!(
        prepare_request(&web, &request, &[]).unwrap().tainting,
        Tainting::Cors,
        "{other}"
      );
    }
  }

  #[test]
  fn redirect_hops() {
    let web = page(PAGE);

    // A cors request that came back to its own origin stays tainted, and
    // its origin is `null` from then on.
    let mut back = request(
      "GET",
      &[
        "https://app.example/start",
        "https://api.example/hop",
        "https://app.example/end",
      ],
    );
    back.tainting = Tainting::Cors;
    let prepared = prepare_request(&web, &back, &[]).unwrap();
    assert_eq!(prepared.tainting, Tainting::Cors);
    assert_eq!(prepared.origin, "null");
    assert_eq!(value_of(&prepared.headers, "origin").unwrap(), "null");

    // The first cross-origin hop still carries the real origin.
    let hop = request(
      "GET",
      &["https://app.example/start", "https://api.example/hop"],
    );
    let prepared = prepare_request(&web, &hop, &[]).unwrap();
    assert_eq!(prepared.tainting, Tainting::Cors);
    assert_eq!(prepared.origin, "https://app.example");

    // same-origin mode refuses the cross-origin hop.
    let mut same_origin = hop.clone();
    same_origin.mode = Mode::SameOrigin;
    assert!(prepare_request(&web, &same_origin, &[]).is_err());

    // Credentials in the URL of a hop are not sent.
    let with_credentials = request(
      "GET",
      &[
        "https://app.example/start",
        "https://user:pw@app.example/end",
      ],
    );
    let prepared = prepare_request(&web, &with_credentials, &[]).unwrap();
    assert_eq!(prepared.url.as_str(), "https://app.example/end");

    // The redirect itself: credentials in a cross-origin location, or in
    // any location once the request is tainted.
    let location = |url: &str| Url::parse(url).unwrap();
    let start = request("GET", &["https://app.example/start"]);
    assert!(
      check_redirect(&web, &start, &location("https://api.example/x")).is_ok()
    );
    assert!(
      check_redirect(&web, &start, &location("https://u:p@app.example/x"))
        .is_ok()
    );
    assert!(
      check_redirect(&web, &start, &location("https://u:p@api.example/x"))
        .is_err()
    );
    assert!(
      check_redirect(&web, &start, &location("https://u@api.example/x"))
        .is_err()
    );
    let mut tainted = hop.clone();
    tainted.tainting = Tainting::Cors;
    assert!(
      check_redirect(&web, &tainted, &location("https://u:p@app.example/x"))
        .is_err()
    );
    let mut no_cors = start.clone();
    no_cors.mode = Mode::NoCors;
    assert!(
      check_redirect(&web, &no_cors, &location("https://u:p@api.example/x"))
        .is_ok()
    );
  }

  #[test]
  fn opaque_page_origins_are_null() {
    for url in [
      "data:text/html,<p>x</p>",
      "about:blank",
      "file:///tmp/x.html",
    ] {
      let web = page(url);
      assert_eq!(web.serialized_origin(), "null", "{url}");
      // Nothing is same origin with it, not even its own URL's host.
      let get = request("GET", &["https://api.example/"]);
      let prepared = prepare_request(&web, &get, &[]).unwrap();
      assert_eq!(prepared.tainting, Tainting::Cors, "{url}");
      assert_eq!(value_of(&prepared.headers, "origin").unwrap(), "null");
      // And it has no referrer.
      assert_eq!(value_of(&prepared.headers, "referer"), None, "{url}");
      let mut same_origin = request("GET", &["https://api.example/"]);
      same_origin.mode = Mode::SameOrigin;
      assert!(prepare_request(&web, &same_origin, &[]).is_err(), "{url}");
    }
  }

  #[test]
  fn referrers_and_origins_of_requests() {
    let web = page(PAGE);
    let cross = "https://api.example/";

    // An explicit same-origin referrer is used; one of another origin is
    // the client's.
    let mut explicit = request("GET", &[cross]);
    explicit.referrer =
      Referrer::Url(Url::parse("https://app.example/other").unwrap());
    explicit.referrer_policy = Some(ReferrerPolicy::UnsafeUrl);
    assert_eq!(
      value_of(
        &prepare_request(&web, &explicit, &[]).unwrap().headers,
        "referer"
      )
      .unwrap(),
      "https://app.example/other"
    );
    explicit.referrer =
      Referrer::Url(Url::parse("https://spoofed.example/").unwrap());
    assert_eq!(
      value_of(
        &prepare_request(&web, &explicit, &[]).unwrap().headers,
        "referer"
      )
      .unwrap(),
      "https://app.example/dir/page.html?q=1"
    );
    explicit.referrer = Referrer::NoReferrer;
    assert_eq!(
      value_of(
        &prepare_request(&web, &explicit, &[]).unwrap().headers,
        "referer"
      ),
      None
    );

    // The origin of a no-cors POST follows the referrer policy.
    let mut post = request("POST", &[cross]);
    post.mode = Mode::NoCors;
    let origin_of = |request: &WebRequest| {
      value_of(
        &prepare_request(&web, request, &[]).unwrap().headers,
        "origin",
      )
    };
    assert_eq!(origin_of(&post).unwrap(), "https://app.example");
    post.referrer_policy = Some(ReferrerPolicy::NoReferrer);
    assert_eq!(origin_of(&post).unwrap(), "null");
    post.referrer_policy = Some(ReferrerPolicy::SameOrigin);
    assert_eq!(origin_of(&post).unwrap(), "null");
    post.referrer_policy = Some(ReferrerPolicy::UnsafeUrl);
    assert_eq!(origin_of(&post).unwrap(), "https://app.example");
    // A no-cors GET carries none.
    let mut get = request("GET", &[cross]);
    get.mode = Mode::NoCors;
    assert_eq!(origin_of(&get), None);
  }

  #[test]
  fn websocket_handshake_headers() {
    let web = page(PAGE);
    assert_eq!(
      web.websocket_headers(None),
      headers(&[("origin", "https://app.example")])
    );
    assert_eq!(
      web.websocket_headers(Some(headers(&[
        ("Origin", "https://evil.example"),
        ("Cookie", "a=b"),
        ("Host", "evil.example"),
        ("Sec-WebSocket-Key", "x"),
        ("X-Custom", "1"),
      ]))),
      headers(&[("X-Custom", "1"), ("origin", "https://app.example")])
    );
    assert_eq!(
      page("data:text/html,x").websocket_headers(None),
      headers(&[("origin", "null")])
    );
  }

  #[test]
  fn the_web_origin_is_read_from_the_op_state() {
    let mut state = OpState::new(None);
    assert!(EmbedderWebOrigin::from_state(&state).is_none());
    assert!(matches!(
      web_origin(&state),
      Err(WebOriginError::NotEnabled)
    ));
    state.put(page(PAGE));
    let web = EmbedderWebOrigin::from_state(&state).unwrap();
    assert_eq!(web.serialized_origin(), "https://app.example");
    assert!(web.origin().is_tuple());
  }
}
