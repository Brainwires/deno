// Copyright 2018-2026 the Deno authors. MIT license.

//! OS registration of the app's deep-link schemes: the startup registration
//! and `Deno.desktop.getSchemeOwner()` / `registerScheme()`.
//!
//! The decisions (never take a scheme another app handles, refresh this
//! app's own stale registration) are `deno_lib`'s
//! [`scheme_handler`](deno_lib::standalone::scheme_handler); this module
//! reads and writes each OS's handler database:
//!
//! - Windows: `HKCU\Software\Classes\<scheme>` (written), `HKLM\…` and the
//!   `UserChoice` (read only; the latter is hash-protected).
//! - macOS: LaunchServices (`LSCopyDefaultHandlerForURLScheme`,
//!   `LSRegisterURL`, and `LSSetDefaultHandlerForURLScheme` only when forced).
//! - Linux: the freedesktop MIME defaults (read directly), an own
//!   `.desktop` entry in `$XDG_DATA_HOME/applications`, `xdg-mime default`
//!   and `update-desktop-database` (absolute paths, no shell, bounded time).
//!
//! The executable registered is always the running process's own.

use std::sync::Arc;
use std::sync::Mutex;

use deno_lib::standalone::scheme_handler::OwnerStatus;
use deno_lib::standalone::scheme_handler::RegisterMode;
use deno_lib::standalone::scheme_handler::RegisterOutcome;
use deno_lib::standalone::scheme_handler::SchemeRegistry;
use deno_lib::standalone::scheme_handler::register_scheme;
use deno_lib::standalone::scheme_handler::resolve_declared_scheme;
use deno_runtime::ops::desktop::DesktopSchemeHandlers;
use deno_runtime::ops::desktop::SchemeOwnerInfo;
use deno_runtime::ops::desktop::SchemeRegisterInfo;

/// The app's deep-link scheme registration.
pub struct SchemeRegistrar {
  /// The normalized `desktop.app.deepLinks`.
  declared: Vec<String>,
  registry: Box<dyn SchemeRegistry + Send + Sync>,
  /// Serializes registrations: the startup pass and explicit calls.
  lock: Mutex<()>,
  /// A development run (`deno desktop --hmr`, a dev server): the running
  /// executable is not the app a link should start, so nothing is written.
  dev_run: bool,
}

impl SchemeRegistrar {
  /// The registrar for this process. `identifier` is `desktop.app.identifier`
  /// and `app_name` the app's display name.
  pub fn new(
    declared: Vec<String>,
    identifier: Option<String>,
    app_name: Option<String>,
    dev_run: bool,
  ) -> Self {
    let registry = os::registry(&declared, identifier, app_name);
    Self::with_registry(declared, registry, dev_run)
  }

  pub fn with_registry(
    declared: Vec<String>,
    registry: Box<dyn SchemeRegistry + Send + Sync>,
    dev_run: bool,
  ) -> Self {
    Self {
      declared,
      registry,
      lock: Mutex::new(()),
      dev_run,
    }
  }

  fn register(&self, scheme: &str, mode: RegisterMode) -> RegisterOutcome {
    let _guard = self
      .lock
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    if self.dev_run {
      return RegisterOutcome {
        registered: false,
        status: self.registry.owner(scheme),
        wrote: false,
        reason: Some(
          "deep-link schemes are not registered in a development run (the \
           running executable is not the packaged app)"
            .to_string(),
        ),
      };
    }
    register_scheme(self.registry.as_ref(), scheme, mode)
  }

  /// Register every declared scheme that no app handles, and refresh this
  /// app's own stale registrations, on a background thread: app start never
  /// waits for it. Schemes another app handles are left alone. Nothing is
  /// written when nothing changed.
  pub fn register_declared_in_background(self: Arc<Self>) {
    if self.declared.is_empty() || self.dev_run {
      return;
    }
    let spawned = std::thread::Builder::new()
      .name("deno-desktop-schemes".to_string())
      .spawn(move || {
        for scheme in &self.declared {
          let out = self.register(scheme, RegisterMode::Startup);
          log_startup_outcome(scheme, &out);
        }
      });
    if let Err(e) = spawned {
      log::debug!("[desktop] deep-link registration thread failed: {e}");
    }
  }
}

fn log_startup_outcome(scheme: &str, out: &RegisterOutcome) {
  let handler = out.status.handler.as_deref().unwrap_or("-");
  match (out.registered, out.wrote) {
    (true, true) => log::debug!(
      "[desktop] registered this app as the {scheme}: handler ({handler})"
    ),
    (true, false) => {
      log::debug!("[desktop] this app already handles {scheme}: ({handler})")
    }
    (false, _) => log::debug!(
      "[desktop] not registering {scheme}: (owner {}, handler {handler}): {}",
      out.status.owner.as_str(),
      out.reason.as_deref().unwrap_or("-")
    ),
  }
}

fn owner_info(status: &OwnerStatus) -> SchemeOwnerInfo {
  SchemeOwnerInfo {
    owner: status.owner.as_str(),
    handler: status.handler.clone(),
  }
}

impl DesktopSchemeHandlers for SchemeRegistrar {
  fn check_scheme(&self, scheme: &str) -> Result<String, String> {
    resolve_declared_scheme(scheme, &self.declared)
  }

  fn scheme_owner(&self, scheme: &str) -> SchemeOwnerInfo {
    owner_info(&self.registry.owner(scheme))
  }

  fn register_scheme(&self, scheme: &str, force: bool) -> SchemeRegisterInfo {
    let mode = if force {
      RegisterMode::Force
    } else {
      RegisterMode::Explicit
    };
    let out = self.register(scheme, mode);
    SchemeRegisterInfo {
      registered: out.registered,
      owner: out.status.owner.as_str(),
      handler: out.status.handler,
      reason: out.reason,
    }
  }
}

/// The app id: `desktop.app.identifier`, else the launch configuration's
/// (`LAUFEY_APP_ID`, or `appId` in the `laufey-launch.json` next to the
/// executable), when it is a valid laufey app id. (macOS identifies the app
/// by its bundle id instead.)
#[cfg(not(target_os = "macos"))]
#[allow(
  clippy::disallowed_methods,
  reason = "reads the launch file next to the executable, outside any runtime sys"
)]
fn resolve_app_id(identifier: Option<String>) -> Option<String> {
  use deno_lib::standalone::app_id::LAUFEY_APP_ID_ENV;
  use deno_lib::standalone::app_id::is_laufey_app_id;
  identifier
    .or_else(|| {
      std::env::var(LAUFEY_APP_ID_ENV)
        .ok()
        .filter(|id| !id.is_empty())
    })
    .or_else(|| {
      let exe = std::env::current_exe().ok()?;
      let text =
        std::fs::read_to_string(exe.parent()?.join("laufey-launch.json"))
          .ok()?;
      let json: serde_json::Value = serde_json::from_str(&text).ok()?;
      json.get("appId")?.as_str().map(str::to_string)
    })
    .filter(|id| is_laufey_app_id(id))
}

#[cfg(windows)]
mod os {
  //! Windows: the URL protocol keys under `Software\Classes`.

  use std::ptr::null;
  use std::ptr::null_mut;

  use deno_lib::standalone::scheme_handler::OwnerStatus;
  use deno_lib::standalone::scheme_handler::SchemeRegistry;
  use deno_lib::standalone::scheme_handler::windows as logic;
  use windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND;
  use windows_sys::Win32::Foundation::ERROR_MORE_DATA;
  use windows_sys::Win32::Foundation::ERROR_SUCCESS;
  use windows_sys::Win32::System::Registry::HKEY;
  use windows_sys::Win32::System::Registry::HKEY_CURRENT_USER;
  use windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE;
  use windows_sys::Win32::System::Registry::KEY_READ;
  use windows_sys::Win32::System::Registry::KEY_WRITE;
  use windows_sys::Win32::System::Registry::REG_OPTION_NON_VOLATILE;
  use windows_sys::Win32::System::Registry::REG_SZ;
  use windows_sys::Win32::System::Registry::RRF_RT_REG_EXPAND_SZ;
  use windows_sys::Win32::System::Registry::RRF_RT_REG_SZ;
  use windows_sys::Win32::System::Registry::RegCloseKey;
  use windows_sys::Win32::System::Registry::RegCreateKeyExW;
  #[cfg(test)]
  use windows_sys::Win32::System::Registry::RegDeleteTreeW;
  use windows_sys::Win32::System::Registry::RegDeleteValueW;
  use windows_sys::Win32::System::Registry::RegGetValueW;
  use windows_sys::Win32::System::Registry::RegOpenKeyExW;
  use windows_sys::Win32::System::Registry::RegQueryValueExW;
  use windows_sys::Win32::System::Registry::RegSetValueExW;

  fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
  }

  /// An open registry key, closed on drop.
  pub(super) struct Key(HKEY);

  impl Drop for Key {
    fn drop(&mut self) {
      // SAFETY: the handle came from RegOpenKeyExW / RegCreateKeyExW and is
      // closed once.
      unsafe {
        RegCloseKey(self.0);
      }
    }
  }

  impl Key {
    pub(super) fn open(root: HKEY, path: &str) -> Option<Key> {
      let path = wide(path);
      let mut key: HKEY = null_mut();
      // SAFETY: `path` is NUL-terminated and `key` is a valid out pointer.
      let status =
        unsafe { RegOpenKeyExW(root, path.as_ptr(), 0, KEY_READ, &mut key) };
      (status == ERROR_SUCCESS).then_some(Key(key))
    }

    pub(super) fn create(root: HKEY, path: &str) -> Result<Key, String> {
      let wpath = wide(path);
      let mut key: HKEY = null_mut();
      // SAFETY: `wpath` is NUL-terminated; the optional pointers are null;
      // `key` is a valid out pointer.
      let status = unsafe {
        RegCreateKeyExW(
          root,
          wpath.as_ptr(),
          0,
          null(),
          REG_OPTION_NON_VOLATILE,
          KEY_READ | KEY_WRITE,
          null(),
          &mut key,
          null_mut(),
        )
      };
      if status == ERROR_SUCCESS {
        Ok(Key(key))
      } else {
        Err(format!(
          "could not create registry key {path} (error {status})"
        ))
      }
    }

    /// A string value (`None` for the key's default value) of `subkey`
    /// (`None` for this key). `REG_EXPAND_SZ` values are expanded.
    pub(super) fn string(
      &self,
      subkey: Option<&str>,
      value: Option<&str>,
    ) -> Option<String> {
      let subkey = subkey.map(wide);
      let value = value.map(wide);
      let subkey_ptr = subkey.as_ref().map_or(null(), |s| s.as_ptr());
      let value_ptr = value.as_ref().map_or(null(), |s| s.as_ptr());
      let mut buf: Vec<u16> = vec![0; 260];
      loop {
        let mut size = (buf.len() * 2) as u32;
        // SAFETY: the name pointers are NUL-terminated or null; `buf` holds
        // `size` bytes.
        let status = unsafe {
          RegGetValueW(
            self.0,
            subkey_ptr,
            value_ptr,
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
          )
        };
        if status == ERROR_MORE_DATA && buf.len() < 32 * 1024 {
          buf = vec![0; (size as usize).div_ceil(2) + 1];
          continue;
        }
        if status != ERROR_SUCCESS {
          return None;
        }
        let len = (size as usize / 2).min(buf.len());
        let text = &buf[..len];
        let text = text.split(|c| *c == 0).next().unwrap_or(&[]);
        return Some(String::from_utf16_lossy(text));
      }
    }

    pub(super) fn has_value(&self, value: &str) -> bool {
      let value = wide(value);
      // SAFETY: `value` is NUL-terminated; no data is read.
      let status = unsafe {
        RegQueryValueExW(
          self.0,
          value.as_ptr(),
          null(),
          null_mut(),
          null_mut(),
          null_mut(),
        )
      };
      status == ERROR_SUCCESS
    }

    pub(super) fn set_string(
      &self,
      value: Option<&str>,
      data: &str,
    ) -> Result<(), String> {
      let name = value.map(wide);
      let data_w = wide(data);
      // SAFETY: `name` is NUL-terminated or null; `data_w` is a
      // NUL-terminated UTF-16 string of the byte length given.
      let status = unsafe {
        RegSetValueExW(
          self.0,
          name.as_ref().map_or(null(), |n| n.as_ptr()),
          0,
          REG_SZ,
          data_w.as_ptr().cast(),
          (data_w.len() * 2) as u32,
        )
      };
      if status == ERROR_SUCCESS {
        Ok(())
      } else {
        Err(format!(
          "could not write registry value {} (error {status})",
          value.unwrap_or("(default)")
        ))
      }
    }

    fn delete_value(&self, value: &str) -> Result<(), String> {
      let name = wide(value);
      // SAFETY: `name` is NUL-terminated.
      let status = unsafe { RegDeleteValueW(self.0, name.as_ptr()) };
      if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
        Ok(())
      } else {
        Err(format!(
          "could not delete registry value {value} (error {status})"
        ))
      }
    }
  }

  /// Delete `HKCU\<path>` and everything under it (tests' cleanup).
  #[cfg(test)]
  pub(super) fn delete_user_tree(path: &str) {
    let path = wide(path);
    // SAFETY: `path` is NUL-terminated.
    unsafe {
      RegDeleteTreeW(HKEY_CURRENT_USER, path.as_ptr());
    }
  }

  const CLASSES: &str = "Software\\Classes";

  fn read_class_key(root: HKEY, scheme: &str) -> Option<logic::ClassKey> {
    let key = Key::open(root, &format!("{CLASSES}\\{scheme}"))?;
    Some(logic::ClassKey {
      command: key.string(Some("shell\\open\\command"), None),
      url_protocol: key.has_value("URL Protocol"),
      default_icon: key.string(Some("DefaultIcon"), None),
      app_id: key.string(None, Some(logic::APP_ID_VALUE)),
    })
  }

  fn read_user_choice(scheme: &str) -> Option<logic::UserChoice> {
    let base = format!(
      "Software\\Microsoft\\Windows\\Shell\\Associations\\UrlAssociations\\{scheme}"
    );
    // Newer Windows 11 builds keep the choice in `UserChoiceLatest`.
    let prog_id =
      ["UserChoiceLatest", "UserChoice"].iter().find_map(|sub| {
        Key::open(HKEY_CURRENT_USER, &format!("{base}\\{sub}"))?
          .string(None, Some("ProgId"))
          .filter(|p| !p.is_empty())
      })?;
    let command_path = format!("{CLASSES}\\{prog_id}\\shell\\open\\command");
    let command = [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE]
      .into_iter()
      .find_map(|root| Key::open(root, &command_path)?.string(None, None));
    Some(logic::UserChoice { prog_id, command })
  }

  pub(super) fn read_state(scheme: &str) -> logic::SchemeState {
    logic::SchemeState {
      user: read_class_key(HKEY_CURRENT_USER, scheme),
      machine: read_class_key(HKEY_LOCAL_MACHINE, scheme),
      user_choice: read_user_choice(scheme),
    }
  }

  /// Write `HKCU\Software\Classes\<scheme>` for `me`.
  pub(super) fn write_user_key(
    scheme: &str,
    me: &logic::ThisApp,
  ) -> Result<(), String> {
    let expected = logic::expected_key(me);
    let key = Key::create(HKEY_CURRENT_USER, &format!("{CLASSES}\\{scheme}"))?;
    key.set_string(None, &logic::key_description(scheme))?;
    key.set_string(Some("URL Protocol"), "")?;
    match &expected.app_id {
      Some(id) => key.set_string(Some(logic::APP_ID_VALUE), id)?,
      None => key.delete_value(logic::APP_ID_VALUE)?,
    }
    if let Some(icon) = &expected.default_icon {
      Key::create(
        HKEY_CURRENT_USER,
        &format!("{CLASSES}\\{scheme}\\DefaultIcon"),
      )?
      .set_string(None, icon)?;
    }
    if let Some(command) = &expected.command {
      Key::create(
        HKEY_CURRENT_USER,
        &format!("{CLASSES}\\{scheme}\\shell\\open\\command"),
      )?
      .set_string(None, command)?;
    }
    notify_association_changed();
    Ok(())
  }

  fn notify_association_changed() {
    use windows_sys::Win32::UI::Shell::SHCNE_ASSOCCHANGED;
    use windows_sys::Win32::UI::Shell::SHCNF_IDLIST;
    use windows_sys::Win32::UI::Shell::SHChangeNotify;
    // SAFETY: SHCNE_ASSOCCHANGED takes no items.
    unsafe {
      SHChangeNotify(SHCNE_ASSOCCHANGED as _, SHCNF_IDLIST, null(), null());
    }
  }

  pub(super) struct WindowsRegistry {
    pub(super) me: Option<logic::ThisApp>,
  }

  impl SchemeRegistry for WindowsRegistry {
    fn owner(&self, scheme: &str) -> OwnerStatus {
      let state = read_state(scheme);
      match &self.me {
        Some(me) => logic::owner(scheme, &state, me),
        // Without its own path the app can't recognize itself.
        None => logic::owner(
          scheme,
          &state,
          &logic::ThisApp {
            exe: String::new(),
            app_id: None,
            icon: String::new(),
          },
        ),
      }
    }

    fn register(&self, scheme: &str, _take_over: bool) -> Result<(), String> {
      let me = self
        .me
        .as_ref()
        .ok_or_else(|| "the app's executable path is unknown".to_string())?;
      write_user_key(scheme, me)
    }
  }

  pub(super) fn this_app(app_id: Option<String>) -> Option<logic::ThisApp> {
    let exe = std::env::current_exe().ok()?;
    let exe = exe.to_str()?.to_string();
    let exe = exe
      .strip_prefix("\\\\?\\")
      .map(str::to_string)
      .unwrap_or(exe);
    let icon = logic::default_icon(&exe);
    Some(logic::ThisApp { exe, app_id, icon })
  }

  pub(super) fn registry(
    _declared: &[String],
    identifier: Option<String>,
    _app_name: Option<String>,
  ) -> Box<dyn SchemeRegistry + Send + Sync> {
    Box::new(WindowsRegistry {
      me: this_app(super::resolve_app_id(identifier)),
    })
  }
}

#[cfg(target_os = "macos")]
mod os {
  //! macOS: LaunchServices.

  use std::ffi::c_char;
  use std::ffi::c_void;

  use deno_lib::standalone::scheme_handler::OwnerStatus;
  use deno_lib::standalone::scheme_handler::SchemeRegistry;
  use deno_lib::standalone::scheme_handler::macos as logic;

  type CFTypeRef = *const c_void;
  type CFStringRef = *const c_void;
  type CFURLRef = *const c_void;
  type CFBundleRef = *const c_void;
  type CFIndex = isize;
  type Boolean = u8;
  type OSStatus = i32;

  const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

  #[link(name = "CoreFoundation", kind = "framework")]
  unsafe extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFStringCreateWithBytes(
      alloc: *const c_void,
      bytes: *const u8,
      num_bytes: CFIndex,
      encoding: u32,
      is_external: Boolean,
    ) -> CFStringRef;
    fn CFStringGetLength(s: CFStringRef) -> CFIndex;
    fn CFStringGetMaximumSizeForEncoding(len: CFIndex, enc: u32) -> CFIndex;
    fn CFStringGetCString(
      s: CFStringRef,
      buf: *mut c_char,
      size: CFIndex,
      enc: u32,
    ) -> Boolean;
    fn CFBundleGetMainBundle() -> CFBundleRef;
    fn CFBundleGetIdentifier(bundle: CFBundleRef) -> CFStringRef;
    fn CFBundleCopyBundleURL(bundle: CFBundleRef) -> CFURLRef;
    fn CFURLGetFileSystemRepresentation(
      url: CFURLRef,
      resolve_against_base: Boolean,
      buf: *mut u8,
      max_len: CFIndex,
    ) -> Boolean;
  }

  #[link(name = "CoreServices", kind = "framework")]
  unsafe extern "C" {
    fn LSCopyDefaultHandlerForURLScheme(scheme: CFStringRef) -> CFStringRef;
    fn LSSetDefaultHandlerForURLScheme(
      scheme: CFStringRef,
      handler_bundle_id: CFStringRef,
    ) -> OSStatus;
    fn LSRegisterURL(url: CFURLRef, update: Boolean) -> OSStatus;
  }

  /// An owned CF object, released on drop.
  struct Owned(CFTypeRef);

  impl Drop for Owned {
    fn drop(&mut self) {
      if !self.0.is_null() {
        // SAFETY: an object this code owns (a Create/Copy result), released
        // once.
        unsafe { CFRelease(self.0) }
      }
    }
  }

  fn cf_string(s: &str) -> Option<Owned> {
    // SAFETY: `s` is valid UTF-8 of the given length.
    let r = unsafe {
      CFStringCreateWithBytes(
        std::ptr::null(),
        s.as_ptr(),
        s.len() as CFIndex,
        K_CF_STRING_ENCODING_UTF8,
        0,
      )
    };
    (!r.is_null()).then_some(Owned(r))
  }

  fn rust_string(s: CFStringRef) -> Option<String> {
    if s.is_null() {
      return None;
    }
    // SAFETY: `s` is a valid CFString; the buffer is as large as the
    // maximum UTF-8 size plus the NUL.
    unsafe {
      let max = CFStringGetMaximumSizeForEncoding(
        CFStringGetLength(s),
        K_CF_STRING_ENCODING_UTF8,
      ) + 1;
      let mut buf = vec![0u8; max.max(1) as usize];
      if CFStringGetCString(
        s,
        buf.as_mut_ptr().cast(),
        buf.len() as CFIndex,
        K_CF_STRING_ENCODING_UTF8,
      ) == 0
      {
        return None;
      }
      let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
      String::from_utf8(buf[..end].to_vec()).ok()
    }
  }

  /// The LaunchServices default handler of `scheme` (a bundle id).
  pub(super) fn default_handler(scheme: &str) -> Option<String> {
    let scheme = cf_string(scheme)?;
    // SAFETY: a valid CFString; the result follows the Copy rule.
    let handler = Owned(unsafe { LSCopyDefaultHandlerForURLScheme(scheme.0) });
    rust_string(handler.0)
  }

  /// The running app bundle: its path (when it is a `.app`) and bundle id.
  fn main_bundle() -> (Option<std::path::PathBuf>, Option<String>) {
    // SAFETY: the main bundle is a Get-rule object owned by CF; the
    // identifier too; the URL follows the Copy rule and is released.
    unsafe {
      let bundle = CFBundleGetMainBundle();
      if bundle.is_null() {
        return (None, None);
      }
      let id = rust_string(CFBundleGetIdentifier(bundle));
      let url = Owned(CFBundleCopyBundleURL(bundle));
      if url.0.is_null() {
        return (None, id);
      }
      let mut buf = vec![0u8; 4096];
      if CFURLGetFileSystemRepresentation(
        url.0,
        1,
        buf.as_mut_ptr(),
        buf.len() as CFIndex,
      ) == 0
      {
        return (None, id);
      }
      let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
      let path = std::path::PathBuf::from(
        String::from_utf8_lossy(&buf[..end]).into_owned(),
      );
      let is_app = path.extension().is_some_and(|e| e == "app");
      (is_app.then_some(path), id)
    }
  }

  pub(super) struct LaunchServicesRegistry {
    /// The running `.app` bundle, if the app runs from one.
    bundle: Option<std::path::PathBuf>,
    /// The bundle's `CFBundleIdentifier`.
    bundle_id: Option<String>,
    /// `desktop.app.identifier`.
    identifier: Option<String>,
  }

  impl SchemeRegistry for LaunchServicesRegistry {
    fn owner(&self, scheme: &str) -> OwnerStatus {
      let ids: Vec<&str> = self
        .bundle_id
        .iter()
        .chain(self.identifier.iter())
        .map(String::as_str)
        .collect();
      logic::owner(default_handler(scheme).as_deref(), &ids)
    }

    fn register(&self, scheme: &str, take_over: bool) -> Result<(), String> {
      let (Some(bundle), Some(bundle_id)) = (&self.bundle, &self.bundle_id)
      else {
        return Err(
          "the app is not running from an app bundle; LaunchServices only \
           registers bundles"
            .to_string(),
        );
      };
      let path = bundle.to_str().ok_or("the bundle path is not UTF-8")?;
      let url_str = cf_string(path).ok_or("CFString")?;
      // SAFETY: CFURLCreateWithFileSystemPath with a valid string; the URL
      // follows the Create rule.
      let url = Owned(unsafe {
        CFURLCreateWithFileSystemPath(std::ptr::null(), url_str.0, 0, 1)
      });
      if url.0.is_null() {
        return Err(format!("could not make a URL of {path}"));
      }
      // SAFETY: a valid file URL.
      let status = unsafe { LSRegisterURL(url.0, 1) };
      if status != 0 {
        return Err(format!("LSRegisterURL failed (OSStatus {status})"));
      }
      if take_over {
        let scheme_cf = cf_string(scheme).ok_or("CFString")?;
        let id_cf = cf_string(bundle_id).ok_or("CFString")?;
        // SAFETY: two valid CFStrings.
        let status =
          unsafe { LSSetDefaultHandlerForURLScheme(scheme_cf.0, id_cf.0) };
        if status != 0 {
          return Err(format!(
            "LSSetDefaultHandlerForURLScheme failed (OSStatus {status})"
          ));
        }
      }
      Ok(())
    }
  }

  #[link(name = "CoreFoundation", kind = "framework")]
  unsafe extern "C" {
    fn CFURLCreateWithFileSystemPath(
      alloc: *const c_void,
      path: CFStringRef,
      style: CFIndex,
      is_directory: Boolean,
    ) -> CFURLRef;
  }

  /// The backend for a given bundle (tests).
  #[cfg(test)]
  pub(super) fn for_bundle(
    bundle: std::path::PathBuf,
    bundle_id: String,
  ) -> LaunchServicesRegistry {
    LaunchServicesRegistry {
      bundle: Some(bundle),
      bundle_id: Some(bundle_id),
      identifier: None,
    }
  }

  pub(super) fn registry(
    _declared: &[String],
    identifier: Option<String>,
    _app_name: Option<String>,
  ) -> Box<dyn SchemeRegistry + Send + Sync> {
    let (bundle, bundle_id) = main_bundle();
    Box::new(LaunchServicesRegistry {
      bundle,
      bundle_id,
      identifier,
    })
  }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod os {
  //! Linux and other freedesktop systems.

  use std::ffi::OsStr;
  use std::io::Read;
  use std::path::Path;
  use std::path::PathBuf;
  use std::time::Duration;
  use std::time::Instant;

  use deno_lib::standalone::scheme_handler::OwnerStatus;
  use deno_lib::standalone::scheme_handler::SchemeRegistry;
  use deno_lib::standalone::scheme_handler::linux as logic;

  /// Where `xdg-mime` and `update-desktop-database` are looked up (never
  /// the inherited `PATH`), and the `PATH` they run with.
  const TOOL_DIRS: &[&str] = &[
    "/usr/local/sbin",
    "/usr/local/bin",
    "/usr/sbin",
    "/usr/bin",
    "/sbin",
    "/bin",
    "/run/current-system/sw/bin",
  ];
  const TOOL_TIMEOUT: Duration = Duration::from_secs(10);
  /// Larger files are not MIME lists or desktop entries.
  const MAX_FILE: u64 = 1024 * 1024;

  #[allow(
    clippy::disallowed_methods,
    reason = "the OS handler database, outside any runtime sys"
  )]
  pub(super) fn read(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut text = String::new();
    file.take(MAX_FILE).read_to_string(&mut text).ok()?;
    Some(text)
  }

  #[allow(
    clippy::disallowed_methods,
    reason = "the OS handler database, outside any runtime sys"
  )]
  fn canonical(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok()
  }

  /// A desktop entry's program as a canonical path: an absolute path, or a
  /// bare name looked up in `PATH` (as the desktop launches it).
  fn resolve_program(program: &str) -> Option<PathBuf> {
    if program.contains('/') {
      let path = Path::new(program);
      return path.is_absolute().then(|| canonical(path)).flatten();
    }
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
      .filter(|dir| dir.is_absolute())
      .find_map(|dir| canonical(&dir.join(program)))
  }

  #[allow(
    clippy::disallowed_methods,
    reason = "locating system tools, outside any runtime sys"
  )]
  fn find_tool(name: &str) -> Option<PathBuf> {
    TOOL_DIRS
      .iter()
      .map(|dir| Path::new(dir).join(name))
      .find(|p| p.is_file())
  }

  /// Run `program` with `args` (no shell, a fixed `PATH`, no stdio), killed
  /// after [`TOOL_TIMEOUT`].
  fn run_tool(program: &Path, args: &[&OsStr]) -> Result<(), String> {
    let name = program.display();
    let mut child = std::process::Command::new(program)
      .args(args)
      .env("PATH", TOOL_DIRS.join(":"))
      .stdin(std::process::Stdio::null())
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::null())
      .spawn()
      .map_err(|e| format!("could not run {name}: {e}"))?;
    let deadline = Instant::now() + TOOL_TIMEOUT;
    loop {
      match child.try_wait() {
        Ok(Some(status)) if status.success() => return Ok(()),
        Ok(Some(status)) => return Err(format!("{name} failed ({status})")),
        Ok(None) if Instant::now() >= deadline => {
          let _ = child.kill();
          let _ = child.wait();
          return Err(format!(
            "{name} did not finish within {}s",
            TOOL_TIMEOUT.as_secs()
          ));
        }
        Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        Err(e) => return Err(format!("{name}: {e}")),
      }
    }
  }

  /// Write `contents` to `path` through a temporary file and a rename, so a
  /// reader never sees half an entry.
  #[allow(
    clippy::disallowed_methods,
    reason = "the OS handler database, outside any runtime sys"
  )]
  fn write_file(path: &Path, contents: &str) -> Result<(), String> {
    let dir = path.parent().ok_or("no parent directory")?;
    std::fs::create_dir_all(dir)
      .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let tmp = dir.join(format!(
      ".{}.{}.tmp",
      path.file_name().and_then(OsStr::to_str).unwrap_or("entry"),
      std::process::id()
    ));
    std::fs::write(&tmp, contents)
      .and_then(|()| std::fs::rename(&tmp, path))
      .map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("could not write {}: {e}", path.display())
      })
  }

  pub(super) struct XdgRegistry {
    pub(super) dirs: Option<logic::XdgDirs>,
    pub(super) me: logic::ThisApp,
    /// The `.deb` / `.rpm` entry installed under the app's own desktop id
    /// that runs this executable ([`logic::package_entry`]): the schemes it
    /// claims are made default without an entry of the app's own, which
    /// would shadow it.
    pub(super) package_entry: Option<PathBuf>,
    pub(super) app_id: Option<String>,
    pub(super) name: String,
    pub(super) declared: Vec<String>,
    /// Look up `xdg-mime` / `update-desktop-database` (tests replace it).
    pub(super) find_tool: fn(&str) -> Option<PathBuf>,
  }

  impl XdgRegistry {
    /// The schemes this app's own entry claims after registering `scheme`:
    /// the declared ones it already claims, plus `scheme`. A scheme is only
    /// listed once the app registers it, so the entry never makes the app
    /// a candidate handler for a scheme another app handles.
    fn entry_schemes(&self, scheme: &str) -> Vec<String> {
      let existing = self
        .me
        .own_entry
        .as_deref()
        .and_then(read)
        .map(|e| logic::entry_schemes(&e))
        .unwrap_or_default();
      self
        .declared
        .iter()
        .filter(|s| *s == scheme || existing.contains(s))
        .cloned()
        .collect()
    }
  }

  impl SchemeRegistry for XdgRegistry {
    fn owner(&self, scheme: &str) -> OwnerStatus {
      let Some(dirs) = &self.dirs else {
        return OwnerStatus::unowned();
      };
      let handler = logic::default_handler(scheme, dirs, &read);
      logic::owner(scheme, handler.as_ref(), &self.me, &read, &resolve_program)
    }

    fn register(&self, scheme: &str, _take_over: bool) -> Result<(), String> {
      let (Some(dirs), Some(app_id), Some(own_entry), Some(desktop_id)) = (
        &self.dirs,
        &self.app_id,
        &self.me.own_entry,
        &self.me.desktop_id,
      ) else {
        return Err(
          "the app has no identifier (desktop.app.identifier), which names \
           its .desktop entry"
            .to_string(),
        );
      };
      // The package's own entry claims the scheme: it only needs to be the
      // default (an entry of the app's own would shadow it).
      let package_claims = self
        .package_entry
        .as_deref()
        .and_then(read)
        .is_some_and(|e| logic::entry_schemes(&e).iter().any(|s| s == scheme));
      if !package_claims {
        let exe = self
          .me
          .exe
          .to_str()
          .ok_or("the executable path is not UTF-8")?;
        let entry = logic::render_entry(
          &self.name,
          app_id,
          exe,
          &self.entry_schemes(scheme),
        )?;
        if read(own_entry).as_deref() != Some(entry.as_str()) {
          write_file(own_entry, &entry)?;
        }
        if let Some(tool) = (self.find_tool)("update-desktop-database") {
          // Only refreshes mimeinfo.cache; the default is set below.
          if let Err(e) =
            run_tool(&tool, &[dirs.user_applications_dir().as_os_str()])
          {
            log::debug!("[desktop] {e}");
          }
        }
      }
      let Some(xdg_mime) = (self.find_tool)("xdg-mime") else {
        return Err(
          "xdg-mime (xdg-utils) was not found; the app's .desktop entry was \
           installed but not made the scheme's default handler"
            .to_string(),
        );
      };
      let mime = logic::scheme_mime_type(scheme);
      run_tool(
        &xdg_mime,
        &[
          OsStr::new("default"),
          OsStr::new(desktop_id),
          OsStr::new(&mime),
        ],
      )
    }
  }

  /// The program a link must start: the AppImage file when running from a
  /// mounted AppImage (`$APPIMAGE`, trusted only while the executable is
  /// inside `$APPDIR`), else the running executable.
  fn this_exe() -> Option<PathBuf> {
    let exe = canonical(&std::env::current_exe().ok()?)?;
    let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from);
    let appdir = std::env::var_os("APPDIR")
      .map(PathBuf::from)
      .and_then(|d| canonical(&d));
    if let (Some(image), Some(appdir)) = (appimage, appdir)
      && image.is_absolute()
      && exe.starts_with(&appdir)
      && let Some(image) = canonical(&image)
    {
      return Some(image);
    }
    Some(exe)
  }

  pub(super) fn registry(
    declared: &[String],
    identifier: Option<String>,
    app_name: Option<String>,
  ) -> Box<dyn SchemeRegistry + Send + Sync> {
    let dirs = logic::XdgDirs::from_env(|k| std::env::var(k).ok());
    let app_id = super::resolve_app_id(identifier);
    let desktop_id = app_id.as_ref().map(|id| format!("{id}.desktop"));
    let own_entry = match (&dirs, &desktop_id) {
      (Some(dirs), Some(id)) => Some(dirs.user_applications_dir().join(id)),
      _ => None,
    };
    let exe = this_exe().unwrap_or_default();
    let name = app_name
      .filter(|n| !n.is_empty())
      .or_else(|| app_id.clone())
      .unwrap_or_else(|| "App".to_string());
    Box::new(XdgRegistry::new(
      dirs,
      logic::ThisApp {
        desktop_id,
        exe,
        own_entry,
      },
      app_id,
      name,
      declared.to_vec(),
      find_tool,
    ))
  }

  impl XdgRegistry {
    /// The registry for `me`. With a package entry under the app's desktop
    /// id ([`logic::package_entry`]), an entry the runtime generated under
    /// that id in the user's data home (an earlier run of the app as a
    /// tarball or AppImage) is removed: it would hide the package's entry
    /// from the menus and from the shell's notification lookup.
    pub(super) fn new(
      dirs: Option<logic::XdgDirs>,
      me: logic::ThisApp,
      app_id: Option<String>,
      name: String,
      declared: Vec<String>,
      find_tool: fn(&str) -> Option<PathBuf>,
    ) -> Self {
      let package_entry = match (&dirs, &me.desktop_id) {
        (Some(dirs), Some(id)) => {
          logic::package_entry(id, dirs, &me.exe, &read, &resolve_program)
        }
        _ => None,
      };
      if package_entry.is_some()
        && let Some(own) = &me.own_entry
        && read(own).is_some_and(|e| logic::is_generated_entry(&e))
      {
        remove_file(own);
      }
      Self {
        dirs,
        me,
        package_entry,
        app_id,
        name,
        declared,
        find_tool,
      }
    }
  }

  #[allow(
    clippy::disallowed_methods,
    reason = "the OS handler database, outside any runtime sys"
  )]
  fn remove_file(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
      log::debug!("[desktop] could not remove {}: {e}", path.display());
    }
  }
}

#[cfg(not(any(unix, windows)))]
mod os {
  use deno_lib::standalone::scheme_handler::OwnerStatus;
  use deno_lib::standalone::scheme_handler::SchemeRegistry;

  struct Unsupported;

  impl SchemeRegistry for Unsupported {
    fn owner(&self, _scheme: &str) -> OwnerStatus {
      OwnerStatus::unowned()
    }
    fn register(&self, _scheme: &str, _take_over: bool) -> Result<(), String> {
      Err("deep-link registration is not supported on this OS".to_string())
    }
  }

  pub(super) fn registry(
    _declared: &[String],
    _identifier: Option<String>,
    _app_name: Option<String>,
  ) -> Box<dyn SchemeRegistry + Send + Sync> {
    Box::new(Unsupported)
  }
}

#[cfg(test)]
#[allow(
  clippy::disallowed_methods,
  reason = "test fixtures on the real filesystem"
)]
mod tests {
  use deno_lib::standalone::scheme_handler::OwnerStatus;
  use deno_lib::standalone::scheme_handler::SchemeOwner;

  use super::*;

  /// A registry that records writes and reports `this` once written.
  struct Recorder {
    initial: OwnerStatus,
    writes: Mutex<Vec<(String, bool)>>,
  }

  impl SchemeRegistry for Recorder {
    fn owner(&self, _scheme: &str) -> OwnerStatus {
      if self.writes.lock().unwrap().is_empty() {
        self.initial.clone()
      } else {
        OwnerStatus::this(Some("me".into()), false)
      }
    }
    fn register(&self, scheme: &str, take_over: bool) -> Result<(), String> {
      self.writes.lock().unwrap().push((scheme.into(), take_over));
      Ok(())
    }
  }

  fn registrar(initial: OwnerStatus, dev_run: bool) -> SchemeRegistrar {
    SchemeRegistrar::with_registry(
      vec!["acme".into()],
      Box::new(Recorder {
        initial,
        writes: Mutex::new(Vec::new()),
      }),
      dev_run,
    )
  }

  #[test]
  fn only_declared_schemes_are_accepted() {
    let r = registrar(OwnerStatus::unowned(), false);
    assert_eq!(r.check_scheme("Acme").unwrap(), "acme");
    assert!(r.check_scheme("other").is_err());
    assert!(r.check_scheme("https").is_err());
  }

  #[test]
  fn explicit_and_forced_registration() {
    let other = OwnerStatus::other(Some("them".into()));
    let r = registrar(other.clone(), false);
    let out = r.register_scheme("acme", false);
    assert!(!out.registered);
    assert_eq!(out.owner, "other");
    assert_eq!(out.handler.as_deref(), Some("them"));
    assert!(out.reason.is_some());
    let out = r.register_scheme("acme", true);
    assert!(out.registered);
    assert_eq!(out.owner, "self");
    assert_eq!(out.reason, None);

    let r = registrar(OwnerStatus::unowned(), false);
    assert_eq!(
      r.scheme_owner("acme"),
      SchemeOwnerInfo {
        owner: "none",
        handler: None
      }
    );
    assert!(r.register_scheme("acme", false).registered);
  }

  #[test]
  fn dev_runs_write_nothing() {
    let r = registrar(OwnerStatus::unowned(), true);
    let out = r.register_scheme("acme", true);
    assert!(!out.registered);
    assert_eq!(out.owner, "none");
    assert!(out.reason.unwrap().contains("development run"));
  }

  #[test]
  fn startup_registration_runs_in_the_background() {
    let r = Arc::new(registrar(OwnerStatus::unowned(), false));
    r.clone().register_declared_in_background();
    for _ in 0..500 {
      if r.scheme_owner("acme").owner == "self" {
        return;
      }
      std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("the startup registration never ran");
  }

  #[test]
  fn startup_registration_leaves_another_app_alone() {
    let r = Arc::new(registrar(OwnerStatus::other(None), false));
    // Run the startup pass inline (same code path as the thread).
    let out = r.register("acme", RegisterMode::Startup);
    assert!(!out.registered && !out.wrote);
    assert_eq!(out.status.owner, SchemeOwner::Other);
  }

  /// Real registry round trip under a throwaway scheme
  /// (`HKCU\Software\Classes\denext-test-<random>`), cleaned up afterwards.
  #[cfg(windows)]
  #[test]
  fn windows_registry_round_trip() {
    use deno_lib::standalone::scheme_handler::RegisterMode;
    use deno_lib::standalone::scheme_handler::register_scheme;
    use deno_lib::standalone::scheme_handler::windows as logic;

    let scheme = format!(
      "denext-test-{:x}{:x}",
      std::process::id(),
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos()
    );
    struct Cleanup(String);
    impl Drop for Cleanup {
      fn drop(&mut self) {
        os::delete_user_tree(&format!("Software\\Classes\\{}", self.0));
      }
    }
    let _cleanup = Cleanup(scheme.clone());

    let me = os::this_app(Some("com.denext.schemetest".into())).unwrap();
    let reg = os::WindowsRegistry {
      me: Some(me.clone()),
    };
    assert_eq!(reg.owner(&scheme).owner, SchemeOwner::Unowned);

    // Unowned: registered at startup.
    let out = register_scheme(&reg, &scheme, RegisterMode::Startup);
    assert!(out.registered && out.wrote, "{out:?}");
    let state = os::read_state(&scheme);
    assert_eq!(state.user, Some(logic::expected_key(&me)));
    // Idempotent: nothing to write the second time.
    let out = register_scheme(&reg, &scheme, RegisterMode::Startup);
    assert!(out.registered && !out.wrote);
    // The registered command ends the options before the link.
    let command = os::read_state(&scheme).user.unwrap().command.unwrap();
    assert!(command.ends_with(" -- \"%1\""), "{command}");

    // A registration in the earlier form (no `--`) is rewritten at startup.
    os::Key::create(
      windows_sys::Win32::System::Registry::HKEY_CURRENT_USER,
      &format!("Software\\Classes\\{scheme}\\shell\\open\\command"),
    )
    .unwrap()
    .set_string(None, &format!("\"{}\" \"%1\"", me.exe))
    .unwrap();
    let out = register_scheme(&reg, &scheme, RegisterMode::Startup);
    assert!(out.registered && out.wrote, "{out:?}");
    assert_eq!(os::read_state(&scheme).user, Some(logic::expected_key(&me)));

    // The app moved: the key names the old path but carries the app id, so
    // it is this app's, stale, and refreshed.
    let moved = os::WindowsRegistry {
      me: Some(logic::ThisApp {
        exe: "C:\\Moved\\App.exe".into(),
        icon: "\"C:\\Moved\\App.exe\",0".into(),
        ..me.clone()
      }),
    };
    let status = moved.owner(&scheme);
    assert_eq!(status.owner, SchemeOwner::This);
    assert!(status.stale);
    let out = register_scheme(&moved, &scheme, RegisterMode::Startup);
    assert!(out.registered && out.wrote);
    assert_eq!(
      os::read_state(&scheme).user.unwrap().command.as_deref(),
      Some("\"C:\\Moved\\App.exe\" -- \"%1\"")
    );

    // Another app's key (no app id): left alone at startup and without
    // force, taken over with force.
    let foreign = os::Key::create(
      windows_sys::Win32::System::Registry::HKEY_CURRENT_USER,
      &format!("Software\\Classes\\{scheme}"),
    )
    .unwrap();
    foreign
      .set_string(Some(logic::APP_ID_VALUE), "com.other.app")
      .unwrap();
    os::Key::create(
      windows_sys::Win32::System::Registry::HKEY_CURRENT_USER,
      &format!("Software\\Classes\\{scheme}\\shell\\open\\command"),
    )
    .unwrap()
    .set_string(None, "\"C:\\Other\\other.exe\" \"%1\"")
    .unwrap();
    drop(foreign);
    let status = reg.owner(&scheme);
    assert_eq!(status.owner, SchemeOwner::Other);
    assert_eq!(status.handler.as_deref(), Some("C:\\Other\\other.exe"));
    for mode in [RegisterMode::Startup, RegisterMode::Explicit] {
      let out = register_scheme(&reg, &scheme, mode);
      assert!(!out.registered && !out.wrote);
    }
    assert_eq!(
      os::read_state(&scheme).user.unwrap().command.as_deref(),
      Some("\"C:\\Other\\other.exe\" \"%1\"")
    );
    let out = register_scheme(&reg, &scheme, RegisterMode::Force);
    assert!(out.registered && out.wrote, "{out:?}");
    assert_eq!(os::read_state(&scheme).user, Some(logic::expected_key(&me)));
  }

  /// Read-only: the `https` handler is a browser, i.e. another app; and a
  /// test binary (not in a bundle) can't register.
  #[cfg(target_os = "macos")]
  #[test]
  fn macos_launch_services_owner() {
    let handler = os::default_handler("https");
    assert!(
      handler.as_deref().is_some_and(|h| h.contains('.')),
      "{handler:?}"
    );
    let reg = os::registry(&[], Some("com.denext.schemetest".into()), None);
    let status = reg.owner("https");
    assert_eq!(status.owner, SchemeOwner::Other);
    assert_eq!(status.handler, handler);
    // A scheme nobody handles.
    let unique = format!("denext-test-{}", std::process::id());
    assert_eq!(reg.owner(&unique).owner, SchemeOwner::Unowned);
    assert!(
      reg
        .register(&unique, false)
        .unwrap_err()
        .contains("not running from an app bundle")
    );
  }

  /// `LSRegisterURL` on a throwaway bundle claiming a unique scheme makes it
  /// the scheme's handler; unregistered with `lsregister -u` afterwards.
  /// Ignored by default because it writes the user's LaunchServices
  /// database (run with `--ignored`). It never forces: forcing would add a
  /// default-handler entry to the user's LaunchServices preferences.
  #[cfg(target_os = "macos")]
  #[test]
  #[ignore = "writes the user's LaunchServices database"]
  fn macos_register_bundle_with_launch_services() {
    const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";
    let unique = format!(
      "denext-test-{}-{}",
      std::process::id(),
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
    );
    let bundle_id = format!("dev.denext.schemetest.p{}", std::process::id());
    // LaunchServices ignores bundles in temporary directories when it picks
    // a default handler, so the bundle lives under the crate instead.
    let tmp = tempfile::Builder::new()
      .prefix(".scheme-test")
      .tempdir_in(env!("CARGO_MANIFEST_DIR"))
      .unwrap();
    let app = tmp.path().join("SchemeTest.app");
    std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    std::fs::write(
      app.join("Contents/Info.plist"),
      format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{bundle_id}</string>
<key>CFBundleName</key><string>SchemeTest</string>
<key>CFBundleExecutable</key><string>SchemeTest</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleURLTypes</key><array><dict>
<key>CFBundleURLName</key><string>{bundle_id}</string>
<key>CFBundleURLSchemes</key><array><string>{unique}</string></array>
</dict></array>
</dict></plist>
"#
      ),
    )
    .unwrap();
    // A real Mach-O executable: LaunchServices doesn't pick a script.
    std::fs::copy("/usr/bin/true", app.join("Contents/MacOS/SchemeTest"))
      .unwrap();
    let app = std::fs::canonicalize(&app).unwrap();
    struct Unregister(std::path::PathBuf);
    impl Drop for Unregister {
      fn drop(&mut self) {
        let _ = std::process::Command::new(LSREGISTER)
          .arg("-u")
          .arg(&self.0)
          .status();
      }
    }
    let _unregister = Unregister(app.clone());

    let reg = os::for_bundle(app.clone(), bundle_id.clone());
    assert_eq!(reg.owner(&unique).owner, SchemeOwner::Unowned);
    let out = register_scheme(&reg, &unique, RegisterMode::Startup);
    assert!(out.registered && out.wrote, "{out:?}");
    assert_eq!(out.status.handler.as_deref(), Some(bundle_id.as_str()));
    // Idempotent.
    let out = register_scheme(&reg, &unique, RegisterMode::Startup);
    assert!(out.registered && !out.wrote);
  }

  /// The freedesktop backend against throwaway XDG directories, without
  /// xdg-utils (the default is set by hand where xdg-mime would).
  #[cfg(all(unix, not(target_os = "macos")))]
  #[test]
  fn xdg_registry_round_trip() {
    use deno_lib::standalone::scheme_handler::RegisterMode;
    use deno_lib::standalone::scheme_handler::linux as logic;
    use deno_lib::standalone::scheme_handler::register_scheme;

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let dirs = logic::XdgDirs {
      config_home: root.join("config"),
      config_dirs: vec![],
      data_home: root.join("data"),
      data_dirs: vec![root.join("sys")],
      current_desktops: vec![],
    };
    let exe = root.join("App/app");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "").unwrap();
    let exe = std::fs::canonicalize(&exe).unwrap();
    let own = dirs.user_applications_dir().join("com.acme.app.desktop");
    let mk = |exe: std::path::PathBuf| {
      os::XdgRegistry::new(
        Some(dirs.clone()),
        logic::ThisApp {
          desktop_id: Some("com.acme.app.desktop".into()),
          exe,
          own_entry: Some(own.clone()),
        },
        Some("com.acme.app".into()),
        "Acme".into(),
        vec!["acme".into(), "acme2".into()],
        |_| None,
      )
    };
    let reg = mk(exe.clone());
    assert_eq!(reg.owner("acme").owner, SchemeOwner::Unowned);

    // No xdg-mime: the entry is installed, the scheme stays unowned, and the
    // reason says why.
    let out = register_scheme(&reg, "acme", RegisterMode::Startup);
    assert!(!out.registered && out.wrote);
    assert!(out.reason.unwrap().contains("xdg-mime"));
    let entry = std::fs::read_to_string(&own).unwrap();
    assert_eq!(logic::entry_schemes(&entry), vec!["acme"]);
    assert_eq!(
      logic::exec_program(&entry).as_deref(),
      Some(exe.to_str().unwrap())
    );

    // What `xdg-mime default` writes.
    let mimeapps = dirs.config_home.join("mimeapps.list");
    std::fs::create_dir_all(&dirs.config_home).unwrap();
    std::fs::write(
      &mimeapps,
      "[Default Applications]\nx-scheme-handler/acme=com.acme.app.desktop\n",
    )
    .unwrap();
    assert_eq!(
      reg.owner("acme"),
      OwnerStatus::this(Some("com.acme.app.desktop".into()), false)
    );

    // The app moved: stale, and the startup pass rewrites the entry.
    let moved_exe = root.join("Moved/app");
    std::fs::create_dir_all(moved_exe.parent().unwrap()).unwrap();
    std::fs::write(&moved_exe, "").unwrap();
    let moved_exe = std::fs::canonicalize(&moved_exe).unwrap();
    let moved = mk(moved_exe.clone());
    assert!(moved.owner("acme").stale);
    let out = register_scheme(&moved, "acme", RegisterMode::Startup);
    assert!(out.registered, "{out:?}");
    assert_eq!(
      logic::exec_program(&std::fs::read_to_string(&own).unwrap()).as_deref(),
      Some(moved_exe.to_str().unwrap())
    );

    // Another app's default: left alone, and the entry doesn't start
    // claiming that scheme.
    std::fs::create_dir_all(root.join("sys/applications")).unwrap();
    std::fs::write(
      root.join("sys/applications/other.desktop"),
      "[Desktop Entry]\nExec=/opt/other %u\n",
    )
    .unwrap();
    std::fs::write(
      &mimeapps,
      "[Default Applications]\nx-scheme-handler/acme=com.acme.app.desktop\n\
       x-scheme-handler/acme2=other.desktop\n",
    )
    .unwrap();
    let status = moved.owner("acme2");
    assert_eq!(status, OwnerStatus::other(Some("other.desktop".into())));
    let out = register_scheme(&moved, "acme2", RegisterMode::Startup);
    assert!(!out.registered && !out.wrote);
    assert_eq!(
      logic::entry_schemes(&std::fs::read_to_string(&own).unwrap()),
      vec!["acme"]
    );

    // A .deb / .rpm installs the app's entry under the same id: the entry
    // this run generated would hide it, so it goes, and the package's entry
    // is the one made default (nothing written for the app itself).
    let package = root.join("sys/applications/com.acme.app.desktop");
    std::fs::write(
      &package,
      format!(
        "[Desktop Entry]\nName=Acme\nExec=env LAUFEY_APP_ID=com.acme.app {} \
         %u\nMimeType=x-scheme-handler/acme;\n",
        moved_exe.display()
      ),
    )
    .unwrap();
    let packaged = mk(moved_exe.clone());
    assert!(!own.exists(), "the generated entry must be removed");
    assert_eq!(
      packaged.owner("acme"),
      OwnerStatus::this(Some("com.acme.app.desktop".into()), false)
    );
    // The default already names the id: registered, nothing written for
    // the app itself.
    let out = register_scheme(&packaged, "acme", RegisterMode::Force);
    assert!(out.registered, "{out:?}");
    assert!(!own.exists());
    // An entry the user wrote under that id is theirs: left alone.
    std::fs::write(&own, "[Desktop Entry]\nName=Mine\nExec=/bin/true\n")
      .unwrap();
    let _ = mk(moved_exe.clone());
    assert!(own.exists());
  }
}
