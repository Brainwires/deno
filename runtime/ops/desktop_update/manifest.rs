// Copyright 2018-2026 the Deno authors. MIT license.

//! The signed full-app update manifest.
//!
//! The file the update host serves is a JSON envelope:
//!
//! ```json
//! { "signed": "<the manifest, as a JSON string>", "signature": "<base64>" }
//! ```
//!
//! `signature` is ECDSA P-256 / SHA-256 (IEEE P1363 `r || s`, 64 bytes,
//! standard padded base64: exactly what WebCrypto's `sign` returns) over the
//! bytes `"denext-app-update-v1\n" + signed`. The key is the same key pair
//! denext's over-the-air UI updates use (`denext ota keygen`; the public half
//! is base64 SPKI or a PUBLIC KEY PEM), so an app has one signing story. The
//! domain prefix keeps a signature over an OTA UI payload (which starts
//! `"denext-ota-v"`) from ever verifying as an app-update manifest.
//!
//! The signed string is verified BYTE FOR BYTE and only then parsed, so there
//! is no JSON canonicalization for a signer and a verifier to disagree on.
//! Its payload:
//!
//! ```json
//! {
//!   "schema": 1,
//!   "app": "com.example.app",
//!   "version": "2.0.0",
//!   "minVersion": "1.5.0",
//!   "platforms": {
//!     "aarch64-apple-darwin-webview": {
//!       "url": "https://updates.example.com/app-2.0.0-aarch64-apple-darwin-webview.tar.gz",
//!       "sha256": "<64 lowercase hex>",
//!       "size": 123456789,
//!       "kind": "bundle"
//!     }
//!   },
//!   "releaseNotes": "...",
//!   "publishedAt": "2026-10-01T00:00:00Z",
//!   "expiresAt": "2026-10-15T00:00:00Z",
//!   "sequence": 1790812800
//! }
//! ```
//!
//! Unknown keys are refused (a typo must not silently drop a constraint).
//!
//! **Replay and freeze.** A replayed OLDER manifest can never install
//! anything older than what runs (the downgrade guard, and rejected versions
//! are never offered again); what it can do is hide a newer release from a
//! client whose update host an attacker controls (a freeze). Two optional,
//! signed fields bound that:
//!
//! - `expiresAt` (an RFC 3339 timestamp, e.g. `Date.prototype.toISOString`
//!   output): a manifest is refused (`expired`) once the clock passes it, so
//!   a publisher that re-signs on a schedule (TUF's timestamp role) caps how
//!   long an old manifest can be served.
//! - `sequence` (a non-negative integer up to 2^53 - 1 that only grows, e.g.
//!   the Unix time in seconds at signing): the highest one accepted is kept
//!   in the install's update state, and a lower one is refused (`replayed`),
//!   as is a manifest WITHOUT a sequence once a sequenced one was accepted
//!   (otherwise replaying any pre-sequence manifest would undo it). An equal
//!   sequence is the same release checked again.
//!
//! Both are optional: a manifest without them verifies as before (what
//! denext's publisher wrote before they existed), unless this install has
//! already accepted a sequenced one. Runtimes before these fields refuse a
//! manifest that carries them (unknown keys), so a publisher adds them only
//! for apps on a runtime that reads them.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use base64::Engine;
use p256::ecdsa::Signature;
use p256::ecdsa::VerifyingKey;
use p256::ecdsa::signature::Verifier;
use p256::pkcs8::DecodePublicKey;
use serde::Deserialize;
use serde::Serialize;

use super::error::UpdateError;
use super::error::UpdateErrorCode as Code;
use super::error::err;

/// What the signature covers before the signed string.
pub const SIGNATURE_DOMAIN: &[u8] = b"denext-app-update-v1\n";
/// The only payload schema this runtime understands.
pub const SCHEMA: u32 = 1;
/// The largest envelope accepted (the manifest is metadata, not payload).
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
/// The largest archive a manifest may declare.
pub const MAX_ARCHIVE_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// The longest `releaseNotes`, in bytes.
pub const MAX_RELEASE_NOTES_BYTES: usize = 64 * 1024;
/// The only `kind` there is: a whole signed app bundle (no deltas).
pub const KIND_BUNDLE: &str = "bundle";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
  signed: String,
  signature: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SignedManifest {
  schema: u32,
  app: String,
  version: String,
  #[serde(default)]
  min_version: Option<String>,
  platforms: BTreeMap<String, PlatformEntry>,
  #[serde(default)]
  release_notes: Option<String>,
  published_at: String,
  /// RFC 3339; the manifest is refused after it.
  #[serde(default)]
  expires_at: Option<String>,
  /// Monotonic release counter (see the module docs).
  #[serde(default)]
  sequence: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlatformEntry {
  url: String,
  sha256: String,
  size: u64,
  kind: String,
}

/// What the running app expects of a manifest.
#[derive(Debug, Clone)]
pub struct Expectations<'a> {
  /// The baked public key (base64 SPKI or PEM).
  pub public_key: &'a str,
  /// This app's identifier (`desktop.app.identifier`).
  pub app_id: &'a str,
  /// The running app's version (deno.json `version`).
  pub running_version: &'a str,
  /// This build's platform key, `<target>-<backend>`.
  pub platform: &'a str,
  /// Versions that were installed, failed to start and were rolled back.
  pub rejected_versions: &'a [String],
  /// The highest manifest `sequence` this install has accepted.
  pub sequence_floor: Option<u64>,
  /// The current time, Unix seconds (for `expiresAt`).
  pub now_unix: i64,
  /// Accept `http://` on a loopback host (dev only).
  pub allow_insecure_loopback: bool,
}

/// A verified update for this platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedUpdate {
  pub version: String,
  pub min_version: Option<String>,
  /// The running version is below `minVersion`: the publisher marks it as
  /// one users must leave (the app should not let them decline).
  pub required: bool,
  pub platform: String,
  pub url: String,
  pub sha256: String,
  pub size: u64,
  pub release_notes: Option<String>,
  pub published_at: String,
  /// The manifest's `sequence`, recorded as the install's high-water mark.
  pub sequence: Option<u64>,
  pub expires_at: Option<String>,
}

/// The outcome of checking a manifest that verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestVerdict {
  /// A newer version for this platform.
  Available(VerifiedUpdate),
  /// The manifest offers the running version: nothing to do.
  UpToDate {
    version: String,
    /// The manifest's `sequence` (still a high-water mark to record).
    sequence: Option<u64>,
  },
}

/// The largest `sequence` accepted: JavaScript's `Number.MAX_SAFE_INTEGER`,
/// so a publisher's number and the runtime's agree exactly.
pub const MAX_SEQUENCE: u64 = (1 << 53) - 1;

/// Parse the baked public key: standard base64 SPKI (what `denext ota
/// keygen` writes to `<out>.pub`) or a `-----BEGIN PUBLIC KEY-----` PEM of an
/// ECDSA P-256 key.
pub fn parse_public_key(text: &str) -> Result<VerifyingKey, UpdateError> {
  let text = text.trim();
  let der = if let Some(body) = pem_body(text, "PUBLIC KEY") {
    decode_b64(&body)
  } else {
    decode_b64(text)
  };
  let Some(der) = der else {
    return err(
      Code::NotConfigured,
      "the update public key is not base64 SPKI or a PUBLIC KEY PEM",
    );
  };
  VerifyingKey::from_public_key_der(&der).map_err(|_| {
    UpdateError::new(
      Code::NotConfigured,
      "the update public key is not an ECDSA P-256 public key",
    )
  })
}

fn pem_body(text: &str, label: &str) -> Option<String> {
  let begin = format!("-----BEGIN {label}-----");
  let end = format!("-----END {label}-----");
  let start = text.find(&begin)? + begin.len();
  let stop = start + text[start..].find(&end)?;
  Some(
    text[start..stop]
      .chars()
      .filter(|c| !c.is_whitespace())
      .collect(),
  )
}

fn decode_b64(text: &str) -> Option<Vec<u8>> {
  base64::engine::general_purpose::STANDARD.decode(text).ok()
}

/// Verify `envelope` (the raw bytes the update host served) and check it
/// against `exp`. Every check runs before anything is trusted: size, envelope
/// shape, the signature over the exact signed bytes, then — parsing only the
/// verified bytes — schema, app, `expiresAt` and `sequence`, versions, the
/// rejected versions, the platform entry, its hash, size, kind and URL.
pub fn verify_manifest(
  envelope: &[u8],
  exp: &Expectations,
) -> Result<ManifestVerdict, UpdateError> {
  if envelope.len() > MAX_MANIFEST_BYTES {
    return err(
      Code::InvalidManifest,
      format!("the manifest is larger than {MAX_MANIFEST_BYTES} bytes"),
    );
  }
  let key = parse_public_key(exp.public_key)?;
  let envelope: Envelope = serde_json::from_slice(envelope).map_err(|e| {
    UpdateError::new(
      Code::InvalidManifest,
      format!(
        "the manifest is not a {{\"signed\", \"signature\"}} envelope: {e}"
      ),
    )
  })?;

  // The signature, over the domain prefix + the exact signed bytes.
  let Some(raw_sig) = decode_b64(envelope.signature.trim()) else {
    return err(Code::Signature, "the signature is not standard base64");
  };
  let Ok(signature) = Signature::from_slice(&raw_sig) else {
    return err(
      Code::Signature,
      "the signature is not a 64-byte P-256 r||s signature",
    );
  };
  let mut message =
    Vec::with_capacity(SIGNATURE_DOMAIN.len() + envelope.signed.len());
  message.extend_from_slice(SIGNATURE_DOMAIN);
  message.extend_from_slice(envelope.signed.as_bytes());
  if key.verify(&message, &signature).is_err() {
    return err(
      Code::Signature,
      "the manifest signature does not verify against the app's update key",
    );
  }

  // Only the verified bytes are parsed from here on.
  let manifest: SignedManifest = serde_json::from_str(&envelope.signed)
    .map_err(|e| {
      UpdateError::new(
        Code::InvalidManifest,
        format!("the signed manifest is malformed: {e}"),
      )
    })?;
  if manifest.schema != SCHEMA {
    return err(
      Code::InvalidManifest,
      format!(
        "unsupported manifest schema {} (this runtime reads {SCHEMA})",
        manifest.schema
      ),
    );
  }
  if manifest.app != exp.app_id {
    return err(
      Code::WrongApp,
      format!(
        "the manifest is for {:?}, this app is {:?}",
        manifest.app, exp.app_id
      ),
    );
  }
  if manifest.published_at.trim().is_empty() || manifest.published_at.len() > 64
  {
    return err(Code::InvalidManifest, "publishedAt must be a short string");
  }
  if let Some(notes) = &manifest.release_notes
    && notes.len() > MAX_RELEASE_NOTES_BYTES
  {
    return err(Code::InvalidManifest, "releaseNotes is too long");
  }
  check_freshness(&manifest, exp)?;

  let offered = parse_version(&manifest.version, "version")?;
  let running = parse_version(exp.running_version, "the running version")
    .map_err(|e| UpdateError::new(Code::NotConfigured, e.message))?;
  let min_version = manifest
    .min_version
    .as_deref()
    .map(|v| parse_version(v, "minVersion"))
    .transpose()?;
  if let Some(min) = &min_version
    && compare_versions(min, &offered) == Ordering::Greater
  {
    return err(
      Code::InvalidManifest,
      "minVersion is higher than the offered version",
    );
  }

  match compare_versions(&offered, &running) {
    Ordering::Less => {
      return err(
        Code::Downgrade,
        format!(
          "the manifest offers {} but {} is running: a downgrade is never \
           installed",
          manifest.version, exp.running_version
        ),
      );
    }
    Ordering::Equal => {
      return Ok(ManifestVerdict::UpToDate {
        version: manifest.version,
        sequence: manifest.sequence,
      });
    }
    Ordering::Greater => {}
  }
  if exp.rejected_versions.iter().any(|rejected| {
    parse_version(rejected, "rejected")
      .is_ok_and(|r| compare_versions(&offered, &r) == Ordering::Equal)
  }) {
    return err(
      Code::Rejected,
      format!(
        "{} failed to start after it was installed and was rolled back; it \
         is not offered again (publish a newer version)",
        manifest.version
      ),
    );
  }
  let required = min_version
    .as_ref()
    .is_some_and(|min| compare_versions(&running, min) == Ordering::Less);

  let Some(entry) = manifest.platforms.get(exp.platform) else {
    return err(
      Code::NoPlatform,
      format!(
        "the manifest has no build for {} (it has: {})",
        exp.platform,
        manifest
          .platforms
          .keys()
          .cloned()
          .collect::<Vec<_>>()
          .join(", ")
      ),
    );
  };
  if entry.kind != KIND_BUNDLE {
    return err(
      Code::InvalidManifest,
      format!(
        "unsupported kind {:?} (only \"bundle\": whole signed app archives)",
        entry.kind
      ),
    );
  }
  if !is_sha256_hex(&entry.sha256) {
    return err(
      Code::InvalidManifest,
      "sha256 must be 64 lowercase hex digits",
    );
  }
  if entry.size == 0 || entry.size > MAX_ARCHIVE_BYTES {
    return err(
      Code::InvalidManifest,
      format!("size must be 1..={MAX_ARCHIVE_BYTES} bytes"),
    );
  }
  check_archive_url(&entry.url, exp.allow_insecure_loopback)?;

  Ok(ManifestVerdict::Available(VerifiedUpdate {
    version: manifest.version,
    min_version: manifest.min_version,
    required,
    platform: exp.platform.to_string(),
    url: entry.url.clone(),
    sha256: entry.sha256.clone(),
    size: entry.size,
    release_notes: manifest.release_notes,
    published_at: manifest.published_at,
    sequence: manifest.sequence,
    expires_at: manifest.expires_at,
  }))
}

/// `expiresAt` and `sequence` against the clock and the install's
/// high-water mark (see the module docs).
fn check_freshness(
  manifest: &SignedManifest,
  exp: &Expectations,
) -> Result<(), UpdateError> {
  if let Some(expires) = &manifest.expires_at {
    let Some(at) = parse_rfc3339(expires) else {
      return err(
        Code::InvalidManifest,
        format!("expiresAt {expires:?} is not an RFC 3339 timestamp"),
      );
    };
    if exp.now_unix >= at {
      return err(
        Code::Expired,
        format!(
          "the manifest expired at {expires}: the update host serves a stale \
           manifest (or this computer's clock is wrong)"
        ),
      );
    }
  }
  match (manifest.sequence, exp.sequence_floor) {
    (Some(seq), _) if seq > MAX_SEQUENCE => err(
      Code::InvalidManifest,
      format!("sequence must be 0..={MAX_SEQUENCE}"),
    ),
    (Some(seq), Some(floor)) if seq < floor => err(
      Code::Replayed,
      format!(
        "the manifest's sequence {seq} is lower than {floor}, which this \
         install already accepted: an older manifest was served again"
      ),
    ),
    (None, Some(floor)) => err(
      Code::Replayed,
      format!(
        "the manifest has no sequence, but this install already accepted \
         sequence {floor}: an older manifest was served again"
      ),
    ),
    _ => Ok(()),
  }
}

/// Parse an RFC 3339 timestamp (`YYYY-MM-DDTHH:MM:SS[.fraction]` then `Z`
/// or `+HH:MM` / `-HH:MM`) into Unix seconds (the fraction is dropped).
/// `None` for anything else.
pub fn parse_rfc3339(text: &str) -> Option<i64> {
  let b = text.as_bytes();
  if b.len() < 20 || b.len() > 64 {
    return None;
  }
  let num = |from: usize, to: usize| -> Option<i64> {
    let digits = b.get(from..to)?;
    if !digits.iter().all(u8::is_ascii_digit) {
      return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
  };
  let at = |i: usize, c: u8| b.get(i) == Some(&c);
  if !(at(4, b'-')
    && at(7, b'-')
    && at(10, b'T')
    && at(13, b':')
    && at(16, b':'))
  {
    return None;
  }
  let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
  let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
  let mut i = 19;
  if at(i, b'.') {
    i += 1;
    let start = i;
    while b.get(i).is_some_and(u8::is_ascii_digit) {
      i += 1;
    }
    if i == start {
      return None;
    }
  }
  let offset = if at(i, b'Z') && i + 1 == b.len() {
    0
  } else if (at(i, b'+') || at(i, b'-')) && i + 6 == b.len() && at(i + 3, b':')
  {
    let (oh, om) = (num(i + 1, i + 3)?, num(i + 4, i + 6)?);
    if oh > 23 || om > 59 {
      return None;
    }
    let sign = if at(i, b'-') { -1 } else { 1 };
    sign * (oh * 3600 + om * 60)
  } else {
    return None;
  };
  let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
  let days_in_month = match month {
    1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
    4 | 6 | 9 | 11 => 30,
    2 if leap => 29,
    2 => 28,
    _ => return None,
  };
  if day < 1 || day > days_in_month || hour > 23 || minute > 59 || second > 60 {
    return None;
  }
  // Days since 1970-01-01 (H. Hinnant's days_from_civil).
  let y = if month <= 2 { year - 1 } else { year };
  let era = y.div_euclid(400);
  let yoe = y - era * 400;
  let mp = (month + 9) % 12;
  let doy = (153 * mp + 2) / 5 + day - 1;
  let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
  let days = era * 146_097 + doe - 719_468;
  Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// A strict semver: `MAJOR.MINOR.PATCH[-pre][+build]`, ASCII only, no `v`
/// prefix or spaces (the npm-loose parser underneath would accept those).
pub fn parse_version(
  text: &str,
  what: &str,
) -> Result<deno_semver::Version, UpdateError> {
  let strict = !text.is_empty()
    && text.len() <= 128
    && text.as_bytes()[0].is_ascii_digit()
    && text
      .bytes()
      .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
    && text.split(['-', '+']).next().is_some_and(|core| {
      let parts: Vec<&str> = core.split('.').collect();
      parts.len() == 3
        && parts.iter().all(|p| {
          !p.is_empty()
            && p.bytes().all(|b| b.is_ascii_digit())
            && (p.len() == 1 || !p.starts_with('0'))
        })
    });
  if !strict {
    return err(
      Code::InvalidManifest,
      format!("{what} {text:?} is not a semver (MAJOR.MINOR.PATCH)"),
    );
  }
  deno_semver::Version::parse_standard(text).map_err(|e| {
    UpdateError::new(
      Code::InvalidManifest,
      format!("{what} {text:?} is not a semver: {e}"),
    )
  })
}

/// Semver precedence (build metadata ignored).
pub fn compare_versions(
  a: &deno_semver::Version,
  b: &deno_semver::Version,
) -> Ordering {
  a.cmp(b)
}

fn is_sha256_hex(s: &str) -> bool {
  s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `https://` always; `http://` only to a loopback host and only with the
/// dev flag. No credentials in the URL.
pub fn check_archive_url(
  raw: &str,
  allow_insecure_loopback: bool,
) -> Result<url::Url, UpdateError> {
  let Ok(url) = url::Url::parse(raw) else {
    return err(Code::InvalidManifest, format!("invalid url {raw:?}"));
  };
  if !url.username().is_empty() || url.password().is_some() {
    return err(Code::InsecureUrl, "the url must not carry credentials");
  }
  match url.scheme() {
    "https" => Ok(url),
    "http" if allow_insecure_loopback && is_loopback_host(&url) => Ok(url),
    "http" => err(
      Code::InsecureUrl,
      format!(
        "refusing {raw}: updates download over https only (http to a \
         loopback host needs the dev-only allowInsecureLoopback flag)"
      ),
    ),
    other => err(
      Code::InsecureUrl,
      format!("refusing the {other}: url scheme (https only)"),
    ),
  }
}

fn is_loopback_host(url: &url::Url) -> bool {
  match url.host() {
    Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
    Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
    Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
    None => false,
  }
}

#[cfg(test)]
mod tests {
  use p256::ecdsa::SigningKey;
  use p256::ecdsa::signature::Signer;
  use p256::pkcs8::EncodePublicKey;

  use super::*;

  const PLATFORM: &str = "aarch64-apple-darwin-webview";

  pub(crate) fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32].into()).unwrap()
  }

  pub(crate) fn public_b64(key: &SigningKey) -> String {
    let der = key.verifying_key().to_public_key_der().unwrap();
    base64::engine::general_purpose::STANDARD.encode(der.as_bytes())
  }

  pub(crate) fn sign(key: &SigningKey, signed: &str) -> Vec<u8> {
    let mut msg = SIGNATURE_DOMAIN.to_vec();
    msg.extend_from_slice(signed.as_bytes());
    let sig: Signature = key.sign(&msg);
    let envelope = serde_json::json!({
      "signed": signed,
      "signature": base64::engine::general_purpose::STANDARD
        .encode(sig.to_bytes()),
    });
    serde_json::to_vec(&envelope).unwrap()
  }

  fn payload(version: &str) -> serde_json::Value {
    serde_json::json!({
      "schema": 1,
      "app": "com.example.app",
      "version": version,
      "platforms": {
        PLATFORM: {
          "url": "https://updates.example.com/app.tar.gz",
          "sha256": "a".repeat(64),
          "size": 1000,
          "kind": "bundle",
        }
      },
      "publishedAt": "2026-10-01T00:00:00Z",
    })
  }

  fn exp(pk: &str) -> Expectations<'_> {
    Expectations {
      public_key: pk,
      app_id: "com.example.app",
      running_version: "1.0.0",
      platform: PLATFORM,
      rejected_versions: &[],
      sequence_floor: None,
      now_unix: NOW,
      allow_insecure_loopback: false,
    }
  }

  /// 2026-10-01T00:00:00Z.
  const NOW: i64 = 1_790_812_800;

  fn check(
    key: &SigningKey,
    payload: serde_json::Value,
  ) -> Result<ManifestVerdict, UpdateError> {
    let pk = public_b64(key);
    verify_manifest(&sign(key, &payload.to_string()), &exp(&pk))
  }

  fn code(r: Result<ManifestVerdict, UpdateError>) -> Code {
    r.unwrap_err().code
  }

  #[test]
  fn accepts_a_newer_signed_version() {
    let k = key(1);
    let ManifestVerdict::Available(u) = check(&k, payload("2.0.0")).unwrap()
    else {
      panic!("expected an update");
    };
    assert_eq!(u.version, "2.0.0");
    assert_eq!(u.size, 1000);
    assert!(!u.required);
  }

  #[test]
  fn bad_signature_is_refused() {
    let k = key(1);
    let pk = public_b64(&k);
    let mut env: serde_json::Value =
      serde_json::from_slice(&sign(&k, &payload("2.0.0").to_string())).unwrap();
    // Tamper with the signed payload after signing.
    env["signed"] = serde_json::Value::String(
      payload("2.0.0")
        .to_string()
        .replace("app.tar.gz", "evil.tar.gz"),
    );
    let r = verify_manifest(env.to_string().as_bytes(), &exp(&pk));
    assert_eq!(code(r), Code::Signature);
    // A garbage signature.
    env["signature"] = serde_json::Value::String("AAAA".into());
    let r = verify_manifest(env.to_string().as_bytes(), &exp(&pk));
    assert_eq!(code(r), Code::Signature);
  }

  #[test]
  fn missing_signature_is_refused() {
    let k = key(1);
    let pk = public_b64(&k);
    let unsigned =
      serde_json::json!({ "signed": payload("2.0.0").to_string() });
    let r = verify_manifest(unsigned.to_string().as_bytes(), &exp(&pk));
    assert_eq!(code(r), Code::InvalidManifest);
    // A bare (unwrapped) manifest is refused too.
    let r = verify_manifest(payload("2.0.0").to_string().as_bytes(), &exp(&pk));
    assert_eq!(code(r), Code::InvalidManifest);
  }

  #[test]
  fn wrong_key_is_refused() {
    let signer = key(1);
    let other = key(2);
    let pk = public_b64(&other);
    let r =
      verify_manifest(&sign(&signer, &payload("2.0.0").to_string()), &exp(&pk));
    assert_eq!(code(r), Code::Signature);
  }

  #[test]
  fn ota_domain_signature_does_not_verify() {
    // A signature over the same bytes without the app-update domain prefix
    // (as another protocol with the same key would produce) is refused.
    let k = key(1);
    let signed = payload("2.0.0").to_string();
    let sig: Signature = k.sign(signed.as_bytes());
    let env = serde_json::json!({
      "signed": signed,
      "signature": base64::engine::general_purpose::STANDARD
        .encode(sig.to_bytes()),
    });
    let pk = public_b64(&k);
    let r = verify_manifest(env.to_string().as_bytes(), &exp(&pk));
    assert_eq!(code(r), Code::Signature);
  }

  #[test]
  fn wrong_app_is_refused() {
    let k = key(1);
    let mut p = payload("2.0.0");
    p["app"] = "com.example.other".into();
    assert_eq!(code(check(&k, p)), Code::WrongApp);
  }

  #[test]
  fn downgrade_is_refused_and_equal_is_up_to_date() {
    let k = key(1);
    assert_eq!(code(check(&k, payload("0.9.9"))), Code::Downgrade);
    assert_eq!(code(check(&k, payload("1.0.0-rc.1"))), Code::Downgrade);
    assert_eq!(
      check(&k, payload("1.0.0")).unwrap(),
      ManifestVerdict::UpToDate {
        version: "1.0.0".into(),
        sequence: None,
      }
    );
    // Build metadata does not make a version newer.
    assert_eq!(
      check(&k, payload("1.0.0+build.7")).unwrap(),
      ManifestVerdict::UpToDate {
        version: "1.0.0+build.7".into(),
        sequence: None,
      }
    );
  }

  #[test]
  fn min_version_marks_required_and_never_downgrades() {
    let k = key(1);
    let mut p = payload("2.0.0");
    p["minVersion"] = "1.5.0".into();
    let ManifestVerdict::Available(u) = check(&k, p).unwrap() else {
      panic!()
    };
    assert!(u.required);
    let mut p = payload("2.0.0");
    p["minVersion"] = "1.0.0".into();
    let ManifestVerdict::Available(u) = check(&k, p).unwrap() else {
      panic!()
    };
    assert!(!u.required);
    // minVersion above the offered version is malformed.
    let mut p = payload("2.0.0");
    p["minVersion"] = "3.0.0".into();
    assert_eq!(code(check(&k, p)), Code::InvalidManifest);
    // minVersion never lets an older version through.
    let mut p = payload("0.5.0");
    p["minVersion"] = "0.1.0".into();
    assert_eq!(code(check(&k, p)), Code::Downgrade);
  }

  #[test]
  fn rejected_version_is_refused() {
    let k = key(1);
    let pk = public_b64(&k);
    let rejected = ["2.0.0".to_string(), "2.1.0".to_string()];
    let mut e = exp(&pk);
    e.rejected_versions = &rejected;
    // Every version of the set is refused, not only the last one.
    for v in ["2.0.0", "2.1.0", "2.1.0+build.2"] {
      let r = verify_manifest(&sign(&k, &payload(v).to_string()), &e);
      assert_eq!(code(r), Code::Rejected, "{v}");
    }
    let r = verify_manifest(&sign(&k, &payload("2.0.1").to_string()), &e);
    assert!(matches!(r, Ok(ManifestVerdict::Available(_))));
  }

  #[test]
  fn an_expired_manifest_is_refused() {
    let k = key(1);
    let mut p = payload("2.0.0");
    p["expiresAt"] = "2026-10-01T00:00:01.000Z".into();
    let ManifestVerdict::Available(u) = check(&k, p.clone()).unwrap() else {
      panic!("expected an update");
    };
    assert_eq!(u.expires_at.as_deref(), Some("2026-10-01T00:00:01.000Z"));
    // At the expiry instant and after: refused, also when it offers the
    // running version (a frozen "up to date").
    p["expiresAt"] = "2026-10-01T00:00:00Z".into();
    assert_eq!(code(check(&k, p.clone())), Code::Expired);
    p["version"] = "1.0.0".into();
    assert_eq!(code(check(&k, p.clone())), Code::Expired);
    // An offset is honored: 02:00+02:00 is 00:00Z.
    p["expiresAt"] = "2026-10-01T02:00:00+02:00".into();
    assert_eq!(code(check(&k, p.clone())), Code::Expired);
    p["expiresAt"] = "2026-10-01T02:00:01+02:00".into();
    assert!(check(&k, p.clone()).is_ok());
    for bad in [
      "tomorrow",
      "2026-10-01",
      "2026-13-01T00:00:00Z",
      "1790812800",
    ] {
      p["expiresAt"] = bad.into();
      assert_eq!(code(check(&k, p.clone())), Code::InvalidManifest, "{bad}");
    }
    p["expiresAt"] = 1_790_812_900.into();
    assert_eq!(code(check(&k, p)), Code::InvalidManifest);
  }

  #[test]
  fn a_replayed_sequence_is_refused() {
    let k = key(1);
    let pk = public_b64(&k);
    let signed = |v: &str, seq: Option<serde_json::Value>| {
      let mut p = payload(v);
      if let Some(seq) = seq {
        p["sequence"] = seq;
      }
      sign(&k, &p.to_string())
    };
    // No floor yet: sequenced and unsequenced manifests both verify.
    let ManifestVerdict::Available(u) =
      verify_manifest(&signed("2.0.0", Some(10.into())), &exp(&pk)).unwrap()
    else {
      panic!("expected an update");
    };
    assert_eq!(u.sequence, Some(10));
    assert!(verify_manifest(&signed("2.0.0", None), &exp(&pk)).is_ok());
    let mut e = exp(&pk);
    e.sequence_floor = Some(10);
    // The same release again, and a newer one: accepted.
    assert!(verify_manifest(&signed("2.0.0", Some(10.into())), &e).is_ok());
    assert!(verify_manifest(&signed("2.0.0", Some(11.into())), &e).is_ok());
    // An older one, or one without a sequence: replayed, even when it offers
    // the running version (the freeze).
    for (v, seq) in [
      ("2.0.0", Some(9.into())),
      ("1.0.0", Some(9.into())),
      ("2.0.0", None),
    ] {
      let r = verify_manifest(&signed(v, seq.clone()), &e);
      assert_eq!(code(r), Code::Replayed, "{v} {seq:?}");
    }
    // An UpToDate verdict carries its sequence (to record it).
    assert_eq!(
      verify_manifest(&signed("1.0.0", Some(12.into())), &e).unwrap(),
      ManifestVerdict::UpToDate {
        version: "1.0.0".into(),
        sequence: Some(12),
      }
    );
    // Not a non-negative integer within JavaScript's safe range: malformed.
    for bad in [
      serde_json::json!(-1),
      serde_json::json!(1.5),
      serde_json::json!("12"),
      serde_json::json!(MAX_SEQUENCE + 1),
    ] {
      let r = verify_manifest(&signed("2.0.0", Some(bad.clone())), &exp(&pk));
      assert_eq!(code(r), Code::InvalidManifest, "{bad}");
    }
    assert!(
      verify_manifest(&signed("2.0.0", Some(MAX_SEQUENCE.into())), &exp(&pk))
        .is_ok()
    );
  }

  #[test]
  fn rfc3339_timestamps() {
    assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(parse_rfc3339("2026-10-01T00:00:00Z"), Some(NOW));
    assert_eq!(parse_rfc3339("2026-10-01T00:00:00.123Z"), Some(NOW));
    assert_eq!(parse_rfc3339("2000-02-29T12:34:56Z"), Some(951_827_696));
    assert_eq!(parse_rfc3339("1969-12-31T23:59:59Z"), Some(-1));
    assert_eq!(parse_rfc3339("2026-10-01T01:30:00+01:30"), Some(NOW));
    assert_eq!(parse_rfc3339("2026-09-30T22:00:00-02:00"), Some(NOW));
    for bad in [
      "",
      "2026-10-01T00:00:00",
      "2026-10-01 00:00:00Z",
      "2026-10-01T00:00:00.Z",
      "2026-02-29T00:00:00Z",
      "2026-10-01T24:00:00Z",
      "2026-10-01T00:00:00+0200",
      "2026-10-01T00:00:00Zjunk",
      "+026-10-01T00:00:00Z",
    ] {
      assert_eq!(parse_rfc3339(bad), None, "{bad}");
    }
  }

  #[test]
  fn http_url_is_refused_unless_loopback_dev() {
    let k = key(1);
    let pk = public_b64(&k);
    let mut p = payload("2.0.0");
    p["platforms"][PLATFORM]["url"] = "http://updates.example.com/a".into();
    let env = sign(&k, &p.to_string());
    assert_eq!(code(verify_manifest(&env, &exp(&pk))), Code::InsecureUrl);
    let mut e = exp(&pk);
    e.allow_insecure_loopback = true;
    // Still refused: not loopback.
    assert_eq!(code(verify_manifest(&env, &e)), Code::InsecureUrl);
    let mut p = payload("2.0.0");
    p["platforms"][PLATFORM]["url"] = "http://127.0.0.1:8080/a".into();
    let env = sign(&k, &p.to_string());
    assert_eq!(code(verify_manifest(&env, &exp(&pk))), Code::InsecureUrl);
    assert!(verify_manifest(&env, &e).is_ok());
    let mut p = payload("2.0.0");
    p["platforms"][PLATFORM]["url"] = "file:///etc/passwd".into();
    let env = sign(&k, &p.to_string());
    assert_eq!(code(verify_manifest(&env, &e)), Code::InsecureUrl);
    let mut p = payload("2.0.0");
    p["platforms"][PLATFORM]["url"] = "https://u:p@example.com/a".into();
    let env = sign(&k, &p.to_string());
    assert_eq!(code(verify_manifest(&env, &e)), Code::InsecureUrl);
  }

  #[test]
  fn malformed_payloads_are_refused() {
    let k = key(1);
    let mut p = payload("2.0.0");
    p["extra"] = 1.into();
    assert_eq!(code(check(&k, p)), Code::InvalidManifest);
    let mut p = payload("2.0.0");
    p["schema"] = 2.into();
    assert_eq!(code(check(&k, p)), Code::InvalidManifest);
    assert_eq!(code(check(&k, payload("v2.0.0"))), Code::InvalidManifest);
    assert_eq!(code(check(&k, payload("2.0"))), Code::InvalidManifest);
    assert_eq!(code(check(&k, payload("02.0.0"))), Code::InvalidManifest);
    let mut p = payload("2.0.0");
    p["platforms"][PLATFORM]["sha256"] = "A".repeat(64).into();
    assert_eq!(code(check(&k, p)), Code::InvalidManifest);
    let mut p = payload("2.0.0");
    p["platforms"][PLATFORM]["size"] = 0.into();
    assert_eq!(code(check(&k, p)), Code::InvalidManifest);
    let mut p = payload("2.0.0");
    p["platforms"][PLATFORM]["kind"] = "patch".into();
    assert_eq!(code(check(&k, p)), Code::InvalidManifest);
    let mut p = payload("2.0.0");
    p["platforms"] = serde_json::json!({ "x86_64-pc-windows-msvc-webview": p["platforms"][PLATFORM].clone() });
    assert_eq!(code(check(&k, p)), Code::NoPlatform);
    let big = vec![b' '; MAX_MANIFEST_BYTES + 1];
    let pk = public_b64(&k);
    assert_eq!(
      code(verify_manifest(&big, &exp(&pk))),
      Code::InvalidManifest
    );
  }

  #[test]
  fn public_key_formats() {
    let k = key(3);
    let b64 = public_b64(&k);
    assert!(parse_public_key(&b64).is_ok());
    let pem = format!(
      "-----BEGIN PUBLIC KEY-----\n{}\n{}\n-----END PUBLIC KEY-----\n",
      &b64[..64],
      &b64[64..]
    );
    assert!(parse_public_key(&pem).is_ok());
    assert_eq!(
      parse_public_key("not a key").unwrap_err().code,
      Code::NotConfigured
    );
    // An Ed25519 SPKI is not accepted.
    let ed = "MCowBQYDK2VwAyEAGb9ECWmEzf6FQbrBZ9w7lshQhqowtrbLDFw4rXAxZuE=";
    assert_eq!(parse_public_key(ed).unwrap_err().code, Code::NotConfigured);
  }

  /// A manifest signed by denext's WebCrypto signer (`denext desktop
  /// publish-update`, ECDSA P-256 via `crypto.subtle.sign`) verifies here:
  /// the two sides agree on the key format, the signature encoding and the
  /// signed bytes. Regenerate with `tests/desktop-app-update-vector.ts` in
  /// denext.
  #[test]
  fn verifies_a_webcrypto_signed_vector() {
    let r = verify_manifest(
      WEBCRYPTO_VECTOR_ENVELOPE.as_bytes(),
      &Expectations {
        public_key: WEBCRYPTO_VECTOR_PUBLIC_KEY,
        app_id: "com.example.vector",
        running_version: "1.0.0",
        platform: "x86_64-unknown-linux-gnu-webview",
        rejected_versions: &[],
        sequence_floor: None,
        now_unix: NOW,
        allow_insecure_loopback: false,
      },
    );
    let ManifestVerdict::Available(u) = r.unwrap() else {
      panic!("expected an update")
    };
    assert_eq!(u.version, "1.2.3");
    assert_eq!(u.size, 4242);
  }

  // Generated by denext (WebCrypto ECDSA P-256), see the test above.
  const WEBCRYPTO_VECTOR_PUBLIC_KEY: &str =
    include_str!("testdata/webcrypto_vector.pub");
  const WEBCRYPTO_VECTOR_ENVELOPE: &str =
    include_str!("testdata/webcrypto_vector.json");
}
