// Copyright 2018-2026 the Deno authors. MIT license.

//! The identifier of a `deno desktop` app.
//!
//! One reverse-DNS id (`desktop.app.identifier` in `deno.json`, e.g.
//! `com.acme.notes`) names the app everywhere: the macOS `CFBundleIdentifier`,
//! the Linux `.desktop` file and window app_id, and the per-app web data
//! directory the laufey backend keeps web storage in. The backend learns it
//! from the [`LAUFEY_APP_ID_ENV`] environment variable and resolves the
//! directory from it (`~/Library/Application Support/<id>`,
//! `$XDG_DATA_HOME/<id>`, `%LOCALAPPDATA%\<id>`), so `localStorage`,
//! IndexedDB and cookies persist across launches and are never shared with
//! another app.
//!
//! The rules live here, where both the CLI (which bakes the id into the
//! bundle and the launch environment) and `denort` (which reads it back from
//! the binary metadata or an embedded `.deno-desktop/app.json`) can reach
//! them.

/// Environment variable through which a laufey backend learns the app id.
/// CEF reads it when the backend process starts, before the runtime library
/// is loaded; the WebView backends read it when the first window is created.
pub const LAUFEY_APP_ID_ENV: &str = "LAUFEY_APP_ID";

/// Longest accepted identifier: Apple's documented limit for
/// `CFBundleIdentifier` on receipts (bigger values quietly truncate elsewhere
/// in the toolchain).
pub const MAX_LEN: usize = 155;

/// Why an app identifier was rejected. The messages match the ones the CLI
/// has always printed for an invalid bundle identifier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AppIdentifierError {
  #[error("bundle identifier is empty")]
  Empty,
  #[error("bundle identifier {0:?} is longer than 155 characters")]
  TooLong(String),
  #[error(
    "bundle identifier {0:?} must be in reverse-DNS form (e.g. com.acme.foo)"
  )]
  NotReverseDns(String),
  #[error(
    "bundle identifier {0:?} must match [A-Za-z0-9.-]+, but contains {1:?}"
  )]
  InvalidChar(String, char),
  #[error("bundle identifier {0:?} has an empty segment")]
  EmptySegment(String),
}

/// Validate a reverse-DNS app identifier (Apple `CFBundleIdentifier`, also
/// used for Linux `.desktop` filenames, the Windows app id and the laufey
/// web data directory).
///
/// Apple's rules: ASCII alphanumerics, hyphens, and dots; must have at least
/// one dot (so it looks like reverse DNS); each dot-separated segment must be
/// non-empty. The segment-leading-letter rule is not enforced (some legacy
/// apps use digits). An id that passes also passes [`is_laufey_app_id`].
pub fn validate_app_identifier(id: &str) -> Result<(), AppIdentifierError> {
  if id.is_empty() {
    return Err(AppIdentifierError::Empty);
  }
  if id.len() > MAX_LEN {
    return Err(AppIdentifierError::TooLong(id.to_string()));
  }
  if !id.contains('.') {
    return Err(AppIdentifierError::NotReverseDns(id.to_string()));
  }
  if let Some(c) = id
    .chars()
    .find(|c| !(c.is_ascii_alphanumeric() || *c == '.' || *c == '-'))
  {
    return Err(AppIdentifierError::InvalidChar(id.to_string(), c));
  }
  if id.split('.').any(|seg| seg.is_empty()) {
    return Err(AppIdentifierError::EmptySegment(id.to_string()));
  }
  debug_assert!(is_laufey_app_id(id));
  Ok(())
}

/// Whether a laufey backend accepts `id` as [`LAUFEY_APP_ID_ENV`]: it becomes
/// a single path component, so it must be non-empty, not `.` or `..`, and use
/// only `A-Z a-z 0-9 . _ -` (laufey `IsSafeAppId`). The backend ignores any
/// other value with a warning and falls back to its non-persistent default,
/// so callers check this first and fail loudly instead.
pub fn is_laufey_app_id(id: &str) -> bool {
  !id.is_empty()
    && id != "."
    && id != ".."
    && id
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn accepts_reverse_dns_ids() {
    for id in ["com.acme.notes", "dev.t3.code", "com.deno.desktop.my-app"] {
      assert_eq!(validate_app_identifier(id), Ok(()), "{id}");
      assert!(is_laufey_app_id(id), "{id}");
    }
    // Digit-leading segments are tolerated (legacy apps use them).
    assert_eq!(validate_app_identifier("com.3m.app"), Ok(()));
    let max = format!("com.{}", "a".repeat(MAX_LEN - 4));
    assert_eq!(validate_app_identifier(&max), Ok(()));
  }

  #[test]
  fn rejects_invalid_ids() {
    assert_eq!(validate_app_identifier(""), Err(AppIdentifierError::Empty));
    assert!(matches!(
      validate_app_identifier("notes"),
      Err(AppIdentifierError::NotReverseDns(_))
    ));
    assert!(matches!(
      validate_app_identifier(&format!("com.{}", "a".repeat(MAX_LEN))),
      Err(AppIdentifierError::TooLong(_))
    ));
    for (id, c) in [
      ("com.acme app", ' '),
      ("com.acme_app", '_'),
      ("com.acme/app", '/'),
      ("com.acme\\app", '\\'),
      ("com.acme\napp", '\n'),
      ("com.acmé", 'é'),
    ] {
      assert_eq!(
        validate_app_identifier(id),
        Err(AppIdentifierError::InvalidChar(id.to_string(), c)),
        "{id:?}"
      );
    }
    for id in [".com.acme", "com..acme", "com.acme.", "."] {
      assert!(
        matches!(
          validate_app_identifier(id),
          Err(AppIdentifierError::EmptySegment(_))
        ),
        "{id:?}"
      );
    }
  }

  #[test]
  fn laufey_rule_matches_the_backend() {
    // laufey `IsSafeAppId`: one path component of `A-Z a-z 0-9 . _ -`.
    assert!(is_laufey_app_id("com.deno.desktop.my_app"));
    assert!(is_laufey_app_id("notes"));
    assert!(is_laufey_app_id("..."));
    for id in ["", ".", "..", "a/b", "a\\b", "a b", "a:b", "é", "a\0b"] {
      assert!(!is_laufey_app_id(id), "{id:?}");
    }
  }
}
