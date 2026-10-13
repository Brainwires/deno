// Copyright 2018-2026 the Deno authors. MIT license.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use deno_core::anyhow;
use deno_core::anyhow::Context;
use deno_core::error::AnyError;
use deno_npm::registry::NpmRegistryApi;
use deno_npm_cache::TarballCache;
use deno_npmrc::ResolvedNpmRc;
use deno_resolver::workspace::WorkspaceNpmLinkPackagesRc;
use deno_semver::package::PackageNv;

use crate::cache::DenoDir;
use crate::npm::CliNpmCache;
use crate::npm::CliNpmCacheHttpClient;
use crate::npm::CliNpmRegistryInfoProvider;
use crate::sys::CliSys;

pub const ESBUILD_VERSION: &str = "0.25.5";
// Bump when helper acquisition semantics change so an existing executable is
// reacquired under the new rules.
const ESBUILD_CACHE_VERSION: u8 = 1;

fn esbuild_platform() -> &'static str {
  match (std::env::consts::ARCH, std::env::consts::OS) {
    ("x86_64", "linux") => "linux-x64",
    ("aarch64", "linux") => "linux-arm64",
    ("x86_64", "macos" | "apple") => "darwin-x64",
    ("aarch64", "macos" | "apple") => "darwin-arm64",
    ("x86_64", "windows") => "win32-x64",
    ("aarch64", "windows") => "win32-arm64",
    ("x86_64", "android") => "android-x64",
    ("aarch64", "android") => "android-arm64",
    _ => panic!(
      "Unsupported platform: {} {}",
      std::env::consts::ARCH,
      std::env::consts::OS
    ),
  }
}

/// Environment variable naming an esbuild executable to use instead of the
/// one next to `deno` or the one downloaded into `DENO_DIR` (denext).
pub const DENO_ESBUILD_BINARY_ENV: &str = "DENO_ESBUILD_BINARY";

fn esbuild_exe_name() -> &'static str {
  if cfg!(windows) {
    "esbuild.exe"
  } else {
    "esbuild"
  }
}

/// What `<esbuild> --version` prints, or `None` when it can't be run.
fn esbuild_version_of(path: &Path) -> Option<String> {
  let output = std::process::Command::new(path)
    .arg("--version")
    .stdin(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .output()
    .ok()?;
  if !output.status.success() {
    return None;
  }
  Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// An esbuild that needs no download (denext): the `DENO_ESBUILD_BINARY`
/// override, else an `esbuild[.exe]` next to the running `deno` executable
/// (denext's CLI archives ship one) when it reports exactly
/// `ESBUILD_VERSION`. A sibling of any other version is ignored, so the
/// download path below runs as before.
fn local_esbuild(
  override_path: Option<std::ffi::OsString>,
  current_exe: Option<PathBuf>,
) -> Result<Option<PathBuf>, AnyError> {
  if let Some(path) = override_path.filter(|p| !p.is_empty()) {
    let path = PathBuf::from(path);
    if !path.is_file() {
      anyhow::bail!(
        "{} is set to {}, which is not a file",
        DENO_ESBUILD_BINARY_ENV,
        path.display()
      );
    }
    match esbuild_version_of(&path) {
      Some(v) if v == ESBUILD_VERSION => {}
      other => log::warn!(
        "{} {} reports version {}, expected {}; using it anyway",
        DENO_ESBUILD_BINARY_ENV,
        path.display(),
        other.as_deref().unwrap_or("<unknown>"),
        ESBUILD_VERSION
      ),
    }
    return Ok(Some(path));
  }
  let Some(sibling) = current_exe
    .as_deref()
    .and_then(Path::parent)
    .map(|dir| dir.join(esbuild_exe_name()))
  else {
    return Ok(None);
  };
  if !sibling.is_file() {
    return Ok(None);
  }
  match esbuild_version_of(&sibling) {
    Some(v) if v == ESBUILD_VERSION => {
      log::debug!("using the esbuild next to deno: {}", sibling.display());
      Ok(Some(sibling))
    }
    other => {
      log::debug!(
        "ignoring {}: version {}, expected {}",
        sibling.display(),
        other.as_deref().unwrap_or("<unknown>"),
        ESBUILD_VERSION
      );
      Ok(None)
    }
  }
}

pub async fn ensure_esbuild(
  deno_dir: &DenoDir,
  npmrc: &ResolvedNpmRc,
  api: &Arc<CliNpmRegistryInfoProvider>,
  workspace_link_packages: &WorkspaceNpmLinkPackagesRc,
  tarball_cache: &Arc<TarballCache<CliNpmCacheHttpClient, CliSys>>,
  npm_cache: &CliNpmCache,
) -> Result<PathBuf, AnyError> {
  if let Some(path) = local_esbuild(
    std::env::var_os(DENO_ESBUILD_BINARY_ENV),
    std::env::current_exe().ok(),
  )? {
    return Ok(path);
  }

  let target = esbuild_platform();
  let mut esbuild_path = deno_dir
    .dl_folder_path()
    .join(format!(
      "esbuild-{}-{}",
      ESBUILD_VERSION, ESBUILD_CACHE_VERSION
    ))
    .join(format!("esbuild-{}", target));
  if cfg!(windows) {
    esbuild_path.set_extension("exe");
  }

  if esbuild_path.exists() {
    return Ok(esbuild_path);
  }

  let pkg_name = format!("@esbuild/{}", target);
  let nv =
    PackageNv::from_str(&format!("{}@{}", pkg_name, ESBUILD_VERSION)).unwrap();
  let mut info = api.package_info(&pkg_name).await?;
  let version_info = match info.version_info(&nv, &workspace_link_packages.0) {
    Ok(version_info) => version_info,
    Err(_) => {
      api.mark_force_reload();
      info = api.package_info(&pkg_name).await?;
      info.version_info(&nv, &workspace_link_packages.0)?
    }
  };
  if let Some(dist) = &version_info.dist {
    let registry_url = npmrc.get_registry_url(&nv.name);
    let package_folder =
      npm_cache.package_folder_for_nv_and_url(&nv, registry_url);
    let existed = package_folder.exists();

    if !existed {
      tarball_cache
        .ensure_package(&nv, dist)
        .await
        .with_context(|| {
          format!(
            "failed to download esbuild package tarball {} from {}",
            nv, dist.tarball
          )
        })?;
    }

    let path = if cfg!(windows) {
      package_folder.join("esbuild.exe")
    } else {
      package_folder.join("bin").join("esbuild")
    };

    let esbuild_dir = esbuild_path.parent().unwrap();
    std::fs::create_dir_all(esbuild_dir).with_context(|| {
      format!("failed to create directory {}", esbuild_dir.display())
    })?;
    // Install the binary atomically: copy to a temporary file in the same
    // directory and then rename it into place. The rename is atomic, so a
    // concurrent `deno bundle` process never observes (via the `exists()`
    // check above) and tries to execute a half-written binary, which would
    // otherwise fail with ETXTBSY ("text file busy") on Linux.
    let tmp_path =
      esbuild_dir.join(format!(".esbuild-{}.tmp", std::process::id()));
    std::fs::copy(&path, &tmp_path).with_context(|| {
      format!(
        "failed to copy esbuild binary from {} to {}",
        path.display(),
        tmp_path.display()
      )
    })?;
    match std::fs::rename(&tmp_path, &esbuild_path) {
      Ok(()) => {}
      // Another process won the race and installed the binary already; our
      // copy is redundant, so just discard it.
      Err(_) if esbuild_path.exists() => {
        let _ = std::fs::remove_file(&tmp_path);
      }
      Err(err) => {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err).with_context(|| {
          format!(
            "failed to move esbuild binary into place at {}",
            esbuild_path.display()
          )
        });
      }
    }

    if !existed {
      let _ = std::fs::remove_dir_all(&package_folder).inspect_err(|e| {
        log::warn!(
          "failed to remove directory {}: {}",
          package_folder.display(),
          e
        );
      });
    }
    Ok(esbuild_path)
  } else {
    anyhow::bail!(
      "could not get fetch esbuild binary; download it manually and copy it to {}",
      esbuild_path.display()
    );
  }
}

#[cfg(all(test, unix))]
mod tests {
  use std::os::unix::fs::PermissionsExt;

  use super::*;

  /// A stand-in esbuild that prints `version` for `--version`.
  fn fake_esbuild(dir: &Path, version: &str) -> PathBuf {
    let path = dir.join("esbuild");
    std::fs::write(&path, format!("#!/bin/sh\necho {version}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
      .unwrap();
    path
  }

  #[test]
  fn sibling_of_the_pinned_version_is_used() {
    let dir = tempfile::tempdir().unwrap();
    let esbuild = fake_esbuild(dir.path(), ESBUILD_VERSION);
    let found = local_esbuild(None, Some(dir.path().join("deno"))).unwrap();
    assert_eq!(found, Some(esbuild));
  }

  #[test]
  fn sibling_of_another_version_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    fake_esbuild(dir.path(), "0.0.1");
    let found = local_esbuild(None, Some(dir.path().join("deno"))).unwrap();
    assert_eq!(found, None);
  }

  #[test]
  fn no_sibling_falls_back_to_the_download() {
    let dir = tempfile::tempdir().unwrap();
    let found = local_esbuild(None, Some(dir.path().join("deno"))).unwrap();
    assert_eq!(found, None);
    assert_eq!(local_esbuild(None, None).unwrap(), None);
  }

  #[test]
  fn override_wins_over_the_sibling() {
    let sibling_dir = tempfile::tempdir().unwrap();
    fake_esbuild(sibling_dir.path(), ESBUILD_VERSION);
    let override_dir = tempfile::tempdir().unwrap();
    // Another version: an explicit override is used anyway (with a warning).
    let esbuild = fake_esbuild(override_dir.path(), "0.0.1");
    let found = local_esbuild(
      Some(esbuild.clone().into_os_string()),
      Some(sibling_dir.path().join("deno")),
    )
    .unwrap();
    assert_eq!(found, Some(esbuild));
  }

  #[test]
  fn override_that_is_not_a_file_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope");
    let err = local_esbuild(Some(missing.into_os_string()), None).unwrap_err();
    assert!(err.to_string().contains(DENO_ESBUILD_BINARY_ENV), "{err}");
  }

  #[test]
  fn empty_override_is_unset() {
    let dir = tempfile::tempdir().unwrap();
    let found =
      local_esbuild(Some("".into()), Some(dir.path().join("deno"))).unwrap();
    assert_eq!(found, None);
  }
}
