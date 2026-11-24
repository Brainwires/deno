// Copyright 2018-2026 the Deno authors. MIT license.

//! denext (DENEXT.md item 5NN): the `deno` CLI checks denext's signed
//! revoked-runtimes list at startup.
//!
//! denext (`src/build/runtime-revocations.ts`) keeps a signed list of
//! runtime releases maintainers pulled after release, cached under the
//! toolchain cache (`<DENO_DIR>/denext-deno/revocations.json`). denext reads
//! it before it runs a pinned runtime; a `deno` linked onto `PATH` by
//! `denext runtime link`, or run directly, never asked. This module reads
//! that same cached file, and when it revokes this build (component `cli`,
//! this version, this target) prints one line to stderr, at most once a day.
//!
//! The envelope is `{ "signed": "<JSON>", "signature": "<base64 r‖s>" }`
//! (or `signatures: [...]`): ECDSA P-256 / SHA-256 over
//! `"denext-runtime-revocations-v1\n" + signed`, verified against the public
//! keys compiled in here (a copy of denext's `runtime-revocation-keys.ts`)
//! before the payload is parsed. A list that expired, is dated more than a
//! day ahead, or has a `sequence` below the floor compiled in is ignored.
//!
//! It never touches the network, never fails and never blocks: no file, or
//! any error at all, is silence. Only the file's existence is checked on a
//! machine without denext; the read is capped at 64 KiB.
//!
//! Switches: `DENEXT_REVOCATION_CHECK=0` turns it off (denext's own switch),
//! `DENEXT_ALLOW_REVOKED_RUNTIME=<this version>` silences it for this version,
//! and `DENEXT_REVOCATION_NOTICED` (set by denext on the processes it starts
//! once it said a revocation) keeps a child from repeating it.

use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use aws_lc_rs::signature;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use deno_core::serde_json;

/// What the signature covers before the signed string.
const SIGNATURE_DOMAIN: &str = "denext-runtime-revocations-v1\n";

/// The compiled-in public keys (base64 SPKI, ECDSA P-256): denext's
/// `RUNTIME_REVOCATION_PUBLIC_KEYS`, `[active, backup]`.
const PUBLIC_KEYS: [&str; 2] = [
  "MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE1ONKcv2NsDjbz5bEPbnVY5QnmWzoJ8DY6gxOVZI9XLM0cUGc5xlLy/Db921wMOvVJeBCa4R5wkttcd+P6wAO/A==",
  "MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEuDh/JXMBjzN1LfZg/kmonu+aGsHQutmtVJzJG2q5Gfc5FdcAgUzgtCzvfUNmfAML4Ysc8myF6rD9Gp0IifyXOw==",
];

/// The lowest list `sequence` taken: denext's
/// `RUNTIME_REVOCATION_MIN_SEQUENCE` when this fork was cut.
const MIN_SEQUENCE: i64 = 1795192910;

/// The largest list read (denext's `REVOCATION_MAX_BYTES`).
const MAX_BYTES: u64 = 64 * 1024;
/// How far ahead of the clock a list may be dated.
const MAX_FUTURE_MS: i64 = 24 * 3600 * 1000;
/// The shortest time between two notices.
const NOTICE_INTERVAL_SECS: i64 = 24 * 3600;

/// The cached list, beside denext's toolchain versions.
const CACHE_FILE: &str = "revocations.json";
/// When this CLI last said a revocation (`<version>\n<unix seconds>\n`).
const MARKER_FILE: &str = "revocations.noticed";

/// `0` turns the check off.
const CHECK_ENV: &str = "DENEXT_REVOCATION_CHECK";
/// `=<version>`: run that revoked version without a notice.
const ALLOW_ENV: &str = "DENEXT_ALLOW_REVOKED_RUNTIME";
/// Set by denext on its children once it said a revocation.
const NOTICED_ENV: &str = "DENEXT_REVOCATION_NOTICED";
/// Tests: a JSON file holding the public keys (base64 SPKI) to verify with
/// instead of the compiled-in ones. A notice from such a list says so.
const TEST_KEYS_FILE_ENV: &str = "DENO_TESTING_REVOCATION_KEYS_FILE";

/// The DER prefix of a P-256 public key's SubjectPublicKeyInfo, before the
/// 65-byte uncompressed point.
const P256_SPKI_PREFIX: [u8; 26] = [
  0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01,
  0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
];

/// The entry that revokes this build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revocation {
  pub reason: String,
  pub advisory: Option<String>,
  pub fixed_in: Option<String>,
}

/// What a check needs to know about this build and the clock.
pub struct Subject<'a> {
  pub version: &'a str,
  pub triple: &'a str,
  pub now_ms: i64,
  pub min_sequence: i64,
}

/// The uncompressed point of a base64 P-256 SPKI key, or `None`.
fn spki_point(key: &str) -> Option<Vec<u8>> {
  let der = STANDARD.decode(key.trim()).ok()?;
  let point = der.strip_prefix(&P256_SPKI_PREFIX[..])?;
  (point.len() == 65 && point[0] == 0x04).then(|| point.to_vec())
}

/// Whether one of `signatures` verifies `signed` under one of `keys`.
fn signature_verifies(
  signed: &str,
  signatures: &[&str],
  keys: &[String],
) -> bool {
  let mut message = Vec::with_capacity(SIGNATURE_DOMAIN.len() + signed.len());
  message.extend_from_slice(SIGNATURE_DOMAIN.as_bytes());
  message.extend_from_slice(signed.as_bytes());
  keys.iter().filter_map(|k| spki_point(k)).any(|point| {
    let key = signature::UnparsedPublicKey::new(
      &signature::ECDSA_P256_SHA256_FIXED,
      point,
    );
    signatures.iter().any(|s| {
      STANDARD
        .decode(s)
        .is_ok_and(|sig| key.verify(&message, &sig).is_ok())
    })
  })
}

/// An RFC 3339 timestamp (as denext's `isTimestamp`) in epoch ms.
fn timestamp_ms(v: Option<&serde_json::Value>) -> Option<i64> {
  let s = v?.as_str()?;
  let b = s.as_bytes();
  let shaped = b.len() > 10
    && b[..4].iter().all(u8::is_ascii_digit)
    && b[4] == b'-'
    && b[7] == b'-'
    && b[10] == b'T';
  if !shaped {
    return None;
  }
  chrono::DateTime::parse_from_rfc3339(s)
    .ok()
    .map(|d| d.timestamp_millis())
}

/// An optional string field: `Ok(None)` when absent, `Err` when malformed.
fn optional_string(
  e: &serde_json::Map<String, serde_json::Value>,
  field: &str,
  ok: impl Fn(&str) -> bool,
) -> Result<Option<String>, ()> {
  match e.get(field) {
    None => Ok(None),
    Some(serde_json::Value::String(s)) if ok(s) => Ok(Some(s.clone())),
    Some(_) => Err(()),
  }
}

/// One `cli` entry checked as denext checks it; `Ok(None)` when it does not
/// name this build. A malformed known entry refuses the whole list (`Err`).
fn entry_hit(
  raw: &serde_json::Value,
  version: &str,
  triple: &str,
) -> Result<Option<Revocation>, ()> {
  let e = raw.as_object().ok_or(())?;
  let component = e.get("component").and_then(|c| c.as_str());
  if component != Some("cli") && component != Some("desktop") {
    return Ok(None); // a component this build does not know
  }
  let entry_version = e
    .get("version")
    .and_then(|v| v.as_str())
    .filter(|v| !v.is_empty())
    .ok_or(())?;
  let reason = e.get("reason").and_then(|r| r.as_str()).ok_or(())?;
  let targets = match e.get("targets") {
    None => None,
    Some(serde_json::Value::Array(list)) => Some(
      list
        .iter()
        .map(|t| t.as_str().filter(|t| !t.is_empty()).ok_or(()))
        .collect::<Result<Vec<_>, ()>>()?,
    ),
    Some(_) => return Err(()),
  };
  let advisory = optional_string(e, "advisory", |s| s.starts_with("https://"))?;
  let fixed_in = optional_string(e, "fixedIn", |s| !s.is_empty())?;
  let hit = component == Some("cli")
    && entry_version == version
    && targets.is_none_or(|t| t.contains(&triple));
  Ok(hit.then(|| Revocation {
    reason: reason.to_string(),
    advisory,
    fixed_in,
  }))
}

/// Verify a cached document against `keys` and return the entry revoking
/// `subject`, if any. Every refusal (size, shape, signature, schema, a
/// malformed field, expiry, a future date, a sequence below the floor) is
/// `None`.
pub fn revocation_for(
  text: &[u8],
  keys: &[String],
  subject: &Subject<'_>,
) -> Option<Revocation> {
  if text.len() as u64 > MAX_BYTES {
    return None;
  }
  let envelope: serde_json::Value = serde_json::from_slice(text).ok()?;
  let signed = envelope.get("signed")?.as_str()?;
  let signatures: Vec<&str> =
    match (envelope.get("signature"), envelope.get("signatures")) {
      (Some(serde_json::Value::String(s)), _) => vec![s.as_str()],
      (_, Some(serde_json::Value::Array(list))) if !list.is_empty() => list
        .iter()
        .map(|s| s.as_str())
        .collect::<Option<Vec<_>>>()?,
      _ => return None,
    };
  if !signature_verifies(signed, &signatures, keys) {
    return None;
  }
  let payload: serde_json::Value = serde_json::from_str(signed).ok()?;
  if payload.get("schema").and_then(|s| s.as_i64()) != Some(1) {
    return None;
  }
  let issued = timestamp_ms(payload.get("issuedAt"))?;
  let expires = timestamp_ms(payload.get("expiresAt"))?;
  let sequence = payload.get("sequence")?.as_i64().filter(|s| *s >= 0)?;
  let revoked = payload.get("revoked")?.as_array()?;
  if sequence.saturating_mul(1000) > subject.now_ms + MAX_FUTURE_MS
    || issued > subject.now_ms + MAX_FUTURE_MS
    || subject.now_ms >= expires
    || sequence < subject.min_sequence
  {
    return None;
  }
  let mut found = None;
  for raw in revoked {
    let hit = entry_hit(raw, subject.version, subject.triple).ok()?;
    if found.is_none() {
      found = hit;
    }
  }
  found
}

/// `s` with control characters (a newline, an escape sequence) removed, so a
/// notice stays one plain line.
fn plain(s: &str) -> String {
  s.chars()
    .filter(|c| !c.is_control() && !matches!(c, '\u{202a}'..='\u{202e}'))
    .collect()
}

/// The one stderr line about this build being revoked.
pub fn notice(
  hit: &Revocation,
  version: &str,
  triple: &str,
  test_keys: bool,
) -> String {
  let why = std::iter::once(plain(&hit.reason))
    .chain(hit.advisory.as_deref().map(plain))
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join("; ");
  let fixed = hit
    .fixed_in
    .as_deref()
    .map(|f| format!(" (fixed in {})", plain(f)))
    .unwrap_or_default();
  format!(
    "warning{}: this Deno ({version}, {triple}) is a revoked denext runtime: \
     {why}. Run `denext runtime install` for the safe release{fixed}; \
     {ALLOW_ENV}={version} silences this.",
    if test_keys { " (test keys)" } else { "" },
  )
}

/// Whether a notice is due, given the marker file's text: not when this
/// version said one less than a day ago.
pub fn notice_due(marker: Option<&str>, version: &str, now_secs: i64) -> bool {
  let Some(marker) = marker else {
    return true;
  };
  let mut lines = marker.lines();
  let (Some(v), Some(at)) = (lines.next(), lines.next()) else {
    return true;
  };
  let Ok(at) = at.trim().parse::<i64>() else {
    return true;
  };
  let age = now_secs - at;
  !(v == version && (0..NOTICE_INTERVAL_SECS).contains(&age))
}

/// `value` when it is an absolute path (a relative `HOME` would put the
/// cache under the working directory).
fn absolute(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
  let p = PathBuf::from(value?);
  p.is_absolute().then_some(p)
}

/// Deno's cache directory as denext resolves it (`deno-cache-dir.ts`):
/// `DENO_DIR`, else the platform default, from absolute variables only.
fn deno_cache_dir(
  env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> Option<PathBuf> {
  if let Some(dir) = env("DENO_DIR").filter(|d| !d.is_empty()) {
    return Some(PathBuf::from(dir));
  }
  if cfg!(windows) {
    if let Some(local) = absolute(env("LOCALAPPDATA")) {
      return Some(local.join("deno"));
    }
  } else if !cfg!(target_os = "macos")
    && let Some(xdg) = absolute(env("XDG_CACHE_HOME"))
  {
    return Some(xdg.join("deno"));
  }
  let home = absolute(env("HOME")).or_else(|| absolute(env("USERPROFILE")))?;
  Some(if cfg!(target_os = "macos") {
    home.join("Library").join("Caches").join("deno")
  } else if cfg!(windows) {
    home.join("AppData").join("Local").join("deno")
  } else {
    home.join(".cache").join("deno")
  })
}

/// The file's bytes, when it is no larger than [`MAX_BYTES`].
fn read_capped(path: &Path) -> Option<Vec<u8>> {
  let file = std::fs::File::open(path).ok()?;
  if file.metadata().ok()?.len() > MAX_BYTES {
    return None;
  }
  let mut bytes = Vec::new();
  file.take(MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
  (bytes.len() as u64 <= MAX_BYTES).then_some(bytes)
}

/// The keys from the test override file (a JSON array of base64 SPKI keys).
fn test_keys(path: &Path) -> Option<Vec<String>> {
  serde_json::from_slice(&read_capped(path)?).ok()
}

/// The startup check (see the module docs). Never fails, never prints
/// anything but the notice.
pub fn check_at_startup(version: &str) {
  let env = |k: &str| std::env::var_os(k);
  if env(CHECK_ENV).is_some_and(|v| v.to_str().map(str::trim) == Some("0"))
    || env(NOTICED_ENV).is_some_and(|v| !v.is_empty())
    || env(ALLOW_ENV)
      .is_some_and(|v| v.to_str().map(str::trim) == Some(version))
  {
    return;
  }
  let Some(root) = deno_cache_dir(&env).map(|d| d.join("denext-deno")) else {
    return;
  };
  // The common case on a machine without denext: one failed open.
  let Some(text) = read_capped(&root.join(CACHE_FILE)) else {
    return;
  };
  let test_file = env(TEST_KEYS_FILE_ENV).filter(|v| !v.is_empty());
  let keys = match &test_file {
    Some(file) => match test_keys(Path::new(file)) {
      Some(keys) => keys,
      None => return,
    },
    None => PUBLIC_KEYS.iter().map(|k| k.to_string()).collect(),
  };
  let Ok(now) =
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
  else {
    return;
  };
  let triple = env!("TARGET");
  let subject = Subject {
    version,
    triple,
    now_ms: i64::try_from(now.as_millis()).unwrap_or(i64::MAX),
    min_sequence: MIN_SEQUENCE,
  };
  let Some(hit) = revocation_for(&text, &keys, &subject) else {
    return;
  };
  let now_secs = i64::try_from(now.as_secs()).unwrap_or(i64::MAX);
  let marker_path = root.join(MARKER_FILE);
  let marker =
    read_capped(&marker_path).and_then(|b| String::from_utf8(b).ok());
  if !notice_due(marker.as_deref(), version, now_secs) {
    return;
  }
  let line = notice(&hit, version, triple, test_file.is_some());
  let _ = writeln!(std::io::stderr(), "{line}");
  let _ = std::fs::write(&marker_path, format!("{version}\n{now_secs}\n"));
}

#[cfg(test)]
mod tests {
  use aws_lc_rs::rand::SystemRandom;
  use aws_lc_rs::signature::EcdsaKeyPair;
  use aws_lc_rs::signature::KeyPair;

  use super::*;

  const NOW: i64 = 1_800_000_000_000;

  fn keypair() -> (EcdsaKeyPair, String) {
    let pair =
      EcdsaKeyPair::generate(&signature::ECDSA_P256_SHA256_FIXED_SIGNING)
        .unwrap();
    let mut spki = P256_SPKI_PREFIX.to_vec();
    spki.extend_from_slice(pair.public_key().as_ref());
    let key = STANDARD.encode(spki);
    (pair, key)
  }

  fn envelope(pair: &EcdsaKeyPair, payload: &serde_json::Value) -> Vec<u8> {
    let signed = payload.to_string();
    let message = format!("{SIGNATURE_DOMAIN}{signed}");
    let sig = pair.sign(&SystemRandom::new(), message.as_bytes()).unwrap();
    serde_json::json!({
      "signed": signed,
      "signature": STANDARD.encode(sig.as_ref()),
    })
    .to_string()
    .into_bytes()
  }

  fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
      .unwrap()
      .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
  }

  fn payload(revoked: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
      "schema": 1,
      "issuedAt": iso(NOW - 3_600_000),
      "expiresAt": iso(NOW + 21 * 86_400_000),
      "sequence": (NOW - 3_600_000) / 1000,
      "revoked": revoked,
    })
  }

  fn subject() -> Subject<'static> {
    Subject {
      version: "3.0.0-denext.9",
      triple: "x86_64-unknown-linux-gnu",
      now_ms: NOW,
      min_sequence: 0,
    }
  }

  fn cli_entry() -> serde_json::Value {
    serde_json::json!({
      "component": "cli",
      "version": "3.0.0-denext.9",
      "reason": "a sandbox escape",
      "advisory": "https://github.com/Brainwires/deno/security/advisories/GHSA-x",
      "fixedIn": "3.0.0-denext.10",
    })
  }

  #[test]
  fn a_signed_list_revoking_this_build_is_a_hit() {
    let (pair, key) = keypair();
    let text = envelope(&pair, &payload(serde_json::json!([cli_entry()])));
    let hit = revocation_for(&text, &[key], &subject()).unwrap();
    assert_eq!(hit.reason, "a sandbox escape");
    assert_eq!(hit.fixed_in.as_deref(), Some("3.0.0-denext.10"));
  }

  #[test]
  fn signatures_list_form_verifies() {
    let (pair, key) = keypair();
    let text = envelope(&pair, &payload(serde_json::json!([cli_entry()])));
    let mut v: serde_json::Value = serde_json::from_slice(&text).unwrap();
    let sig = v["signature"].take();
    v.as_object_mut().unwrap().remove("signature");
    v["signatures"] = serde_json::json!(["AAAA", sig]);
    let text = v.to_string().into_bytes();
    assert!(revocation_for(&text, &[key], &subject()).is_some());
  }

  #[test]
  fn another_key_or_a_changed_payload_verifies_nothing() {
    let (pair, _) = keypair();
    let (_, other) = keypair();
    let text = envelope(&pair, &payload(serde_json::json!([cli_entry()])));
    assert!(revocation_for(&text, &[other], &subject()).is_none());
    // the compiled-in keys never verify a test key's list
    let compiled: Vec<String> =
      PUBLIC_KEYS.iter().map(|k| k.to_string()).collect();
    assert!(revocation_for(&text, &compiled, &subject()).is_none());
    // a tampered payload
    let (pair, key) = keypair();
    let text = envelope(&pair, &payload(serde_json::json!([])));
    let mut v: serde_json::Value = serde_json::from_slice(&text).unwrap();
    v["signed"] = serde_json::Value::String(
      payload(serde_json::json!([cli_entry()])).to_string(),
    );
    let text = v.to_string().into_bytes();
    assert!(revocation_for(&text, &[key], &subject()).is_none());
  }

  #[test]
  fn the_compiled_in_keys_parse() {
    for key in PUBLIC_KEYS {
      assert!(spki_point(key).is_some(), "{key}");
    }
  }

  #[test]
  fn other_versions_components_and_targets_are_no_hit() {
    let (pair, key) = keypair();
    let mut other_version = cli_entry();
    other_version["version"] = "3.0.0-denext.8".into();
    let mut desktop = cli_entry();
    desktop["component"] = "desktop".into();
    let mut other_target = cli_entry();
    other_target["targets"] = serde_json::json!(["aarch64-apple-darwin"]);
    let mut unknown = cli_entry();
    unknown["component"] = "future-thing".into();
    unknown["version"] = serde_json::Value::Null;
    let text = envelope(
      &pair,
      &payload(serde_json::json!([
        other_version,
        desktop,
        other_target,
        unknown
      ])),
    );
    assert!(
      revocation_for(&text, std::slice::from_ref(&key), &subject()).is_none()
    );
    let mut this_target = cli_entry();
    this_target["targets"] =
      serde_json::json!(["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"]);
    let text = envelope(&pair, &payload(serde_json::json!([this_target])));
    assert!(revocation_for(&text, &[key], &subject()).is_some());
  }

  #[test]
  fn expired_future_and_replayed_lists_are_ignored() {
    let (pair, key) = keypair();
    let keys = [key];
    let mut expired = payload(serde_json::json!([cli_entry()]));
    expired["expiresAt"] = iso(NOW).into();
    let text = envelope(&pair, &expired);
    assert!(revocation_for(&text, &keys, &subject()).is_none());

    let mut future = payload(serde_json::json!([cli_entry()]));
    future["issuedAt"] = iso(NOW + 2 * 86_400_000).into();
    let text = envelope(&pair, &future);
    assert!(revocation_for(&text, &keys, &subject()).is_none());

    let mut future_seq = payload(serde_json::json!([cli_entry()]));
    future_seq["sequence"] = ((NOW + 2 * 86_400_000) / 1000).into();
    let text = envelope(&pair, &future_seq);
    assert!(revocation_for(&text, &keys, &subject()).is_none());

    let text = envelope(&pair, &payload(serde_json::json!([cli_entry()])));
    let floor = Subject {
      min_sequence: NOW / 1000,
      ..subject()
    };
    assert!(revocation_for(&text, &keys, &floor).is_none());
  }

  #[test]
  fn malformed_lists_are_ignored() {
    let (pair, key) = keypair();
    let keys = [key];
    let mut bad_schema = payload(serde_json::json!([cli_entry()]));
    bad_schema["schema"] = 2.into();
    let mut bad_advisory = cli_entry();
    bad_advisory["advisory"] = "http://insecure".into();
    let mut no_reason = cli_entry();
    no_reason.as_object_mut().unwrap().remove("reason");
    let mut bad_targets = cli_entry();
    bad_targets["targets"] = "x86_64-unknown-linux-gnu".into();
    for p in [
      bad_schema,
      payload(serde_json::json!([cli_entry(), bad_advisory])),
      payload(serde_json::json!([no_reason])),
      payload(serde_json::json!([bad_targets])),
      payload(serde_json::json!({})),
    ] {
      let text = envelope(&pair, &p);
      assert!(revocation_for(&text, &keys, &subject()).is_none(), "{p}");
    }
    for text in [&b"not json"[..], b"{}", b"{\"signed\":\"{}\"}"] {
      assert!(revocation_for(text, &keys, &subject()).is_none());
    }
    let big = vec![b' '; MAX_BYTES as usize + 1];
    assert!(revocation_for(&big, &keys, &subject()).is_none());
  }

  #[test]
  fn the_notice_is_one_plain_line() {
    let hit = Revocation {
      reason: "bad\n\u{1b}[31mthing\u{202e}".into(),
      advisory: Some("https://example.com/a".into()),
      fixed_in: Some("3.0.0-denext.10".into()),
    };
    let line = notice(&hit, "3.0.0-denext.9", "aarch64-apple-darwin", false);
    assert_eq!(
      line,
      "warning: this Deno (3.0.0-denext.9, aarch64-apple-darwin) is a revoked \
       denext runtime: bad[31mthing; https://example.com/a. Run `denext \
       runtime install` for the safe release (fixed in 3.0.0-denext.10); \
       DENEXT_ALLOW_REVOKED_RUNTIME=3.0.0-denext.9 silences this."
    );
    let bare = Revocation {
      reason: "r".into(),
      advisory: None,
      fixed_in: None,
    };
    assert!(notice(&bare, "v", "t", true).starts_with("warning (test keys): "));
  }

  #[test]
  fn a_notice_is_due_once_a_day_per_version() {
    let now = 1_800_000_000;
    assert!(notice_due(None, "v1", now));
    assert!(notice_due(Some("garbage"), "v1", now));
    assert!(!notice_due(Some("v1\n1800000000\n"), "v1", now + 3600));
    assert!(notice_due(Some("v1\n1800000000\n"), "v1", now + 86_400));
    assert!(notice_due(Some("v0\n1800000000\n"), "v1", now + 60));
    // a marker from the future (a clock set back) does not silence it
    assert!(notice_due(Some("v1\n1800090000\n"), "v1", now));
  }

  #[test]
  fn the_cache_dir_follows_denext() {
    let env = |vars: &'static [(&'static str, &'static str)]| {
      move |k: &str| {
        vars
          .iter()
          .find(|(n, _)| *n == k)
          .map(|(_, v)| std::ffi::OsString::from(v))
      }
    };
    assert_eq!(
      deno_cache_dir(&env(&[("DENO_DIR", "/d"), ("HOME", "/h")])),
      Some(PathBuf::from("/d"))
    );
    // a relative HOME is never the working directory
    assert_eq!(deno_cache_dir(&env(&[("HOME", "rel")])), None);
    #[cfg(target_os = "macos")]
    assert_eq!(
      deno_cache_dir(&env(&[("HOME", "/h")])),
      Some(PathBuf::from("/h/Library/Caches/deno"))
    );
    #[cfg(target_os = "linux")]
    {
      assert_eq!(
        deno_cache_dir(&env(&[("HOME", "/h")])),
        Some(PathBuf::from("/h/.cache/deno"))
      );
      assert_eq!(
        deno_cache_dir(&env(&[("HOME", "/h"), ("XDG_CACHE_HOME", "/x")])),
        Some(PathBuf::from("/x/deno"))
      );
    }
  }
}
