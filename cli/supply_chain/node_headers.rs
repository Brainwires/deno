// Copyright 2018-2026 the Deno authors. MIT license.

//! Node's C/C++ headers for node-gyp, offline (DENEXT.md item NNN).
//!
//! A native addon built from source (`node-gyp rebuild`, also prebuild-install's
//! and node-pre-gyp's fallback) needs the headers of the Node version the
//! runtime reports. node-gyp downloads them from nodejs.org into a cache under
//! the home directory, which a jailed lifecycle script can neither reach nor
//! keep. The installer itself provides them instead: the
//! `node-v<version>-headers.tar.gz` of exactly `process.version`, checked
//! against the SHA-256 pinned here, unpacked once into the Deno cache
//! (`<DENO_DIR>/node_headers/v<version>`), and handed to the script as
//! `npm_config_nodedir`, so node-gyp builds without touching the network.
//!
//! The archive is taken from beside the `deno` executable when a release ships
//! it there, else downloaded from `NODEJS_ORG_MIRROR` (node-gyp's own mirror
//! variable) or nodejs.org; either way only a file matching the pin is used.
//! Windows is left out: node-gyp also needs `node.lib` there, which the
//! headers archive doesn't carry.

use std::io::Read;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use deno_core::anyhow::Context;
use deno_core::anyhow::bail;
use deno_core::error::AnyError;
use deno_core::url::Url;
use sha2::Digest;

use crate::http_util::HttpClientProvider;

/// The Node version whose headers are pinned: the `process.version` the
/// runtime reports (`deno_node::NODE_VERSION`, checked by a test).
pub const NODE_HEADERS_VERSION: &str = "26.5.1";
/// SHA-256 of `node-v26.5.1-headers.tar.gz`, from nodejs.org's
/// `SHASUMS256.txt` for v26.5.1.
pub const NODE_HEADERS_SHA256: &str =
  "598a3139679db65461f8af50463d668008eaee5e0d09df4eeaa27f3eeefecc03";

/// The marker written last into an unpacked headers directory: the archive's
/// checksum. A directory without it (an interrupted unpack) is not used.
const MARKER: &str = ".deno-headers-sha256";

/// The archive's file name.
pub fn archive_name(version: &str) -> String {
  format!("node-v{version}-headers.tar.gz")
}

/// Where the archive is downloaded from: `NODEJS_ORG_MIRROR` (as node-gyp
/// reads it), else nodejs.org's release directory.
fn archive_url(version: &str, mirror: Option<&str>) -> Result<Url, AnyError> {
  let base = mirror
    .map(str::trim)
    .filter(|m| !m.is_empty())
    .unwrap_or("https://nodejs.org/download/release");
  let base = base.trim_end_matches('/');
  Ok(Url::parse(&format!(
    "{base}/v{version}/{}",
    archive_name(version)
  ))?)
}

/// Whether `dir` is a complete unpack of the archive with this checksum.
fn is_complete(dir: &Path, sha256: &str) -> bool {
  std::fs::read_to_string(dir.join(MARKER)).is_ok_and(|m| m.trim() == sha256)
    && dir
      .join("include")
      .join("node")
      .join("common.gypi")
      .is_file()
}

/// Checks `bytes` against `sha256` and unpacks the archive's
/// `node-v<version>/` tree into `dest` (which must not exist yet; it is
/// created through a temporary sibling and renamed into place). Only regular
/// files and directories inside that tree are accepted.
pub fn unpack_verified(
  bytes: &[u8],
  version: &str,
  sha256: &str,
  dest: &Path,
) -> Result<PathBuf, AnyError> {
  let actual = faster_hex::hex_string(&sha2::Sha256::digest(bytes));
  if !actual.eq_ignore_ascii_case(sha256) {
    bail!(
      "{} doesn't match its pinned checksum (expected {sha256}, got {actual})",
      archive_name(version)
    );
  }
  let parent = dest.parent().context("headers directory has no parent")?;
  std::fs::create_dir_all(parent)?;
  let tmp = tempfile::Builder::new()
    .prefix(".node-headers-")
    .tempdir_in(parent)?;
  let root = format!("node-v{version}");
  let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
  for entry in archive.entries()? {
    let mut entry = entry?;
    let path = entry.path()?.into_owned();
    let mut components = path.components();
    match components.next() {
      Some(Component::Normal(first)) if first == root.as_str() => {}
      _ => bail!("unexpected path in the headers archive: {}", path.display()),
    }
    let rest = components.as_path();
    if rest.as_os_str().is_empty() {
      continue;
    }
    if !rest.components().all(|c| matches!(c, Component::Normal(_))) {
      bail!("unexpected path in the headers archive: {}", path.display());
    }
    let target = tmp.path().join(rest);
    match entry.header().entry_type() {
      tar::EntryType::Directory => std::fs::create_dir_all(&target)?,
      tar::EntryType::Regular => {
        if let Some(dir) = target.parent() {
          std::fs::create_dir_all(dir)?;
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        std::fs::write(&target, data)?;
      }
      other => bail!(
        "unexpected entry type {other:?} in the headers archive: {}",
        path.display()
      ),
    }
  }
  if !tmp.path().join("include/node/common.gypi").is_file() {
    bail!("{} has no include/node/common.gypi", archive_name(version));
  }
  std::fs::write(tmp.path().join(MARKER), sha256)?;
  let tmp = tmp.into_path();
  if let Err(err) = std::fs::rename(&tmp, dest) {
    let _ = std::fs::remove_dir_all(&tmp);
    // another install unpacked it first
    if !is_complete(dest, sha256) {
      return Err(err.into());
    }
  }
  Ok(dest.to_path_buf())
}

/// The headers, fetched and unpacked the first time a jailed script needs
/// them, at most once per install; and the pinned node-gyp itself, which a
/// jailed script can't fetch from the registry either.
pub struct NodeHeaders {
  cache_root: PathBuf,
  node_gyp_root: PathBuf,
  http_client_provider: Arc<HttpClientProvider>,
  dir: tokio::sync::OnceCell<Option<PathBuf>>,
  node_gyp: tokio::sync::OnceCell<Option<PathBuf>>,
}

impl std::fmt::Debug for NodeHeaders {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("NodeHeaders")
      .field("cache_root", &self.cache_root)
      .finish()
  }
}

impl NodeHeaders {
  /// Keeps the headers in `<deno_dir>/node_headers` and node-gyp in
  /// `<deno_dir>/node_gyp`.
  pub fn new(
    deno_dir: &Path,
    http_client_provider: Arc<HttpClientProvider>,
  ) -> Self {
    Self {
      cache_root: deno_dir.join("node_headers"),
      node_gyp_root: deno_dir.join("node_gyp"),
      http_client_provider,
      dir: Default::default(),
      node_gyp: Default::default(),
    }
  }

  /// The pinned node-gyp (`task_runner::NODE_GYP_NPM_SPECIFIER`) installed
  /// with its dependencies into the Deno cache, outside the jail, the first
  /// time a jailed script needs it: the entry point a jailed script runs as
  /// `DENO_NODE_GYP`. `None` when it can't be installed here; the script then
  /// fetches it itself, as before.
  pub async fn node_gyp(&self) -> Option<PathBuf> {
    self
      .node_gyp
      .get_or_init(|| async {
        match self.install_node_gyp().await {
          Ok(path) => Some(path),
          Err(err) => {
            log::debug!("jail-scripts: node-gyp couldn't be provided: {err:#}");
            None
          }
        }
      })
      .await
      .clone()
  }

  async fn install_node_gyp(&self) -> Result<PathBuf, AnyError> {
    let version = crate::task_runner::NODE_GYP_NPM_SPECIFIER
      .strip_prefix("npm:node-gyp@")
      .context("unexpected node-gyp specifier")?;
    let dir = self.node_gyp_root.join(version);
    let entry = dir.join("node_modules/node-gyp/bin/node-gyp.js");
    let marker = dir.join(".deno-node-gyp-installed");
    if marker.is_file() && entry.is_file() {
      return Ok(entry);
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
      dir.join("package.json"),
      format!(
        "{{ \"name\": \"deno-node-gyp\", \"private\": true, \"dependencies\": {{ \"node-gyp\": \"{version}\" }} }}\n"
      ),
    )?;
    #[allow(clippy::disallowed_methods, reason = "spawning this executable")]
    let exe = std::env::current_exe()?;
    // node-gyp's dependencies have no install scripts: none are approved
    let output = tokio::process::Command::new(exe)
      .args(["install", "--node-modules-dir=auto", "--quiet"])
      .current_dir(&dir)
      .env("DENO_NO_UPDATE_CHECK", "1")
      .stdin(std::process::Stdio::null())
      .output()
      .await
      .context("running deno install for node-gyp")?;
    if !output.status.success() || !entry.is_file() {
      bail!(
        "installing npm:node-gyp@{version} failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
      );
    }
    std::fs::write(&marker, version)?;
    Ok(entry)
  }

  /// The unpacked headers directory (`npm_config_nodedir`), or `None` when
  /// they can't be had here (node-gyp then downloads them itself).
  pub async fn dir(&self, mirror: Option<&str>) -> Option<PathBuf> {
    if cfg!(windows) {
      return None;
    }
    self
      .dir
      .get_or_init(|| async {
        match self.resolve(mirror).await {
          Ok(dir) => Some(dir),
          Err(err) => {
            // node-gyp then fetches them itself, as it does outside the
            // jail (the built-in exceptions list nodejs.org for it)
            log::debug!(
              "jail-scripts: Node's headers for node-gyp couldn't be provided: {err:#}"
            );
            None
          }
        }
      })
      .await
      .clone()
  }

  async fn resolve(&self, mirror: Option<&str>) -> Result<PathBuf, AnyError> {
    let version = NODE_HEADERS_VERSION;
    let dest = self.cache_root.join(format!("v{version}"));
    if is_complete(&dest, NODE_HEADERS_SHA256) {
      return Ok(dest);
    }
    if dest.exists() {
      // an interrupted unpack
      let _ = std::fs::remove_dir_all(&dest);
    }
    // shipped beside the executable
    #[allow(clippy::disallowed_methods, reason = "locating this executable")]
    let beside = std::env::current_exe()
      .ok()
      .and_then(|exe| Some(exe.parent()?.join(archive_name(version))));
    if let Some(path) = beside
      && let Ok(bytes) = std::fs::read(&path)
    {
      return unpack_verified(&bytes, version, NODE_HEADERS_SHA256, &dest);
    }
    let url = archive_url(version, mirror)?;
    log::debug!("Downloading Node's headers from {url}");
    let client = self.http_client_provider.get_or_create()?;
    let bytes = client
      .download(url.clone())
      .await
      .with_context(|| format!("downloading {url}"))?;
    unpack_verified(&bytes, version, NODE_HEADERS_SHA256, &dest)
  }
}

/// Whether a package builds a native addon with node-gyp: it has a
/// `binding.gyp` (npm runs `node-gyp rebuild` for one without an install
/// script) or its scripts name node-gyp or a prebuilt-binary installer that
/// falls back to it.
pub fn needs_node_gyp(package_folder: &Path, script: &str) -> bool {
  package_folder.join("binding.gyp").is_file()
    || ["node-gyp", "prebuild-install", "node-pre-gyp", "cmake-js"]
      .iter()
      .any(|tool| script.contains(tool))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
      Vec::new(),
      flate2::Compression::fast(),
    ));
    for (path, data) in entries {
      let mut header = tar::Header::new_gnu();
      header.set_size(data.len() as u64);
      header.set_mode(0o644);
      header.set_entry_type(tar::EntryType::Regular);
      // set_path refuses `..`; write the raw name to test the check
      let name = header.as_old_mut().name.as_mut();
      name[..path.len()].copy_from_slice(path.as_bytes());
      header.set_cksum();
      builder.append(&header, *data).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
  }

  fn sha(bytes: &[u8]) -> String {
    faster_hex::hex_string(&sha2::Sha256::digest(bytes))
  }

  #[test]
  fn pinned_version_is_the_runtime_node_version() {
    assert_eq!(NODE_HEADERS_VERSION, deno_runtime::deno_node::NODE_VERSION);
    assert_eq!(NODE_HEADERS_SHA256.len(), 64);
  }

  #[test]
  fn unpacks_a_matching_archive() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = archive(&[
      ("node-v1.2.3/include/node/common.gypi", b"{}"),
      ("node-v1.2.3/include/node/node_api.h", b"/* api */"),
    ]);
    let dest = dir.path().join("v1.2.3");
    let out = unpack_verified(&bytes, "1.2.3", &sha(&bytes), &dest).unwrap();
    assert_eq!(out, dest);
    assert_eq!(
      std::fs::read_to_string(dest.join("include/node/node_api.h")).unwrap(),
      "/* api */"
    );
    assert!(is_complete(&dest, &sha(&bytes)));
    // no temporary directory left behind
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
  }

  #[test]
  fn refuses_a_wrong_checksum_or_path() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = archive(&[("node-v1.2.3/include/node/common.gypi", b"{}")]);
    let err =
      unpack_verified(&bytes, "1.2.3", &"0".repeat(64), &dir.path().join("a"))
        .unwrap_err();
    assert!(err.to_string().contains("pinned checksum"), "{err}");
    assert!(!dir.path().join("a").exists());

    for path in [
      "node-v1.2.3/../escape.h",
      "other/include/node/common.gypi",
      "/abs/common.gypi",
    ] {
      let bytes = archive(&[
        ("node-v1.2.3/include/node/common.gypi", b"{}"),
        (path, b"x"),
      ]);
      let dest = dir.path().join("b");
      let err =
        unpack_verified(&bytes, "1.2.3", &sha(&bytes), &dest).unwrap_err();
      assert!(err.to_string().contains("unexpected path"), "{path}: {err}");
      assert!(!dest.exists());
      assert!(!dir.path().join("escape.h").exists());
    }

    // not the headers archive at all
    let bytes = archive(&[("node-v1.2.3/README.md", b"x")]);
    let err =
      unpack_verified(&bytes, "1.2.3", &sha(&bytes), &dir.path().join("c"))
        .unwrap_err();
    assert!(err.to_string().contains("common.gypi"), "{err}");
  }

  #[tokio::test]
  async fn installed_node_gyp_is_reused() {
    let dir = tempfile::tempdir().unwrap();
    let headers = NodeHeaders::new(
      dir.path(),
      Arc::new(HttpClientProvider::new(None, None)),
    );
    let version = crate::task_runner::NODE_GYP_NPM_SPECIFIER
      .strip_prefix("npm:node-gyp@")
      .unwrap();
    let root = dir.path().join("node_gyp").join(version);
    let entry = root.join("node_modules/node-gyp/bin/node-gyp.js");
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    std::fs::write(&entry, "").unwrap();
    std::fs::write(root.join(".deno-node-gyp-installed"), version).unwrap();
    assert_eq!(headers.node_gyp().await, Some(entry));
  }

  #[test]
  fn mirror_url() {
    assert_eq!(
      archive_url("26.5.1", None).unwrap().as_str(),
      "https://nodejs.org/download/release/v26.5.1/node-v26.5.1-headers.tar.gz"
    );
    assert_eq!(
      archive_url("26.5.1", Some("https://mirror.example/node/"))
        .unwrap()
        .as_str(),
      "https://mirror.example/node/v26.5.1/node-v26.5.1-headers.tar.gz"
    );
  }

  #[test]
  fn node_gyp_packages() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!needs_node_gyp(dir.path(), "node install.js"));
    assert!(needs_node_gyp(
      dir.path(),
      "prebuild-install || node-gyp rebuild"
    ));
    std::fs::write(dir.path().join("binding.gyp"), "{}").unwrap();
    assert!(needs_node_gyp(dir.path(), "node install.js"));
  }
}
