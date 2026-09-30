// Copyright 2018-2026 the Deno authors. MIT license.

//! The stable page origin of a `deno desktop` app.
//!
//! A desktop app's renderer runs at `<scheme>://<host>` — a custom URL scheme
//! that the embedded webview hands to the runtime, which bridges every request
//! into the app's in-process `Deno.serve` (no TCP loopback, no random port).
//! Because the origin never changes between launches or machines it can be
//! allow-listed by a server that validates the browser `Origin` header, and
//! origin-keyed storage (`localStorage`, IndexedDB, cookies) stays put.
//!
//! The value comes from `desktop.app.origin` in `deno.json`. It is validated
//! and normalized at compile time (`deno desktop`), baked into the binary's
//! metadata, and read back by the desktop runtime, which also compares the
//! WebSocket relay's `Origin` header against it — so the parser lives here,
//! where both the CLI and `denort` can reach it.

use std::fmt;

/// The origin used when `desktop.app.origin` is not configured.
///
/// Stable by construction: `app` is a scheme no browser or platform claims,
/// and `localhost` is the host both Tauri (`tauri://localhost`) and Wails
/// (`wails://wails.localhost`) settle on for the same purpose. Storage is
/// scoped per app bundle by the webview's data store, so two apps sharing the
/// default do not see each other's data.
pub const DEFAULT_APP_ORIGIN: &str = "app://localhost";

/// Schemes that may not be used as a desktop app origin.
///
/// The WHATWG "special" schemes (`http`, `https`, `ws`, `wss`, `file`,
/// `ftp`) have URL-parser behavior a custom-scheme handler cannot emulate and
/// would let the app masquerade as a web origin; `blob`, `data`, `javascript`
/// and `about` are handled inside the engine and WebKit refuses to register a
/// handler for them (`+[WKWebView handlesURLScheme:]`); the rest are reserved
/// by WebKit or Chromium for their own internal pages.
pub const RESERVED_SCHEMES: &[&str] = &[
  // WHATWG special schemes.
  "http",
  "https",
  "ws",
  "wss",
  "file",
  "ftp",
  // Engine-handled schemes.
  "blob",
  "data",
  "javascript",
  "about",
  // Browser-internal schemes.
  "applewebdata",
  "chrome",
  "chrome-devtools",
  "chrome-extension",
  "chrome-untrusted",
  "devtools",
  "filesystem",
  "view-source",
  "webkit",
];

/// Why a `desktop.app.origin` value was rejected. The message is shown to the
/// user verbatim, prefixed with the offending value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AppOriginError {
  #[error("origin is empty")]
  Empty,
  #[error(
    "origin must be of the form <scheme>://<host> (e.g. \"myapp://app\")"
  )]
  MissingSeparator,
  #[error("scheme is empty")]
  EmptyScheme,
  #[error("scheme must start with an ASCII letter")]
  SchemeFirstChar,
  #[error(
    "scheme may only contain ASCII letters, digits, '+', '-' and '.' (RFC 3986)"
  )]
  SchemeChars,
  #[error("scheme {0:?} is reserved by browsers and cannot be an app origin")]
  ReservedScheme(String),
  #[error("host is empty")]
  EmptyHost,
  #[error("origin must not carry a port (found ':')")]
  HasPort,
  #[error("origin must not carry userinfo (found '@')")]
  HasUserinfo,
  #[error("origin must not carry a path, query or fragment")]
  HasPath,
  #[error(
    "host may only contain ASCII letters, digits, '-' and '.', with no empty label"
  )]
  HostChars,
  #[error("origin is longer than {} characters", MAX_LEN)]
  TooLong,
}

/// Path, relative to a directory of the app, of the file a desktop runtime
/// also reads the app origin and identifier from:
/// `{ "origin": "<scheme>://<host>", "identifier": "<reverse-DNS id>" }`.
///
/// This is how a `deno desktop` CLI that predates `desktop.app.origin` (and
/// therefore rejects that key in `deno.json`) can still configure the origin:
/// the file is embedded like any other asset — `"compile": { "include":
/// [".deno-desktop/app.json"] }` in `deno.json`, or `--include` — and the
/// runtime finds it in the embedded file system next to the entrypoint or in
/// any directory above it, up to the embedded root. A value baked into the
/// binary metadata (from `desktop.app.origin`) takes precedence.
///
/// `identifier` is the app's reverse-DNS id (`desktop.app.identifier`, see
/// [`super::app_id`]); the runtime hands it to the webview backend so web
/// storage lives in a per-app directory. A value baked into the binary
/// metadata takes precedence here too.
pub const APP_CONFIG_FILE: &str = ".deno-desktop/app.json";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SerializedAppConfigFile {
  origin: Option<String>,
  identifier: Option<String>,
}

/// The validated contents of an [`APP_CONFIG_FILE`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppConfigFile {
  pub origin: Option<AppOrigin>,
  pub identifier: Option<String>,
}

/// Parse the contents of an [`APP_CONFIG_FILE`]. Both keys are optional; an
/// error for malformed JSON, unknown keys (a typo must not silently fall back
/// to the default origin or to shared storage), an invalid origin or an
/// invalid identifier.
pub fn parse_app_config_file(bytes: &[u8]) -> Result<AppConfigFile, String> {
  let config: SerializedAppConfigFile =
    serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
  let origin = match config.origin {
    None => None,
    Some(origin) => Some(
      AppOrigin::parse(&origin)
        .map_err(|e| format!("invalid origin {origin:?}: {e}"))?,
    ),
  };
  if let Some(identifier) = &config.identifier {
    super::app_id::validate_app_identifier(identifier)
      .map_err(|e| format!("invalid identifier: {e}"))?;
  }
  Ok(AppConfigFile {
    origin,
    identifier: config.identifier,
  })
}

/// Upper bound on the serialized origin. Generous for anything a person would
/// type, small enough that it cannot be used to stuff the metadata or a header.
pub const MAX_LEN: usize = 255;

/// A validated, normalized desktop app origin: `<scheme>://<host>`.
///
/// Both parts are lower-cased. Schemes are case-insensitive by RFC 3986 and
/// the URL parser lower-cases them; the host is lower-cased here because a
/// non-special scheme's host is opaque to the URL parser (it is *not*
/// lower-cased by the browser), so the runtime must navigate to exactly the
/// string it later expects in the `Origin` header.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AppOrigin {
  scheme: String,
  host: String,
}

impl AppOrigin {
  /// Parse and validate a `desktop.app.origin` value. Accepts
  /// `scheme://host` with an optional single trailing `/` (what
  /// `new URL(origin).href` prints), and nothing else.
  pub fn parse(input: &str) -> Result<Self, AppOriginError> {
    let input = input.trim();
    if input.is_empty() {
      return Err(AppOriginError::Empty);
    }
    if input.len() > MAX_LEN {
      return Err(AppOriginError::TooLong);
    }
    let (scheme, rest) = input
      .split_once("://")
      .ok_or(AppOriginError::MissingSeparator)?;
    validate_scheme(scheme)?;
    // A lone trailing slash is the URL serializer's doing, not a path.
    let host = rest.strip_suffix('/').unwrap_or(rest);
    validate_host(host)?;
    Ok(Self {
      scheme: scheme.to_ascii_lowercase(),
      host: host.to_ascii_lowercase(),
    })
  }

  /// [`DEFAULT_APP_ORIGIN`], parsed.
  pub fn default_origin() -> Self {
    Self::parse(DEFAULT_APP_ORIGIN)
      .expect("DEFAULT_APP_ORIGIN is a valid origin")
  }

  /// The URL scheme (`myapp` in `myapp://app`). This is what gets registered
  /// with the webview as a custom scheme handler.
  pub fn scheme(&self) -> &str {
    &self.scheme
  }

  /// The host (`app` in `myapp://app`). What `location.host` reports.
  pub fn host(&self) -> &str {
    &self.host
  }

  /// The serialized origin, `scheme://host`, exactly as a browser serializes
  /// it into `location.origin` and the `Origin` request header.
  pub fn as_origin_string(&self) -> String {
    format!("{}://{}", self.scheme, self.host)
  }

  /// The URL the webview navigates to on startup: the origin plus `/`.
  pub fn root_url(&self) -> String {
    format!("{}://{}/", self.scheme, self.host)
  }

  /// Whether an `Origin` request header value names exactly this origin.
  ///
  /// The comparison is byte-exact against the normalized serialization: a
  /// browser emits the origin it navigated to, which is the lower-cased form
  /// the runtime produced from this value. Anything else — `null`, a foreign
  /// origin, a same-scheme different host, trailing garbage — is a mismatch.
  pub fn matches_origin_header(&self, header_value: &str) -> bool {
    let value = header_value.trim_matches([' ', '\t']);
    let Some(rest) = value.strip_prefix(self.scheme.as_str()) else {
      return false;
    };
    let Some(host) = rest.strip_prefix("://") else {
      return false;
    };
    host == self.host
  }
}

impl fmt::Display for AppOrigin {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}://{}", self.scheme, self.host)
  }
}

/// RFC 3986 `scheme = ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`, minus the
/// schemes in [`RESERVED_SCHEMES`].
fn validate_scheme(scheme: &str) -> Result<(), AppOriginError> {
  let mut chars = scheme.chars();
  match chars.next() {
    None => return Err(AppOriginError::EmptyScheme),
    Some(c) if !c.is_ascii_alphabetic() => {
      return Err(AppOriginError::SchemeFirstChar);
    }
    Some(_) => {}
  }
  if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
    return Err(AppOriginError::SchemeChars);
  }
  let lower = scheme.to_ascii_lowercase();
  if RESERVED_SCHEMES.contains(&lower.as_str()) {
    return Err(AppOriginError::ReservedScheme(lower));
  }
  Ok(())
}

/// A bare host: DNS-style labels of ASCII letters, digits and `-`, joined by
/// `.`. No port, userinfo, path, query or fragment — an origin has none of
/// those, and a value carrying one would silently differ from what the
/// browser reports as `location.origin`.
fn validate_host(host: &str) -> Result<(), AppOriginError> {
  if host.is_empty() {
    return Err(AppOriginError::EmptyHost);
  }
  if host.contains(['/', '?', '#']) {
    return Err(AppOriginError::HasPath);
  }
  if host.contains('@') {
    return Err(AppOriginError::HasUserinfo);
  }
  if host.contains(':') {
    return Err(AppOriginError::HasPort);
  }
  let labels_ok = host.split('.').all(|label| {
    !label.is_empty()
      && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
  });
  if !labels_ok {
    return Err(AppOriginError::HostChars);
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn default_origin_is_valid_and_stable() {
    let origin = AppOrigin::default_origin();
    assert_eq!(origin.scheme(), "app");
    assert_eq!(origin.host(), "localhost");
    assert_eq!(origin.as_origin_string(), DEFAULT_APP_ORIGIN);
    assert_eq!(origin.root_url(), "app://localhost/");
    assert_eq!(origin.to_string(), DEFAULT_APP_ORIGIN);
  }

  #[test]
  fn parses_scheme_and_host() {
    let origin = AppOrigin::parse("t3code://app").unwrap();
    assert_eq!(origin.scheme(), "t3code");
    assert_eq!(origin.host(), "app");
    assert_eq!(origin.as_origin_string(), "t3code://app");
    assert_eq!(origin.root_url(), "t3code://app/");
  }

  #[test]
  fn accepts_the_url_serializer_form() {
    // `new URL("t3code://app").href` is "t3code://app/" — a lone trailing
    // slash is not a path.
    assert_eq!(
      AppOrigin::parse("t3code://app/").unwrap(),
      AppOrigin::parse("t3code://app").unwrap()
    );
    // Surrounding whitespace is tolerated (a deno.json value), interior is not.
    assert!(AppOrigin::parse("  t3code://app \n").is_ok());
    assert_eq!(
      AppOrigin::parse("t3code://my app"),
      Err(AppOriginError::HostChars)
    );
  }

  #[test]
  fn normalizes_to_lowercase() {
    let origin = AppOrigin::parse("T3Code://App.Local").unwrap();
    assert_eq!(origin.as_origin_string(), "t3code://app.local");
  }

  #[test]
  fn scheme_follows_rfc_3986() {
    assert!(AppOrigin::parse("my-app+v2.0://app").is_ok());
    assert_eq!(
      AppOrigin::parse("1app://app"),
      Err(AppOriginError::SchemeFirstChar)
    );
    assert_eq!(
      AppOrigin::parse("-app://app"),
      Err(AppOriginError::SchemeFirstChar)
    );
    assert_eq!(
      AppOrigin::parse("my_app://app"),
      Err(AppOriginError::SchemeChars)
    );
    assert_eq!(
      AppOrigin::parse("my app://app"),
      Err(AppOriginError::SchemeChars)
    );
    assert_eq!(AppOrigin::parse("://app"), Err(AppOriginError::EmptyScheme));
  }

  #[test]
  fn rejects_reserved_schemes_case_insensitively() {
    for scheme in RESERVED_SCHEMES {
      let err = AppOrigin::parse(&format!("{scheme}://app")).unwrap_err();
      assert_eq!(
        err,
        AppOriginError::ReservedScheme((*scheme).to_string()),
        "{scheme} must be reserved"
      );
    }
    assert_eq!(
      AppOrigin::parse("HTTPS://app"),
      Err(AppOriginError::ReservedScheme("https".into()))
    );
    assert_eq!(
      AppOrigin::parse("File://app"),
      Err(AppOriginError::ReservedScheme("file".into()))
    );
  }

  #[test]
  fn requires_the_scheme_host_shape() {
    assert_eq!(AppOrigin::parse(""), Err(AppOriginError::Empty));
    assert_eq!(AppOrigin::parse("   "), Err(AppOriginError::Empty));
    assert_eq!(
      AppOrigin::parse("t3code"),
      Err(AppOriginError::MissingSeparator)
    );
    assert_eq!(
      AppOrigin::parse("t3code:app"),
      Err(AppOriginError::MissingSeparator)
    );
    assert_eq!(
      AppOrigin::parse("t3code://"),
      Err(AppOriginError::EmptyHost)
    );
    assert_eq!(
      AppOrigin::parse("t3code:///"),
      Err(AppOriginError::EmptyHost)
    );
  }

  #[test]
  fn rejects_authority_and_path_extras() {
    assert_eq!(
      AppOrigin::parse("t3code://app:8080"),
      Err(AppOriginError::HasPort)
    );
    assert_eq!(
      AppOrigin::parse("t3code://user@app"),
      Err(AppOriginError::HasUserinfo)
    );
    assert_eq!(
      AppOrigin::parse("t3code://app/index.html"),
      Err(AppOriginError::HasPath)
    );
    assert_eq!(
      AppOrigin::parse("t3code://app//"),
      Err(AppOriginError::HasPath)
    );
    assert_eq!(
      AppOrigin::parse("t3code://app?x=1"),
      Err(AppOriginError::HasPath)
    );
    assert_eq!(
      AppOrigin::parse("t3code://app#top"),
      Err(AppOriginError::HasPath)
    );
    // IPv6 literals carry ':' and brackets — not a valid origin host here.
    assert_eq!(
      AppOrigin::parse("t3code://[::1]"),
      Err(AppOriginError::HasPort)
    );
  }

  #[test]
  fn host_labels_are_dns_like() {
    assert!(AppOrigin::parse("t3code://app.example-1.local").is_ok());
    assert_eq!(
      AppOrigin::parse("t3code://.app"),
      Err(AppOriginError::HostChars)
    );
    assert_eq!(
      AppOrigin::parse("t3code://app."),
      Err(AppOriginError::HostChars)
    );
    assert_eq!(
      AppOrigin::parse("t3code://a..b"),
      Err(AppOriginError::HostChars)
    );
    assert_eq!(
      AppOrigin::parse("t3code://app_1"),
      Err(AppOriginError::HostChars)
    );
    assert_eq!(
      AppOrigin::parse("t3code://äpp"),
      Err(AppOriginError::HostChars)
    );
  }

  #[test]
  fn rejects_overlong_values() {
    let long = format!("t3code://{}", "a".repeat(MAX_LEN));
    assert_eq!(AppOrigin::parse(&long), Err(AppOriginError::TooLong));
  }

  #[test]
  fn app_config_file() {
    assert_eq!(
      parse_app_config_file(br#"{ "origin": "T3Code://App/" }"#)
        .unwrap()
        .origin,
      Some(AppOrigin::parse("t3code://app").unwrap())
    );
    assert_eq!(
      parse_app_config_file(b"{}").unwrap(),
      AppConfigFile::default()
    );
    assert!(
      parse_app_config_file(br#"{ "origin": "https://app" }"#)
        .unwrap_err()
        .contains("reserved")
    );
    // A typo is an error, not a silent fallback to the default origin.
    assert!(
      parse_app_config_file(br#"{ "orgin": "t3code://app" }"#)
        .unwrap_err()
        .contains("orgin")
    );
    assert!(parse_app_config_file(b"not json").is_err());
    assert!(parse_app_config_file(br#"{ "origin": 1 }"#).is_err());
  }

  #[test]
  fn app_config_file_identifier() {
    assert_eq!(
      parse_app_config_file(
        br#"{ "origin": "t3code://app", "identifier": "com.t3.code" }"#
      )
      .unwrap(),
      AppConfigFile {
        origin: Some(AppOrigin::parse("t3code://app").unwrap()),
        identifier: Some("com.t3.code".to_string()),
      }
    );
    // Either key may appear alone; the runtime decides whether the
    // combination is acceptable.
    assert_eq!(
      parse_app_config_file(br#"{ "identifier": "com.t3.code" }"#)
        .unwrap()
        .identifier
        .as_deref(),
      Some("com.t3.code")
    );
    // Strictly validated: the id becomes a directory name.
    for bad in [
      r#"{ "identifier": "" }"#,
      r#"{ "identifier": "notes" }"#,
      r#"{ "identifier": "com.acme/../evil" }"#,
      r#"{ "identifier": "com.acme app" }"#,
      r#"{ "identifier": 1 }"#,
    ] {
      assert!(parse_app_config_file(bad.as_bytes()).is_err(), "{bad}");
    }
    assert!(
      parse_app_config_file(br#"{ "identifer": "com.t3.code" }"#)
        .unwrap_err()
        .contains("identifer")
    );
  }

  #[test]
  fn origin_header_match_is_exact() {
    let origin = AppOrigin::parse("t3code://app").unwrap();
    assert!(origin.matches_origin_header("t3code://app"));
    // Header values may carry surrounding whitespace after a lenient split.
    assert!(origin.matches_origin_header(" t3code://app\t"));

    assert!(!origin.matches_origin_header("null"));
    assert!(!origin.matches_origin_header(""));
    assert!(!origin.matches_origin_header("https://app"));
    assert!(!origin.matches_origin_header("t3code://evil"));
    assert!(!origin.matches_origin_header("t3code://app.evil"));
    assert!(!origin.matches_origin_header("t3code://appx"));
    assert!(!origin.matches_origin_header("t3code://app/"));
    assert!(!origin.matches_origin_header("t3code://app:80"));
    assert!(!origin.matches_origin_header("xt3code://app"));
    // A browser never upper-cases what it navigated to; a spoofer might.
    assert!(!origin.matches_origin_header("T3CODE://APP"));
  }
}
