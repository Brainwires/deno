// Copyright 2018-2026 the Deno authors. MIT license.

//! The typed refusal every full-app update step returns.
//!
//! The code is stable API: it reaches JavaScript as the `code` property of
//! the thrown error (`"<code>: <message>"` across the op boundary, split back
//! apart by the `Deno.desktop.updater` JS), and denext's
//! `denext/desktop/updater` re-exports it as `AppUpdateError.code`.

use std::fmt;

/// Why an update step refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateErrorCode {
  /// The app has no update public key, identifier or version baked in, or
  /// the runtime cannot tell which platform it is.
  NotConfigured,
  /// The manifest is not the signed envelope, or its signed payload is
  /// malformed (bad JSON, unknown keys, bad semver, bad hash, ...).
  InvalidManifest,
  /// The signature is missing, malformed, or does not verify against the
  /// baked public key.
  Signature,
  /// The manifest is for another app (`app` differs from the identifier).
  WrongApp,
  /// The offered version is not newer than the running one.
  Downgrade,
  /// The offered version was installed before, failed to start, and was
  /// rolled back.
  Rejected,
  /// The manifest has no entry for this platform.
  NoPlatform,
  /// The archive URL is not https (loopback http needs the dev flag).
  InsecureUrl,
  /// The download grew past the size the manifest declares.
  SizeExceeded,
  /// The download's size or SHA-256 does not match the manifest.
  Integrity,
  /// The archive holds an entry the safe extractor refuses.
  UnsafeArchive,
  /// The staged app is not the same app (another bundle id / executable).
  BundleMismatch,
  /// The OS code-signature check of the staged app failed, or the running
  /// app is unsigned and the dev opt-out was not given.
  OsSignature,
  /// The install location (or its parent) is not writable by this user.
  InstallNotWritable,
  /// The app runs from a location it cannot replace (App Translocation, a
  /// self-extracting launcher's cache, a read-only mount, ...).
  UnsupportedLayout,
  /// Nothing (or something else) is staged.
  NotStaged,
  /// Another update step is in progress.
  Busy,
  /// A file-system or process error.
  Io,
}

impl UpdateErrorCode {
  /// The wire name of the code.
  pub fn as_str(self) -> &'static str {
    match self {
      Self::NotConfigured => "not_configured",
      Self::InvalidManifest => "invalid_manifest",
      Self::Signature => "signature",
      Self::WrongApp => "wrong_app",
      Self::Downgrade => "downgrade",
      Self::Rejected => "rejected",
      Self::NoPlatform => "no_platform",
      Self::InsecureUrl => "insecure_url",
      Self::SizeExceeded => "size_exceeded",
      Self::Integrity => "integrity",
      Self::UnsafeArchive => "unsafe_archive",
      Self::BundleMismatch => "bundle_mismatch",
      Self::OsSignature => "os_signature",
      Self::InstallNotWritable => "install_not_writable",
      Self::UnsupportedLayout => "unsupported_layout",
      Self::NotStaged => "not_staged",
      Self::Busy => "busy",
      Self::Io => "io",
    }
  }
}

/// A refused update step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateError {
  pub code: UpdateErrorCode,
  pub message: String,
}

impl UpdateError {
  pub fn new(code: UpdateErrorCode, message: impl Into<String>) -> Self {
    Self {
      code,
      message: message.into(),
    }
  }

  /// An [`UpdateErrorCode::Io`] error for `what`.
  pub fn io(what: impl fmt::Display, err: impl fmt::Display) -> Self {
    Self::new(UpdateErrorCode::Io, format!("{what}: {err}"))
  }
}

impl fmt::Display for UpdateError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}: {}", self.code.as_str(), self.message)
  }
}

impl std::error::Error for UpdateError {}

/// Shorthand for building an error.
pub fn err<T>(
  code: UpdateErrorCode,
  message: impl Into<String>,
) -> Result<T, UpdateError> {
  Err(UpdateError::new(code, message))
}
