// Copyright 2018-2026 the Deno authors. MIT license.

//! Pinned native TypeScript (tsgo) binaries (denext).
//!
//! Every native compiler this fork runs is an exact version of the official
//! `@typescript/typescript-<platform>` npm packages, pinned per role in
//! `typescript_pins.json` by the tarball's npm `dist.integrity` (sha512).
//! The same file drives the release archive (`fetch_typescript.py` vendors the
//! binary next to `deno`), the test server and `tools/update_typescript_pin.ts`
//! (the one command that bumps a pin), so there is a single pin mechanism.
//!
//! A binary is looked up, in order:
//!  1. the role's override environment variable (a path to `tsc[.exe]`);
//!  2. `<dir of the deno executable>/typescript-<role>/tsc[.exe]`, what the
//!     denext CLI archive ships, when it reports the pinned version;
//!  3. `$DENO_DIR/dl/typescript-<role>-<version>/<platform>/tsc[.exe]`,
//!     downloaded on first use from the pinned tarball URL and verified
//!     against the pinned integrity (never against what the registry says
//!     today).

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;

use base64::Engine;
use deno_core::anyhow;
use deno_core::anyhow::Context;
use deno_core::error::AnyError;
use deno_core::serde_json;
use deno_core::url::Url;
use serde::Deserialize;
use sha2::Digest;

use crate::cache::DenoDir;
use crate::http_util::HttpClient;

/// One pinned platform tarball.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PinnedTarball {
  pub tarball: String,
  pub integrity: String,
}

/// A pinned native TypeScript version for one role (e.g. `lsp`).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct NativeTsPin {
  pub version: String,
  pub platforms: BTreeMap<String, PinnedTarball>,
}

/// The raw pins file, embedded so a build always knows its pins.
pub const TYPESCRIPT_PINS_JSON: &str = include_str!("typescript_pins.json");

static PINS: LazyLock<BTreeMap<String, NativeTsPin>> = LazyLock::new(|| {
  parse_pins(TYPESCRIPT_PINS_JSON).expect("valid cli/tsc/typescript_pins.json")
});

fn parse_pins(text: &str) -> Result<BTreeMap<String, NativeTsPin>, AnyError> {
  let value: serde_json::Value = serde_json::from_str(text)?;
  let mut pins = BTreeMap::new();
  let serde_json::Value::Object(map) = value else {
    anyhow::bail!("typescript_pins.json must be an object");
  };
  for (role, entry) in map {
    if role.starts_with('$') {
      continue;
    }
    let pin: NativeTsPin = serde_json::from_value(entry)
      .with_context(|| format!("invalid pin for role \"{role}\""))?;
    pins.insert(role, pin);
  }
  Ok(pins)
}

/// The pin for `role`. Panics on an unknown role: roles are compile-time
/// constants of this crate.
pub fn pin(role: &str) -> &'static NativeTsPin {
  PINS
    .get(role)
    .unwrap_or_else(|| panic!("no TypeScript pin for role \"{role}\""))
}

/// The npm platform-package suffix (`@typescript/typescript-<suffix>`) for
/// an `(ARCH, OS)` pair as in [`std::env::consts`].
pub fn platform_for(arch: &str, os: &str) -> Option<&'static str> {
  Some(match (arch, os) {
    ("x86_64", "linux") => "linux-x64",
    ("aarch64", "linux") => "linux-arm64",
    ("x86_64", "macos") => "darwin-x64",
    ("aarch64", "macos") => "darwin-arm64",
    ("x86_64", "windows") => "win32-x64",
    ("aarch64", "windows") => "win32-arm64",
    _ => return None,
  })
}

pub fn host_platform() -> Option<&'static str> {
  platform_for(std::env::consts::ARCH, std::env::consts::OS)
}

pub fn exe_name() -> &'static str {
  if cfg!(windows) { "tsc.exe" } else { "tsc" }
}

/// The directory, next to the `deno` executable, that the CLI archive ships
/// a role's binary in.
pub fn vendored_dir_name(role: &str) -> String {
  format!("typescript-{role}")
}

/// What `<tsc> --version` reports (`Version 7.1.0-dev.…` → `7.1.0-dev.…`),
/// or `None` when it can't be run.
pub fn version_of(path: &Path) -> Option<String> {
  let output = std::process::Command::new(path)
    .arg("--version")
    .stdin(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .output()
    .ok()?;
  if !output.status.success() {
    return None;
  }
  let text = String::from_utf8_lossy(&output.stdout);
  let text = text.trim();
  Some(text.strip_prefix("Version ").unwrap_or(text).to_string())
}

/// A binary for `role` that needs no download: the override, else the one
/// vendored next to `current_exe` when it reports the pinned version.
pub fn find_local(
  role: &str,
  override_env: &str,
  override_path: Option<std::ffi::OsString>,
  current_exe: Option<PathBuf>,
) -> Result<Option<PathBuf>, AnyError> {
  let pin = pin(role);
  if let Some(path) = override_path.filter(|p| !p.is_empty()) {
    let path = PathBuf::from(path);
    if !path.is_file() {
      anyhow::bail!(
        "{} is set to {}, which is not a file",
        override_env,
        path.display()
      );
    }
    match version_of(&path) {
      Some(v) if v == pin.version => {}
      other => log::warn!(
        "{} {} reports version {}, expected {}; using it anyway",
        override_env,
        path.display(),
        other.as_deref().unwrap_or("<unknown>"),
        pin.version
      ),
    }
    return Ok(Some(path));
  }
  let Some(vendored) = current_exe
    .as_deref()
    .and_then(Path::parent)
    .map(|d| d.join(vendored_dir_name(role)).join(exe_name()))
  else {
    return Ok(None);
  };
  if !vendored.is_file() {
    return Ok(None);
  }
  match version_of(&vendored) {
    Some(v) if v == pin.version => Ok(Some(vendored)),
    other => {
      log::debug!(
        "ignoring {}: version {}, expected {}",
        vendored.display(),
        other.as_deref().unwrap_or("<unknown>"),
        pin.version
      );
      Ok(None)
    }
  }
}

/// Where a downloaded binary for `role` lives in `DENO_DIR`.
pub fn cache_dir(deno_dir: &DenoDir, role: &str, platform: &str) -> PathBuf {
  let pin = pin(role);
  deno_dir
    .dl_folder_path()
    .join(format!("typescript-{role}-{}", pin.version))
    .join(platform)
}

/// Check `bytes` against an npm `sha512-<base64>` integrity string.
pub fn verify_integrity(bytes: &[u8], integrity: &str) -> Result<(), AnyError> {
  let Some(expected) = integrity.strip_prefix("sha512-") else {
    anyhow::bail!("unsupported integrity \"{integrity}\" (want sha512)");
  };
  let actual = base64::engine::general_purpose::STANDARD
    .encode(sha2::Sha512::digest(bytes));
  if actual != expected {
    anyhow::bail!(
      "integrity mismatch: got sha512-{actual}, pinned {integrity}"
    );
  }
  Ok(())
}

/// Unpack the package's `lib/` (the binary and the default `lib.*.d.ts` it
/// loads from its own directory) plus `LICENSE` and `NOTICE.txt` from an npm
/// tarball into `out_dir`, flattened (`package/lib/x` → `out_dir/x`).
pub fn extract_package(tgz: &[u8], out_dir: &Path) -> Result<(), AnyError> {
  std::fs::create_dir_all(out_dir)?;
  let mut archive =
    tar::Archive::new(flate2::read::GzDecoder::new(std::io::Cursor::new(tgz)));
  let mut found_exe = false;
  for entry in archive.entries()? {
    let mut entry = entry?;
    if !entry.header().entry_type().is_file() {
      continue;
    }
    let path = entry.path()?.into_owned();
    let Ok(rel) = path.strip_prefix("package") else {
      continue;
    };
    let target = if let Ok(lib_rel) = rel.strip_prefix("lib") {
      lib_rel.to_path_buf()
    } else if rel == Path::new("LICENSE") || rel == Path::new("NOTICE.txt") {
      rel.to_path_buf()
    } else {
      continue;
    };
    if target
      .components()
      .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
      anyhow::bail!("unexpected path in tarball: {}", path.display());
    }
    let dest = out_dir.join(&target);
    if let Some(parent) = dest.parent() {
      std::fs::create_dir_all(parent)?;
    }
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes)?;
    std::fs::write(&dest, &bytes)
      .with_context(|| format!("failed to write {}", dest.display()))?;
    if target == Path::new(exe_name()) {
      found_exe = true;
      #[cfg(unix)]
      {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
          &dest,
          std::fs::Permissions::from_mode(0o755),
        )?;
      }
    }
  }
  if !found_exe {
    anyhow::bail!("the tarball has no package/lib/{}", exe_name());
  }
  Ok(())
}

/// Download (once) and return the pinned binary for `role` in `DENO_DIR`.
pub async fn ensure_downloaded(
  role: &str,
  deno_dir: &DenoDir,
  http_client: &HttpClient,
) -> Result<PathBuf, AnyError> {
  let pin = pin(role);
  let platform = host_platform().ok_or_else(|| {
    anyhow::anyhow!(
      "no native TypeScript build for {} {}",
      std::env::consts::ARCH,
      std::env::consts::OS
    )
  })?;
  let install_dir = cache_dir(deno_dir, role, platform);
  let exe = install_dir.join(exe_name());
  if exe.is_file() {
    return Ok(exe);
  }
  let tarball = pin.platforms.get(platform).ok_or_else(|| {
    anyhow::anyhow!(
      "typescript_pins.json has no {platform} tarball for role \"{role}\""
    )
  })?;
  log::info!(
    "Downloading TypeScript {} ({platform}) for {role}",
    pin.version
  );
  let bytes = http_client
    .download(Url::parse(&tarball.tarball)?)
    .await
    .with_context(|| format!("failed to download {}", tarball.tarball))?;
  verify_integrity(&bytes, &tarball.integrity)
    .with_context(|| format!("refusing {}", tarball.tarball))?;
  // Unpack beside the final directory and rename it into place, so a
  // concurrent process never sees a half-written tree through `is_file()`.
  let parent = install_dir.parent().unwrap();
  std::fs::create_dir_all(parent)?;
  let tmp_dir = parent.join(format!(".{platform}-{}.tmp", std::process::id()));
  let _ = std::fs::remove_dir_all(&tmp_dir);
  if let Err(err) = extract_package(&bytes, &tmp_dir) {
    let _ = std::fs::remove_dir_all(&tmp_dir);
    return Err(err);
  }
  match std::fs::rename(&tmp_dir, &install_dir) {
    Ok(()) => {}
    Err(_) if exe.is_file() => {
      let _ = std::fs::remove_dir_all(&tmp_dir);
    }
    Err(err) => {
      let _ = std::fs::remove_dir_all(&tmp_dir);
      return Err(err).with_context(|| {
        format!("failed to move TypeScript into {}", install_dir.display())
      });
    }
  }
  Ok(exe)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn pins_file_parses_and_covers_every_platform() {
    let pins = parse_pins(TYPESCRIPT_PINS_JSON).unwrap();
    assert!(pins.contains_key("lsp"));
    for (role, pin) in &pins {
      for (arch, os) in [
        ("x86_64", "linux"),
        ("aarch64", "linux"),
        ("x86_64", "macos"),
        ("aarch64", "macos"),
        ("x86_64", "windows"),
        ("aarch64", "windows"),
      ] {
        let platform = platform_for(arch, os).unwrap();
        let entry = pin
          .platforms
          .get(platform)
          .unwrap_or_else(|| panic!("{role}: no {platform}"));
        assert!(entry.integrity.starts_with("sha512-"), "{role} {platform}");
        assert!(
          entry
            .tarball
            .ends_with(&format!("/typescript-{platform}-{}.tgz", pin.version)),
          "{role} {platform}: {}",
          entry.tarball
        );
      }
    }
  }

  #[test]
  fn lsp_pin_is_an_exact_dated_nightly() {
    let version = &pin("lsp").version;
    let re = lazy_regex::regex!(r"^7\.\d+\.\d+-dev\.\d{8}\.\d+$");
    assert!(re.is_match(version), "{version}");
  }

  #[test]
  fn integrity_is_checked() {
    let bytes = b"hello";
    let good = format!(
      "sha512-{}",
      base64::engine::general_purpose::STANDARD
        .encode(sha2::Sha512::digest(bytes))
    );
    verify_integrity(bytes, &good).unwrap();
    assert!(verify_integrity(b"hellp", &good).is_err());
    assert!(verify_integrity(bytes, "sha1-abc").is_err());
  }

  fn tgz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
      Vec::new(),
      flate2::Compression::fast(),
    ));
    for (path, data) in files {
      let mut header = tar::Header::new_gnu();
      header.set_size(data.len() as u64);
      header.set_mode(0o644);
      header.set_cksum();
      builder.append_data(&mut header, path, *data).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
  }

  #[test]
  fn extracts_lib_and_notices_flattened() {
    let dir = tempfile::tempdir().unwrap();
    let exe = format!("package/lib/{}", exe_name());
    let bytes = tgz(&[
      (&exe, b"bin"),
      ("package/lib/lib.es5.d.ts", b"decl"),
      ("package/LICENSE", b"apache"),
      ("package/NOTICE.txt", b"notice"),
      ("package/README.md", b"skip"),
    ]);
    extract_package(&bytes, dir.path()).unwrap();
    assert_eq!(std::fs::read(dir.path().join(exe_name())).unwrap(), b"bin");
    assert!(dir.path().join("lib.es5.d.ts").is_file());
    assert!(dir.path().join("LICENSE").is_file());
    assert!(dir.path().join("NOTICE.txt").is_file());
    assert!(!dir.path().join("README.md").exists());
  }

  #[test]
  fn extract_requires_the_binary() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = tgz(&[("package/lib/lib.es5.d.ts", b"decl")]);
    assert!(extract_package(&bytes, dir.path()).is_err());
  }

  #[test]
  fn override_that_is_not_a_file_is_an_error() {
    assert!(
      find_local(
        "lsp",
        "X_TEST",
        Some("/definitely/not/here/tsc".into()),
        None
      )
      .is_err()
    );
  }

  #[test]
  fn no_vendored_binary_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let found =
      find_local("lsp", "X_TEST", None, Some(dir.path().join("deno"))).unwrap();
    assert!(found.is_none());
  }
}
