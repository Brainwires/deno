// Copyright 2018-2026 the Deno authors. MIT license.

//! Where the running app is installed, and the paths the updater uses next
//! to it.
//!
//! | OS      | install (what is swapped)            | executable                    |
//! | ------- | ------------------------------------ | ----------------------------- |
//! | macOS   | `<App>.app` (the bundle directory)   | `Contents/MacOS/<exe>`        |
//! | Windows | the app directory                    | `<App>.exe` in it             |
//! | Linux   | the app directory                    | `<App>` in it                 |
//! | Linux   | an `.AppImage` file (`$APPIMAGE`)    | the AppImage itself           |
//!
//! Everything the updater writes lives NEXT TO the install, in the same
//! directory, so every swap is a same-volume rename:
//!
//! - `.<name>.denext-update/` the staging directory (download + extraction);
//! - `<name>.old` the previous install, kept until the update is confirmed;
//! - `.<name>.denext-update.json` the state file (see `swap.rs`).
//!
//! An install the updater cannot replace is refused up front with
//! `unsupported_layout`: a macOS app running translocated (Gatekeeper's
//! read-only App Translocation mount), or one of `deno desktop --compress`'s
//! self-extracting launchers (the running copy is a per-user cache, not the
//! install).

#![allow(
  clippy::disallowed_methods,
  reason = "the updater inspects and writes next to the app install, \
            outside any user permission sandbox, by design"
)]

use std::path::Path;
use std::path::PathBuf;

use super::error::UpdateError;
use super::error::UpdateErrorCode as Code;
use super::error::err;

/// How the install is shaped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InstallKind {
  /// A macOS `.app` bundle directory.
  MacBundle,
  /// A Windows or Linux app directory holding the executable.
  AppDir,
  /// A Linux AppImage file.
  AppImage,
}

/// The running app's install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallLayout {
  pub kind: InstallKind,
  /// The `.app`, the app directory, or the AppImage file.
  pub install: PathBuf,
  /// The directory holding `install` (where staging, `.old` and the state
  /// file live).
  pub parent: PathBuf,
  /// `install`'s file name.
  pub name: String,
  /// The main executable's path relative to `install` (empty for an
  /// AppImage, which is itself the executable).
  pub exe_rel: PathBuf,
}

impl InstallLayout {
  /// The staging directory.
  pub fn staging_dir(&self) -> PathBuf {
    self.parent.join(format!(".{}.denext-update", self.name))
  }

  /// Where the extracted new app sits inside staging before the swap.
  pub fn extract_dir(&self) -> PathBuf {
    self.staging_dir().join("extract")
  }

  /// The previous install, kept until the update is confirmed.
  pub fn old_path(&self) -> PathBuf {
    self.parent.join(format!("{}.old", self.name))
  }

  /// A failed new install moved aside during a rollback.
  pub fn failed_path(&self) -> PathBuf {
    self.parent.join(format!(".{}.denext-failed", self.name))
  }

  /// The state file.
  pub fn state_path(&self) -> PathBuf {
    self
      .parent
      .join(format!(".{}.denext-update.json", self.name))
  }

  /// The executable inside `root` (an install-shaped path).
  pub fn exe_in(&self, root: &Path) -> PathBuf {
    if self.exe_rel.as_os_str().is_empty() {
      root.to_path_buf()
    } else {
      root.join(&self.exe_rel)
    }
  }

  /// The executable of the installed app.
  pub fn exe(&self) -> PathBuf {
    self.exe_in(&self.install)
  }
}

/// The AppImage runtime's variables, when set.
#[derive(Debug, Clone, Default)]
pub struct AppImageEnv {
  /// `$APPIMAGE`: the AppImage file's path.
  pub appimage: Option<PathBuf>,
  /// `$APPDIR`: where the AppImage is mounted.
  pub appdir: Option<PathBuf>,
}

impl AppImageEnv {
  pub fn from_env() -> Self {
    Self {
      appimage: std::env::var_os("APPIMAGE").map(PathBuf::from),
      appdir: std::env::var_os("APPDIR").map(PathBuf::from),
    }
  }
}

/// Find the install of the app whose executable is `current_exe`
/// (canonicalized here). See the module docs.
pub fn detect_install(
  current_exe: &Path,
  appimage: &AppImageEnv,
) -> Result<InstallLayout, UpdateError> {
  let exe = std::fs::canonicalize(current_exe)
    .map_err(|e| UpdateError::io(current_exe.display(), e))?;
  let exe_name = exe
    .file_name()
    .map(|n| n.to_string_lossy().into_owned())
    .unwrap_or_default();
  if cfg!(target_os = "macos") {
    // <...>/<App>.app/Contents/MacOS/<exe>
    let macos_dir = exe.parent();
    let contents = macos_dir.and_then(|p| p.parent());
    let bundle = contents.and_then(|p| p.parent());
    let (Some(macos_dir), Some(contents), Some(bundle)) =
      (macos_dir, contents, bundle)
    else {
      return err(Code::UnsupportedLayout, "the app is not in a .app bundle");
    };
    if macos_dir.file_name().is_none_or(|n| n != "MacOS")
      || contents.file_name().is_none_or(|n| n != "Contents")
      || bundle.extension().is_none_or(|e| e != "app")
    {
      return err(
        Code::UnsupportedLayout,
        format!("{} is not inside a .app bundle", exe.display()),
      );
    }
    let s = bundle.to_string_lossy();
    if s.contains("/AppTranslocation/") {
      return err(
        Code::UnsupportedLayout,
        "the app runs translocated (macOS App Translocation): move it to \
         /Applications (or another folder) and launch it from there to \
         enable updates",
      );
    }
    if s.contains("/Library/Application Support/") {
      return err(
        Code::UnsupportedLayout,
        "the app runs from a self-extracting launcher's cache (deno desktop \
         --compress); full-app updates need an uncompressed bundle",
      );
    }
    return layout(
      InstallKind::MacBundle,
      bundle,
      PathBuf::from("Contents").join("MacOS").join(&exe_name),
    );
  }

  if cfg!(target_os = "linux")
    && let (Some(image), Some(appdir)) = (&appimage.appimage, &appimage.appdir)
  {
    // Only when THIS process runs from the AppImage's mount: `$APPIMAGE`
    // alone is inherited by anything an AppImage app launches.
    let appdir = std::fs::canonicalize(appdir).unwrap_or(appdir.clone());
    if exe.starts_with(&appdir) {
      let image = std::fs::canonicalize(image)
        .map_err(|e| UpdateError::io(image.display(), e))?;
      return layout(InstallKind::AppImage, &image, PathBuf::new());
    }
  }

  let Some(dir) = exe.parent() else {
    return err(Code::UnsupportedLayout, "the executable has no directory");
  };
  if is_self_extract_cache(dir) {
    return err(
      Code::UnsupportedLayout,
      "the app runs from a self-extracting launcher's cache (deno desktop \
       --compress); full-app updates need an uncompressed app directory",
    );
  }
  layout(InstallKind::AppDir, dir, PathBuf::from(&exe_name))
}

fn layout(
  kind: InstallKind,
  install: &Path,
  exe_rel: PathBuf,
) -> Result<InstallLayout, UpdateError> {
  let (Some(parent), Some(name)) = (install.parent(), install.file_name())
  else {
    return err(Code::UnsupportedLayout, "the install has no parent");
  };
  Ok(InstallLayout {
    kind,
    install: install.to_path_buf(),
    parent: parent.to_path_buf(),
    name: name.to_string_lossy().into_owned(),
    exe_rel,
  })
}

/// `deno desktop --compress` extracts to `<data>/com.deno.desktop.<app>/<hash>/<App>`.
fn is_self_extract_cache(dir: &Path) -> bool {
  let hash = dir.parent();
  let id = hash.and_then(|h| h.parent());
  let hash_ok = hash
    .and_then(|h| h.file_name())
    .map(|n| n.to_string_lossy().into_owned())
    .is_some_and(|n| n.len() >= 8 && n.bytes().all(|b| b.is_ascii_hexdigit()));
  let id_ok = id
    .and_then(|i| i.file_name())
    .is_some_and(|n| n.to_string_lossy().starts_with("com.deno.desktop."));
  hash_ok && id_ok
}

/// The Rust target triple this runtime was built for.
pub fn target_triple() -> Option<&'static str> {
  match (std::env::consts::OS, std::env::consts::ARCH) {
    ("macos", "aarch64") => Some("aarch64-apple-darwin"),
    ("macos", "x86_64") => Some("x86_64-apple-darwin"),
    ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
    ("windows", "aarch64") => Some("aarch64-pc-windows-msvc"),
    ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
    ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
    _ => None,
  }
}

/// The webview backend this app ships, judged from the running executable:
/// `cef` when the Chromium Embedded Framework sits next to it (the bundle's
/// `Contents/Frameworks/` on macOS, the executable's directory elsewhere, an
/// AppImage's mount included), else `webview` (the system webview).
pub fn detect_backend(exe: &Path) -> &'static str {
  let Some(dir) = exe.parent() else {
    return "webview";
  };
  let cef = if cfg!(target_os = "macos") {
    dir
      .join("../Frameworks/Chromium Embedded Framework.framework")
      .exists()
  } else {
    dir.join("libcef.dll").exists() || dir.join("libcef.so").exists()
  };
  if cef { "cef" } else { "webview" }
}

/// The manifest's platform key for this build: `<target>-<backend>`.
pub fn platform_key(backend: &str) -> Option<String> {
  target_triple().map(|t| format!("{t}-{backend}"))
}

/// Refuse with `install_not_writable` unless this user can create the
/// staging directory and the `.old` next to the install, and (for a
/// directory install) delete the install's contents once it is `.old`.
/// Nothing is escalated: an install in a root-owned location (a `.pkg` in
/// `/Applications`, an MSI in Program Files, a `.deb`) must be updated by
/// its installer.
pub fn check_writable(layout: &InstallLayout) -> Result<(), UpdateError> {
  let probe = layout.parent.join(format!(
    ".{}.denext-probe-{}",
    layout.name,
    std::process::id()
  ));
  let parent_ok = std::fs::OpenOptions::new()
    .write(true)
    .create_new(true)
    .open(&probe)
    .map(|_| {
      let _ = std::fs::remove_file(&probe);
    })
    .is_ok();
  if !parent_ok {
    return err(
      Code::InstallNotWritable,
      format!(
        "{} is not writable by this user: the app cannot replace itself \
         there. Update it with its installer, or install it in a \
         user-writable location",
        layout.parent.display()
      ),
    );
  }
  if layout.kind != InstallKind::AppImage && !dir_writable(&layout.install) {
    return err(
      Code::InstallNotWritable,
      format!(
        "{} is not writable by this user (installed by another user or an \
         installer)",
        layout.install.display()
      ),
    );
  }
  Ok(())
}

#[cfg(unix)]
fn dir_writable(dir: &Path) -> bool {
  use std::os::unix::ffi::OsStrExt;
  let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
    return false;
  };
  // SAFETY: a valid NUL-terminated path.
  unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

#[cfg(not(unix))]
fn dir_writable(dir: &Path) -> bool {
  let probe = dir.join(format!(".denext-probe-{}", std::process::id()));
  std::fs::OpenOptions::new()
    .write(true)
    .create_new(true)
    .open(&probe)
    .map(|_| {
      let _ = std::fs::remove_file(&probe);
    })
    .is_ok()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn sibling_paths() {
    let l = InstallLayout {
      kind: InstallKind::MacBundle,
      install: PathBuf::from("/Applications/My App.app"),
      parent: PathBuf::from("/Applications"),
      name: "My App.app".into(),
      exe_rel: PathBuf::from("Contents/MacOS/my"),
    };
    assert_eq!(l.old_path(), PathBuf::from("/Applications/My App.app.old"));
    assert_eq!(
      l.staging_dir(),
      PathBuf::from("/Applications/.My App.app.denext-update")
    );
    assert_eq!(
      l.state_path(),
      PathBuf::from("/Applications/.My App.app.denext-update.json")
    );
    assert_eq!(
      l.exe(),
      PathBuf::from("/Applications/My App.app/Contents/MacOS/my")
    );
  }

  #[test]
  fn detects_self_extract_cache() {
    assert!(is_self_extract_cache(Path::new(
      "/home/u/.local/share/com.deno.desktop.app/0123abcd4567ef89/App"
    )));
    assert!(!is_self_extract_cache(Path::new("/home/u/Apps/App")));
    assert!(!is_self_extract_cache(Path::new(
      "/home/u/.local/share/other/0123abcd4567ef89/App"
    )));
  }

  #[test]
  fn detects_the_running_test_binary() {
    // The test binary is not in a .app on macOS; elsewhere its directory is
    // an app dir.
    let exe = std::env::current_exe().unwrap();
    let r = detect_install(&exe, &AppImageEnv::default());
    if cfg!(target_os = "macos") {
      assert_eq!(r.unwrap_err().code, Code::UnsupportedLayout);
    } else {
      let l = r.unwrap();
      assert_eq!(l.kind, InstallKind::AppDir);
      assert_eq!(l.exe(), std::fs::canonicalize(&exe).unwrap());
    }
  }

  #[cfg(target_os = "macos")]
  #[test]
  fn detects_a_bundle_and_refuses_translocation() {
    let t = tempfile::tempdir().unwrap();
    let exe = t.path().join("A.app/Contents/MacOS/a");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, b"").unwrap();
    let l = detect_install(&exe, &AppImageEnv::default()).unwrap();
    assert_eq!(l.kind, InstallKind::MacBundle);
    assert_eq!(l.name, "A.app");
    assert_eq!(l.exe_rel, PathBuf::from("Contents/MacOS/a"));
    let exe = t.path().join("AppTranslocation/x/d/B.app/Contents/MacOS/b");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, b"").unwrap();
    let e = detect_install(&exe, &AppImageEnv::default()).unwrap_err();
    assert_eq!(e.code, Code::UnsupportedLayout);
  }

  #[cfg(target_os = "linux")]
  #[test]
  fn appimage_only_when_running_from_its_mount() {
    let t = tempfile::tempdir().unwrap();
    let image = t.path().join("App.AppImage");
    std::fs::write(&image, b"").unwrap();
    let mount = t.path().join("mount");
    std::fs::create_dir_all(&mount).unwrap();
    let exe = mount.join("app");
    std::fs::write(&exe, b"").unwrap();
    let env = AppImageEnv {
      appimage: Some(image.clone()),
      appdir: Some(mount.clone()),
    };
    let l = detect_install(&exe, &env).unwrap();
    assert_eq!(l.kind, InstallKind::AppImage);
    assert_eq!(l.install, std::fs::canonicalize(&image).unwrap());
    // An inherited $APPIMAGE (this exe is not under $APPDIR) is ignored.
    let other = t.path().join("other/app");
    std::fs::create_dir_all(other.parent().unwrap()).unwrap();
    std::fs::write(&other, b"").unwrap();
    assert_eq!(
      detect_install(&other, &env).unwrap().kind,
      InstallKind::AppDir
    );
  }

  #[cfg(unix)]
  #[test]
  fn read_only_parent_is_not_writable() {
    use std::os::unix::fs::PermissionsExt;
    // Root ignores mode bits; nothing to test then.
    // SAFETY: getuid has no preconditions.
    if unsafe { libc::getuid() } == 0 {
      return;
    }
    let t = tempfile::tempdir().unwrap();
    let parent = t.path().join("ro");
    let install = parent.join("App");
    std::fs::create_dir_all(&install).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555))
      .unwrap();
    let l = InstallLayout {
      kind: InstallKind::AppDir,
      install: install.clone(),
      parent: parent.clone(),
      name: "App".into(),
      exe_rel: PathBuf::from("App"),
    };
    let e = check_writable(&l).unwrap_err();
    assert_eq!(e.code, Code::InstallNotWritable);
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755))
      .unwrap();
    std::fs::set_permissions(&install, std::fs::Permissions::from_mode(0o555))
      .unwrap();
    assert_eq!(
      check_writable(&l).unwrap_err().code,
      Code::InstallNotWritable
    );
    std::fs::set_permissions(&install, std::fs::Permissions::from_mode(0o755))
      .unwrap();
    assert!(check_writable(&l).is_ok());
  }
}
