// Copyright 2018-2026 the Deno authors. MIT license.

//! Node's C/C++ headers for node-gyp, offline (DENEXT.md item 288).
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
//! On Windows node-gyp also links the addon against `node.lib`, which the
//! headers archive doesn't carry: the installer writes one beside the headers
//! (`<nodedir>/Release/node.lib`, and `Debug`), an import library for
//! `node.exe` naming exactly the Node-API symbols `deno.exe` exports
//! (`deno_napi::WINDOWS_SYMBOL_EXPORTS_DEF`). node-gyp's addons load
//! `node.exe` delay-loaded, and its `win_delay_load_hook` answers that import
//! with the host executable, so they bind to `deno.exe` at run time.

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
    let dest = self.resolve_headers(mirror).await?;
    if cfg!(windows) {
      write_node_libs(&dest, host_machine()?)?;
    }
    Ok(dest)
  }

  async fn resolve_headers(
    &self,
    mirror: Option<&str>,
  ) -> Result<PathBuf, AnyError> {
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

/// The symbol names a module definition (`.def`) exports.
fn def_exports(def: &str) -> Vec<&str> {
  def
    .lines()
    .map(str::trim)
    .skip_while(|line| *line != "EXPORTS")
    .skip(1)
    .filter(|line| !line.is_empty() && !line.starts_with(';'))
    .collect()
}

/// The machine `node.lib` is written for: this executable's, which is the
/// `process.arch` node-gyp builds for.
fn host_machine() -> Result<ar_archive_writer::MachineTypes, AnyError> {
  use ar_archive_writer::MachineTypes;
  match std::env::consts::ARCH {
    "x86_64" => Ok(MachineTypes::AMD64),
    "aarch64" => Ok(MachineTypes::ARM64),
    arch => bail!("no node.lib for {arch}"),
  }
}

/// An import library for `node.exe` naming every symbol `deno.exe` exports
/// to Node-API addons, in the format `lib.exe /def:` writes.
pub fn node_import_library(
  machine: ar_archive_writer::MachineTypes,
) -> Result<Vec<u8>, AnyError> {
  let exports =
    def_exports(deno_runtime::deno_napi::WINDOWS_SYMBOL_EXPORTS_DEF)
      .into_iter()
      .map(|name| ar_archive_writer::COFFShortExport {
        name: name.to_string(),
        ext_name: None,
        symbol_name: None,
        import_name: None,
        export_as: None,
        ordinal: 0,
        noname: false,
        data: false,
        private: false,
        constant: false,
      })
      .collect::<Vec<_>>();
  if exports.is_empty() {
    bail!("the Node-API symbol list is empty");
  }
  let mut out = std::io::Cursor::new(Vec::new());
  ar_archive_writer::write_import_library(
    &mut out,
    "node.exe",
    &exports,
    machine,
    false,
    false,
    &[],
  )
  .context("writing node.lib")?;
  Ok(out.into_inner())
}

/// Writes `node.lib` where node-gyp looks for it when `npm_config_nodedir`
/// names `dir`: `<dir>/$(Configuration)/node.lib`, for both configurations.
/// A file already holding the same bytes is left alone.
fn write_node_libs(
  dir: &Path,
  machine: ar_archive_writer::MachineTypes,
) -> Result<(), AnyError> {
  let lib = node_import_library(machine)?;
  for configuration in ["Release", "Debug"] {
    let path = dir.join(configuration).join("node.lib");
    if std::fs::read(&path).is_ok_and(|existing| existing == lib) {
      continue;
    }
    std::fs::create_dir_all(path.parent().unwrap())?;
    // written beside, then renamed: a concurrent build never links a
    // half-written library
    let tmp = path.with_extension(format!("lib.tmp-{}", std::process::id()));
    std::fs::write(&tmp, &lib)?;
    std::fs::rename(&tmp, &path)
      .inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
      })
      .with_context(|| format!("writing {}", path.display()))?;
  }
  Ok(())
}

/// The directory holding the `node-gyp` command a node-gyp build's own
/// children find on `PATH` (DENEXT.md item 297), written into `dir`: npm puts
/// one on `PATH` for every lifecycle script, and node-gyp-build (`bcrypt`,
/// and most addons that ship prebuilds) spawns `node-gyp` by name when no
/// prebuild fits, where the script's shell's own `node-gyp` doesn't reach.
/// It runs `DENO_NODE_GYP` (else the pinned node-gyp) on `deno`, as the
/// shell's `node-gyp` does.
pub fn write_node_gyp_shim(dir: &Path, deno: &Path) -> std::io::Result<()> {
  std::fs::create_dir_all(dir)?;
  let fd_var = deno_runtime::deno_process::NPM_RESOLUTION_STATE_FD_ENV_VAR_NAME;
  let pkg_json_var = crate::task_runner::USE_PKG_JSON_HIDDEN_ENV_VAR_NAME;
  let fallback = crate::task_runner::NODE_GYP_NPM_SPECIFIER;
  let (name, text) = if cfg!(windows) {
    (
      "node-gyp.cmd",
      format!(
        "@echo off\r\nset \"{fd_var}=\"\r\nset \"{pkg_json_var}=\"\r\nif \"%DENO_NODE_GYP%\"==\"\" (\r\n  \"{deno}\" run -A --no-config --node-modules-dir=none {fallback} %*\r\n) else (\r\n  \"{deno}\" run -A --no-config --node-modules-dir=none \"%DENO_NODE_GYP%\" %*\r\n)\r\n",
        deno = deno.display(),
      ),
    )
  } else {
    let deno = deno.to_string_lossy().replace('\'', "'\\''");
    (
      "node-gyp",
      format!(
        "#!/bin/sh\nunset {fd_var} {pkg_json_var}\nexec '{deno}' run -A --no-config --node-modules-dir=none \"${{DENO_NODE_GYP:-{fallback}}}\" \"$@\"\n"
      ),
    )
  };
  let path = dir.join(name);
  std::fs::write(&path, text)?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
  }
  Ok(())
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

  // DENEXT.md item 297: a node-gyp build's children find `node-gyp` on PATH
  // (node-gyp-build spawns it by name), and it runs DENO_NODE_GYP on deno
  #[cfg(unix)]
  #[test]
  fn node_gyp_shim_runs_the_pinned_node_gyp_on_deno() {
    let dir = tempfile::tempdir().unwrap();
    // a stand-in `deno` that prints its arguments and the variables the
    // shim must clear
    let fake = dir.path().join("it's deno");
    std::fs::write(
      &fake,
      "#!/bin/sh\nprintf '%s|' \"$@\"\necho \"fd=${DENO_DONT_USE_INTERNAL_NODE_COMPAT_STATE_FD-unset}\"\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))
      .unwrap();
    let bin = dir.path().join("bin");
    write_node_gyp_shim(&bin, &fake).unwrap();
    let shim = bin.join("node-gyp");
    let mode = std::fs::metadata(&shim).unwrap().permissions().mode();
    assert_eq!(mode & 0o111, 0o111);
    let run = |gyp: Option<&str>| {
      let mut command = std::process::Command::new(&shim);
      command
        .args(["rebuild", "--release"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("DENO_DONT_USE_INTERNAL_NODE_COMPAT_STATE_FD", "3");
      if let Some(gyp) = gyp {
        command.env("DENO_NODE_GYP", gyp);
      }
      String::from_utf8(command.output().unwrap().stdout).unwrap()
    };
    assert_eq!(
      run(Some("/cache/node-gyp.js")),
      "run|-A|--no-config|--node-modules-dir=none|/cache/node-gyp.js|rebuild|--release|fd=unset\n"
    );
    assert_eq!(
      run(None),
      format!(
        "run|-A|--no-config|--node-modules-dir=none|{}|rebuild|--release|fd=unset\n",
        crate::task_runner::NODE_GYP_NPM_SPECIFIER
      )
    );
  }

  /// The short import members of an import library: (machine, symbol,
  /// DLL, import type, name type), read from the archive by hand.
  fn short_imports(lib: &[u8]) -> Vec<(u16, String, String, u16, u16)> {
    assert!(lib.starts_with(b"!<arch>\n"));
    let mut out = Vec::new();
    let mut at = 8;
    while at + 60 <= lib.len() {
      let header = &lib[at..at + 60];
      assert_eq!(&header[58..60], b"`\n");
      let size: usize = std::str::from_utf8(&header[48..58])
        .unwrap()
        .trim()
        .parse()
        .unwrap();
      let data = &lib[at + 60..at + 60 + size];
      if data.len() > 20 && data[..4] == [0, 0, 0xff, 0xff] {
        let u16_at = |i: usize| u16::from_le_bytes([data[i], data[i + 1]]);
        let mut names = data[20..].split(|b| *b == 0);
        let symbol = String::from_utf8(names.next().unwrap().to_vec()).unwrap();
        let dll = String::from_utf8(names.next().unwrap().to_vec()).unwrap();
        let kind = u16_at(18);
        out.push((u16_at(6), symbol, dll, kind & 0x3, (kind >> 2) & 0x7));
      }
      at += 60 + size + (size & 1);
    }
    out
  }

  // DENEXT.md item 297: Windows node-gyp links against `<nodedir>/
  // $(Configuration)/node.lib`; the one written names every symbol deno.exe
  // exports to addons, imported by name from node.exe (which node-gyp's
  // delay-load hook answers with the host executable)
  #[test]
  fn node_lib_imports_every_napi_symbol_from_node_exe() {
    use ar_archive_writer::MachineTypes;
    let expected =
      def_exports(deno_runtime::deno_napi::WINDOWS_SYMBOL_EXPORTS_DEF);
    assert!(expected.contains(&"napi_create_function"));
    assert!(expected.contains(&"napi_module_register"));
    assert!(expected.contains(&"node_api_create_syntax_error"));
    assert!(!expected.contains(&"EXPORTS"));
    for (machine, number) in
      [(MachineTypes::AMD64, 0x8664), (MachineTypes::ARM64, 0xaa64)]
    {
      let lib = node_import_library(machine).unwrap();
      let imports = short_imports(&lib);
      let mut symbols =
        imports.iter().map(|i| i.1.as_str()).collect::<Vec<_>>();
      symbols.sort_unstable();
      let mut want = expected.clone();
      want.sort_unstable();
      assert_eq!(symbols, want);
      for (m, symbol, dll, import_type, name_type) in &imports {
        assert_eq!(*m, number, "{symbol}");
        assert_eq!(dll, "node.exe", "{symbol}");
        // IMPORT_OBJECT_CODE, IMPORT_OBJECT_NAME
        assert_eq!((*import_type, *name_type), (0, 1), "{symbol}");
      }
      // the import descriptor the linker needs for the DLL
      let text = String::from_utf8_lossy(&lib);
      assert!(text.contains("__IMPORT_DESCRIPTOR_node"));
      assert!(text.contains("__NULL_IMPORT_DESCRIPTOR"));
      // deterministic: an unchanged library isn't rewritten
      assert_eq!(node_import_library(machine).unwrap(), lib);
    }

    let dir = tempfile::tempdir().unwrap();
    write_node_libs(dir.path(), MachineTypes::AMD64).unwrap();
    let release = std::fs::read(dir.path().join("Release/node.lib")).unwrap();
    let debug = std::fs::read(dir.path().join("Debug/node.lib")).unwrap();
    assert_eq!(release, debug);
    assert_eq!(release, node_import_library(MachineTypes::AMD64).unwrap());
    // a stale library (another symbol list) is replaced
    std::fs::write(dir.path().join("Release/node.lib"), b"old").unwrap();
    write_node_libs(dir.path(), MachineTypes::AMD64).unwrap();
    assert_eq!(
      std::fs::read(dir.path().join("Release/node.lib")).unwrap(),
      release
    );
    assert_eq!(
      std::fs::read_dir(dir.path().join("Release"))
        .unwrap()
        .count(),
      1
    );
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
