// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: what a packaged desktop app's kernel-sandbox startup needs from
//! the binary (DENEXT.md item 489): the baked grant as the runtime applies
//! it, and the folders its web engine keeps its data in. The decision and
//! the sandbox itself are `deno_lib::kernel_apply::start_desktop_with`'s;
//! `denort_desktop` calls it when the app's host calls the runtime, before
//! the engine starts.

use std::path::Path;
use std::path::PathBuf;

use deno_lib::kernel_desktop::EngineDirs;
use deno_lib::kernel_sandbox::Engine;
use deno_runtime::deno_permissions::PermissionsOptions;

use crate::binary::StandaloneData;
use crate::run::DesktopTokenDirs;

/// The engine a desktop app runs on: what its host says it is (the host
/// that loaded the runtime knows), else what was baked when it was
/// packaged, else the OS web view.
pub fn engine(data: &StandaloneData, host: Option<Engine>) -> Engine {
  host
    .or(data.metadata.desktop_engine)
    .unwrap_or(Engine::Webview)
}

/// The baked permissions as the runtime applies them to a desktop app
/// (`run::run`): the environment defaults, the `$DENO_DESKTOP_*` path
/// tokens expanded, the embedded programs' run grants and write denies, and
/// the extraction folder of a self-extracting app; intersected with what a
/// confined ancestor handed down. Not the loopback hosts the runtime adds
/// for the app's own servers: the kernel's network rule for a desktop app
/// is `kernel_desktop::desktop_grant`'s.
pub fn baked_options(data: &StandaloneData) -> PermissionsOptions {
  let mut options = data.metadata.permissions.clone();
  let net = (
    options.allow_net.clone(),
    options.allow_net_connect.clone(),
    options.allow_net_listen.clone(),
  );
  crate::run::apply_desktop_permission_defaults(&mut options);
  (
    options.allow_net,
    options.allow_net_connect,
    options.allow_net_listen,
  ) = net;
  let dirs = DesktopTokenDirs::for_app(data.metadata.app_identifier.as_deref());
  // the programs themselves are registered by `run::run`; here only their
  // effect on the grant counts
  let _ = crate::run::take_desktop_embedded_programs(&mut options, &dirs);
  crate::run::expand_desktop_path_tokens(&mut options, &dirs);
  if data.metadata.self_extracting.is_some() {
    crate::run::grant_vfs_read_access(&mut options, &data.root_path);
  }
  let inherited = deno_lib::kernel_inherit::inherited_layers();
  if !inherited.is_empty() {
    #[allow(clippy::disallowed_methods, reason = "the process's real cwd")]
    let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
    deno_lib::kernel_inherit::clamp(&mut options, inherited, &cwd);
    if data.metadata.self_extracting.is_some() {
      crate::run::grant_vfs_read_access(&mut options, &data.root_path);
    }
  }
  options
}

/// The install an executable at `exe` belongs to: the `.app` bundle on
/// macOS (`<App>.app/Contents/MacOS/<exe>`), else the executable's folder.
pub fn install_root(exe: &Path) -> Option<PathBuf> {
  let dir = exe.parent()?;
  if cfg!(target_os = "macos")
    && dir.file_name().is_some_and(|n| n == "MacOS")
    && let Some(contents) = dir.parent()
    && contents.file_name().is_some_and(|n| n == "Contents")
    && let Some(bundle) = contents.parent()
    && bundle.extension().is_some_and(|e| e == "app")
  {
    return Some(bundle.to_path_buf());
  }
  Some(dir.to_path_buf())
}

/// The folders the engine's paths are built from, read from this process
/// (the environment, the executable's place, the app identifier).
#[allow(
  clippy::disallowed_methods,
  reason = "the sandbox acts on the real environment and file system"
)]
pub fn engine_dirs(data: &StandaloneData) -> EngineDirs {
  let app_id = data.metadata.app_identifier.clone();
  let token_dirs = DesktopTokenDirs::for_app(app_id.as_deref());
  let env_dir = |name: &str| {
    std::env::var_os(name)
      .map(PathBuf::from)
      .filter(|p| p.is_absolute())
  };
  let canonical = |p: PathBuf| p.canonicalize().unwrap_or(p);
  let temp = canonical(std::env::temp_dir());
  // macOS: the temp folder is `/private/var/folders/xx/yyyy/T/`; the
  // per-user folder above it holds the cache folder (`C`) too
  let user_folder = (cfg!(target_os = "macos")
    && temp.starts_with("/private/var/folders"))
  .then(|| temp.parent().map(Path::to_path_buf))
  .flatten();
  EngineDirs {
    home: env_dir("HOME").map(canonical),
    // the app's cache folder is `<OS cache>/<id>/denext`; the engine keeps
    // its caches beside it, in `<OS cache>/<id>`
    cache: token_dirs
      .cache
      .as_deref()
      .and_then(Path::parent)
      .map(Path::to_path_buf),
    data: token_dirs.data,
    app_id,
    temp: Some(temp),
    user_folder,
    install: std::env::current_exe()
      .ok()
      .map(canonical)
      .and_then(|exe| install_root(&exe)),
    runtime_dir: env_dir("XDG_RUNTIME_DIR"),
    xauthority: env_dir("XAUTHORITY"),
    config_home: env_dir("XDG_CONFIG_HOME"),
    cache_home: env_dir("XDG_CACHE_HOME"),
    data_home: env_dir("XDG_DATA_HOME"),
  }
}

/// Creates the app's own data and cache folders (the user's alone) before
/// the sandbox applies: the engine makes its profile beneath them, and on
/// Linux a folder that doesn't exist yet can't be granted.
#[allow(
  clippy::disallowed_methods,
  reason = "the sandbox acts on the real file system"
)]
pub fn create_app_dirs(dirs: &EngineDirs) {
  for dir in [&dirs.data, &dirs.cache].into_iter().flatten() {
    if dir.exists() {
      continue;
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
      use std::os::unix::fs::DirBuilderExt;
      builder.mode(0o700);
    }
    let _ = builder.create(dir);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_install_of_an_executable() {
    let folder = install_root(Path::new("/opt/acme/acme")).unwrap();
    assert_eq!(folder, Path::new("/opt/acme"));
    let bundle =
      install_root(Path::new("/Applications/Acme.app/Contents/MacOS/Acme"))
        .unwrap();
    if cfg!(target_os = "macos") {
      assert_eq!(bundle, Path::new("/Applications/Acme.app"));
    } else {
      assert_eq!(bundle, Path::new("/Applications/Acme.app/Contents/MacOS"));
    }
    // a MacOS folder outside a bundle is only a folder
    assert_eq!(
      install_root(Path::new("/x/MacOS/tool")).unwrap(),
      Path::new("/x/MacOS")
    );
    assert_eq!(install_root(Path::new("/")), None);
  }
}
