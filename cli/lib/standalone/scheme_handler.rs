// Copyright 2018-2026 the Deno authors. MIT license.

//! Who handles a deep-link URL scheme, and when a `deno desktop` app may
//! make itself the handler.
//!
//! An app declares its deep-link schemes in `deno.json`
//! (`desktop.app.deepLinks`). The packager writes the declarative part of the
//! registration (macOS `CFBundleURLTypes`, the Linux `.desktop` `MimeType`),
//! but the OS routes a scheme to whichever app it considers the handler, and
//! another app may already be that. A callback URL delivered to the wrong app
//! is RFC 8252 §8.6 scheme hijacking, so the runtime:
//!
//! - never silently takes a scheme another app handles ([`plan_registration`]
//!   only writes for a scheme nobody handles, or one this app already does);
//! - tells the app who the handler is ([`OwnerStatus`]), so it can fall back
//!   to a loopback redirect or ask the user;
//! - takes a scheme over only when the app asks with `force`, which the API
//!   documents as "on an explicit user action".
//!
//! The OS offers no protection against another program of the same user
//! re-registering the scheme at any time, so a registration is never proof
//! that a callback reaches this app: PKCE and the owner check stay necessary.
//!
//! The decisions here are platform neutral and pure. The per-OS state
//! (Windows registry values, the LaunchServices default, the freedesktop
//! MIME defaults) is read by `denort` and turned into an [`OwnerStatus`] with
//! the helpers in [`windows`], [`macos`] and [`linux`], which only take
//! values and callbacks so they can be tested on any host.

use std::path::Path;
use std::path::PathBuf;

use super::launch_args::validate_deep_link_scheme;

/// Who handles a scheme, from this app's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemeOwner {
  /// This app (this executable or bundle, or this app id after a move).
  This,
  /// Another app.
  Other,
  /// Nobody: opening a link with the scheme finds no handler.
  Unowned,
}

impl SchemeOwner {
  /// The value `Deno.desktop.getSchemeOwner()` reports.
  pub fn as_str(self) -> &'static str {
    match self {
      SchemeOwner::This => "self",
      SchemeOwner::Other => "other",
      SchemeOwner::Unowned => "none",
    }
  }
}

/// The OS's current handler for a scheme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerStatus {
  pub owner: SchemeOwner,
  /// What identifies the current handler, for display: an executable path
  /// (Windows), a bundle id (macOS), a `.desktop` file id (Linux).
  pub handler: Option<String>,
  /// The handler is this app, but its registration is out of date (the
  /// executable moved, or a value this app writes is missing) and is
  /// refreshed when the app registers.
  pub stale: bool,
  /// A note on how the owner was decided, for a result that would otherwise
  /// surprise (a Windows UserChoice that overrides the registered handler).
  pub reason: Option<String>,
}

impl OwnerStatus {
  pub fn unowned() -> Self {
    Self {
      owner: SchemeOwner::Unowned,
      handler: None,
      stale: false,
      reason: None,
    }
  }

  pub fn this(handler: Option<String>, stale: bool) -> Self {
    Self {
      owner: SchemeOwner::This,
      handler,
      stale,
      reason: None,
    }
  }

  pub fn other(handler: Option<String>) -> Self {
    Self {
      owner: SchemeOwner::Other,
      handler,
      stale: false,
      reason: None,
    }
  }

  fn with_reason(mut self, reason: impl Into<String>) -> Self {
    self.reason = Some(reason.into());
    self
  }
}

/// Why the app registers a scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterMode {
  /// The runtime at app start, for every declared scheme.
  Startup,
  /// `Deno.desktop.registerScheme(scheme)`.
  Explicit,
  /// `Deno.desktop.registerScheme(scheme, { force: true })`: take the scheme
  /// over from another app. Only on an explicit user action.
  Force,
}

/// What [`register_scheme`] does for a scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterAction {
  /// Nothing to do: the registration is this app's and current.
  Keep,
  /// Write (or refresh) this app's registration.
  Write,
  /// Another app handles the scheme; leave it alone.
  LeaveToOther,
}

/// Decide what to do for a scheme whose handler is `status`: register when
/// nobody handles it, refresh this app's own stale registration, and take a
/// scheme from another app only in [`RegisterMode::Force`].
pub fn plan_registration(
  status: &OwnerStatus,
  mode: RegisterMode,
) -> RegisterAction {
  match status.owner {
    SchemeOwner::Unowned => RegisterAction::Write,
    SchemeOwner::This if status.stale => RegisterAction::Write,
    SchemeOwner::This => RegisterAction::Keep,
    SchemeOwner::Other if mode == RegisterMode::Force => RegisterAction::Write,
    SchemeOwner::Other => RegisterAction::LeaveToOther,
  }
}

/// Reads and writes one OS's scheme registration for this app.
pub trait SchemeRegistry {
  /// The current handler of `scheme` (normalized and declared).
  fn owner(&self, scheme: &str) -> OwnerStatus;
  /// Make this app the handler of `scheme`, or refresh its registration.
  /// `take_over` is set when another app handles it and the app forced the
  /// registration: only then may a backend change a user-visible default
  /// (macOS `LSSetDefaultHandlerForURLScheme`).
  fn register(&self, scheme: &str, take_over: bool) -> Result<(), String>;
}

/// The result of [`register_scheme`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterOutcome {
  /// Whether this app handles the scheme afterwards.
  pub registered: bool,
  /// The handler afterwards.
  pub status: OwnerStatus,
  /// Whether anything was written.
  pub wrote: bool,
  /// Why the app does not handle the scheme, when it doesn't.
  pub reason: Option<String>,
}

/// Register this app for `scheme` as [`plan_registration`] decides, and
/// report the handler the OS sees afterwards (a write can succeed and still
/// not make the app the handler, e.g. under a Windows UserChoice).
pub fn register_scheme(
  registry: &dyn SchemeRegistry,
  scheme: &str,
  mode: RegisterMode,
) -> RegisterOutcome {
  let before = registry.owner(scheme);
  match plan_registration(&before, mode) {
    RegisterAction::Keep => RegisterOutcome {
      registered: true,
      reason: None,
      status: before,
      wrote: false,
    },
    RegisterAction::LeaveToOther => RegisterOutcome {
      registered: false,
      reason: Some(match (&before.handler, &before.reason) {
        (Some(handler), Some(why)) => {
          format!("another app handles {scheme}: ({handler}): {why}")
        }
        (Some(handler), None) => format!(
          "another app handles {scheme}: ({handler}); it is left as the \
           handler unless the registration is forced"
        ),
        (None, _) => format!(
          "another app handles {scheme}:; it is left as the handler unless \
           the registration is forced"
        ),
      }),
      status: before,
      wrote: false,
    },
    RegisterAction::Write => {
      let take_over = before.owner == SchemeOwner::Other;
      let written = registry.register(scheme, take_over);
      let after = registry.owner(scheme);
      let registered = after.owner == SchemeOwner::This;
      // A failed step doesn't matter if the OS routes the scheme here anyway
      // (e.g. Linux without xdg-mime, once the entry is in mimeinfo.cache).
      let reason = match written {
        _ if registered => None,
        Err(e) => Some(e),
        Ok(()) => Some(after.reason.clone().unwrap_or_else(|| {
          match (after.owner, &after.handler) {
            (SchemeOwner::Other, Some(handler)) => format!(
              "the registration was written but the OS still routes \
               {scheme}: to {handler}"
            ),
            _ => format!(
              "the registration was written but the OS does not route \
               {scheme}: to this app"
            ),
          }
        })),
      };
      RegisterOutcome {
        registered,
        status: after,
        wrote: true,
        reason,
      }
    }
  }
}

/// Normalize a scheme passed to the runtime API (lower-cased, as the CLI
/// normalizes `desktop.app.deepLinks`) and check it is valid and one of the
/// app's `declared` (normalized) schemes. Only declared schemes may be
/// queried or registered. The error is a message for a `TypeError`.
pub fn resolve_declared_scheme(
  scheme: &str,
  declared: &[String],
) -> Result<String, String> {
  let normalized = scheme.to_ascii_lowercase();
  validate_deep_link_scheme(&normalized).map_err(|reason| {
    format!("Invalid deep-link scheme {scheme:?}: {reason}")
  })?;
  if !declared.iter().any(|d| d == &normalized) {
    return Err(format!(
      "{normalized:?} is not one of the app's deep-link schemes \
       (desktop.app.deepLinks in deno.json); only declared schemes can be \
       queried or registered"
    ));
  }
  Ok(normalized)
}

/// Windows: `HKCU\Software\Classes\<scheme>` / `HKLM\Software\Classes\<scheme>`
/// URL protocol keys and the per-user `UserChoice`.
pub mod windows {
  use super::OwnerStatus;

  /// A value this app writes into its `HKCU\Software\Classes\<scheme>` key:
  /// its app id, so a registration of the same app from a previous location
  /// (the executable moved) is recognized as this app's and refreshed.
  pub const APP_ID_VALUE: &str = "DenoDesktopAppId";

  /// The values of one `Software\Classes\<scheme>` key that matter here.
  #[derive(Debug, Clone, Default, PartialEq, Eq)]
  pub struct ClassKey {
    /// `shell\open\command` default value.
    pub command: Option<String>,
    /// Whether the `URL Protocol` value exists.
    pub url_protocol: bool,
    /// `DefaultIcon` default value.
    pub default_icon: Option<String>,
    /// The [`APP_ID_VALUE`] value.
    pub app_id: Option<String>,
  }

  /// `HKCU\Software\Microsoft\Windows\Shell\Associations\UrlAssociations\
  /// <scheme>\UserChoice`: the user's choice of handler (Windows 10+), which
  /// overrides the classes keys. It is hash-protected: only read, never
  /// written.
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct UserChoice {
    /// The chosen ProgId.
    pub prog_id: String,
    /// The ProgId's `shell\open\command` (per-user classes first, then the
    /// machine's), if it has one.
    pub command: Option<String>,
  }

  /// Everything the owner decision reads for one scheme.
  #[derive(Debug, Clone, Default, PartialEq, Eq)]
  pub struct SchemeState {
    /// `HKCU\Software\Classes\<scheme>`, if the key exists.
    pub user: Option<ClassKey>,
    /// `HKLM\Software\Classes\<scheme>`, if the key exists.
    pub machine: Option<ClassKey>,
    pub user_choice: Option<UserChoice>,
  }

  /// This app, as its registration names it.
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct ThisApp {
    /// The running executable's path (from the process, never from input).
    pub exe: String,
    /// The app id, when the app has one.
    pub app_id: Option<String>,
    /// The `DefaultIcon` value to register.
    pub icon: String,
  }

  /// The `shell\open\command` for `exe`: `"<exe>" -- "%1"`.
  ///
  /// The `--` ends the options: Windows substitutes the link for `%1`
  /// without escaping it, so a link containing a `"` can close the quotes
  /// and add arguments of its own. After `--` every argument is a
  /// positional one for the runtime (`launch_args::parse_launch_args`), the
  /// host (laufey stops reading its options there) and Chromium (CEF's
  /// command line parser treats `--` as the switch terminator), never an
  /// option. The class of bug is Electron's CVE-2018-1000006.
  ///
  /// A registration written in the earlier `"<exe>" "%1"` form no longer
  /// matches [`expected_key`], so the runtime refreshes it on the next
  /// launch (see [`owner`]).
  pub fn command_line(exe: &str) -> String {
    format!("\"{exe}\" -- \"%1\"")
  }

  /// The default value of a URL protocol key: `URL:<scheme>`.
  pub fn key_description(scheme: &str) -> String {
    format!("URL:{scheme}")
  }

  /// The `DefaultIcon` for `exe`: its first icon, `"<exe>",0`.
  pub fn default_icon(exe: &str) -> String {
    format!("\"{exe}\",0")
  }

  /// The key this app writes for itself.
  pub fn expected_key(me: &ThisApp) -> ClassKey {
    ClassKey {
      command: Some(command_line(&me.exe)),
      url_protocol: true,
      default_icon: Some(me.icon.clone()),
      app_id: me.app_id.clone(),
    }
  }

  /// The executable a `shell\open\command` runs: the quoted first token, or
  /// the unquoted text up to the end of the first `.exe` (unquoted paths
  /// with spaces are how many installers write it), or up to the first
  /// space.
  pub fn command_exe(command: &str) -> Option<String> {
    let command = command.trim_start();
    if let Some(rest) = command.strip_prefix('"') {
      let end = rest.find('"')?;
      let exe = &rest[..end];
      return (!exe.is_empty()).then(|| exe.to_string());
    }
    let lower = command.to_ascii_lowercase();
    if let Some(i) = lower.find(".exe") {
      let end = i + ".exe".len();
      if command[end..].is_empty() || command[end..].starts_with(' ') {
        return Some(command[..end].to_string());
      }
    }
    let exe = command.split(' ').next()?;
    (!exe.is_empty()).then(|| exe.to_string())
  }

  /// A path normalized for comparison: `/` as `\`, the `\\?\` prefix
  /// dropped, lower-cased (NTFS paths are case-insensitive).
  pub fn normalize_path(path: &str) -> String {
    let path = path.trim().replace('/', "\\");
    let path = if let Some(rest) = path.strip_prefix("\\\\?\\UNC\\") {
      format!("\\\\{rest}")
    } else if let Some(rest) = path.strip_prefix("\\\\?\\") {
      rest.to_string()
    } else {
      path
    };
    path.to_lowercase()
  }

  pub fn paths_equal(a: &str, b: &str) -> bool {
    normalize_path(a) == normalize_path(b)
  }

  fn command_is_this_app(command: &str, me: &ThisApp) -> bool {
    command_exe(command).is_some_and(|exe| paths_equal(&exe, &me.exe))
  }

  /// The handler the classes keys select (ignoring UserChoice). The merged
  /// `HKCR` view prefers the per-user key, so a per-user command wins over
  /// the machine's; a machine-wide handler of another app is "other" and is
  /// only shadowed by a forced registration.
  fn classes_owner(state: &SchemeState, me: &ThisApp) -> OwnerStatus {
    if let Some(user) = &state.user {
      match &user.command {
        Some(command) => {
          let handler = command_exe(command).or_else(|| Some(command.clone()));
          if command_is_this_app(command, me) {
            let stale = !key_matches_ignoring_case(user, &expected_key(me));
            return OwnerStatus::this(handler, stale);
          }
          // This app's own key from where the executable used to be.
          if let (Some(ours), Some(theirs)) = (&me.app_id, &user.app_id)
            && ours.eq_ignore_ascii_case(theirs)
          {
            return OwnerStatus::this(handler, true);
          }
          return OwnerStatus::other(handler);
        }
        None => {
          // A key without a command opens nothing. It is only this app's
          // if it carries this app's id.
          if let (Some(ours), Some(theirs)) = (&me.app_id, &user.app_id)
            && ours.eq_ignore_ascii_case(theirs)
          {
            return OwnerStatus::this(None, true);
          }
        }
      }
    }
    if let Some(command) =
      state.machine.as_ref().and_then(|m| m.command.as_ref())
    {
      let handler = command_exe(command).or_else(|| Some(command.clone()));
      if command_is_this_app(command, me) {
        // A machine-wide registration of this executable (an installer).
        // One without the `--` of [`command_line`] (an older installer) is
        // stale: the per-user key written to refresh it shadows it in the
        // merged `HKCR` view, so links never reach the unsafe command.
        let stale =
          !command.trim().eq_ignore_ascii_case(&command_line(&me.exe));
        return OwnerStatus::this(handler, stale);
      }
      return OwnerStatus::other(handler)
        .with_reason("registered machine-wide (HKLM) for another app");
    }
    OwnerStatus::unowned()
  }

  fn key_matches_ignoring_case(a: &ClassKey, b: &ClassKey) -> bool {
    let eq = |x: &Option<String>, y: &Option<String>| match (x, y) {
      (Some(x), Some(y)) => x.eq_ignore_ascii_case(y),
      (None, None) => true,
      _ => false,
    };
    a.url_protocol == b.url_protocol
      && eq(&a.command, &b.command)
      && eq(&a.default_icon, &b.default_icon)
      && eq(&a.app_id, &b.app_id)
  }

  /// Who handles `scheme` on Windows. A UserChoice overrides the classes
  /// keys, and is reported as the owner, unless its ProgId is the scheme's
  /// own key (then the classes keys decide, as they do without one).
  pub fn owner(scheme: &str, state: &SchemeState, me: &ThisApp) -> OwnerStatus {
    let classes = classes_owner(state, me);
    let Some(choice) = &state.user_choice else {
      return classes;
    };
    if choice.prog_id.eq_ignore_ascii_case(scheme) {
      return classes;
    }
    match choice.command.as_deref() {
      Some(command) if command_is_this_app(command, me) => {
        OwnerStatus::this(command_exe(command), false)
      }
      Some(command) => OwnerStatus::other(
        command_exe(command).or_else(|| Some(choice.prog_id.clone())),
      )
      .with_reason(format!(
        "the user chose {} for this scheme (Windows UserChoice); only the \
         user can change that, in Settings > Default apps",
        choice.prog_id
      )),
      None => {
        OwnerStatus::other(Some(choice.prog_id.clone())).with_reason(format!(
          "the user chose {} for this scheme (Windows UserChoice); only the \
           user can change that, in Settings > Default apps",
          choice.prog_id
        ))
      }
    }
  }
}

/// macOS: the LaunchServices default handler, by bundle id.
pub mod macos {
  use super::OwnerStatus;

  /// Who handles a scheme whose LaunchServices default handler is
  /// `default_handler` (a bundle id, `None` when there is none), for an app
  /// whose bundle id is one of `this_ids` (the running bundle's
  /// `CFBundleIdentifier`, the configured identifier). Bundle ids compare
  /// case-insensitively, as LaunchServices compares them.
  pub fn owner(
    default_handler: Option<&str>,
    this_ids: &[&str],
  ) -> OwnerStatus {
    match default_handler {
      None => OwnerStatus::unowned(),
      Some(handler)
        if this_ids.iter().any(|id| id.eq_ignore_ascii_case(handler)) =>
      {
        OwnerStatus::this(Some(handler.to_string()), false)
      }
      Some(handler) => OwnerStatus::other(Some(handler.to_string())),
    }
  }
}

/// Linux (freedesktop): `x-scheme-handler/<scheme>` defaults from
/// `mimeapps.list` and `mimeinfo.cache`, and the `.desktop` entry this app
/// installs for itself.
pub mod linux {
  use super::OwnerStatus;
  use super::Path;
  use super::PathBuf;

  /// The key this app's own `.desktop` entry carries, marking it as written
  /// by the runtime (and safe to rewrite).
  pub const GENERATED_KEY: &str = "X-Deno-Desktop-Scheme-Handler";

  /// The XDG base directories the lookup follows.
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct XdgDirs {
    pub config_home: PathBuf,
    pub config_dirs: Vec<PathBuf>,
    pub data_home: PathBuf,
    pub data_dirs: Vec<PathBuf>,
    /// `XDG_CURRENT_DESKTOP`, lower-cased, for `<desktop>-mimeapps.list`.
    pub current_desktops: Vec<String>,
  }

  impl XdgDirs {
    /// Resolve the directories from environment variables (`get`) with the
    /// spec's defaults. Relative values are ignored, as the spec says.
    /// `None` without a usable home directory.
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Option<Self> {
      // Unix paths whatever the host (so the rules test anywhere).
      let abs = |v: String| v.starts_with('/').then(|| PathBuf::from(v));
      let list = |name: &str, default: &[&str]| -> Vec<PathBuf> {
        let dirs: Vec<PathBuf> = get(name)
          .map(|v| {
            v.split(':')
              .filter(|s| !s.is_empty())
              .filter_map(|s| abs(s.to_string()))
              .collect()
          })
          .unwrap_or_default();
        if dirs.is_empty() {
          default.iter().map(PathBuf::from).collect()
        } else {
          dirs
        }
      };
      let home = get("HOME").filter(|h| !h.is_empty()).and_then(abs);
      let config_home = get("XDG_CONFIG_HOME")
        .and_then(abs)
        .or_else(|| home.as_ref().map(|h| h.join(".config")))?;
      let data_home = get("XDG_DATA_HOME")
        .and_then(abs)
        .or_else(|| home.as_ref().map(|h| h.join(".local/share")))?;
      Some(Self {
        config_home,
        config_dirs: list("XDG_CONFIG_DIRS", &["/etc/xdg"]),
        data_home,
        data_dirs: list("XDG_DATA_DIRS", &["/usr/local/share", "/usr/share"]),
        current_desktops: get("XDG_CURRENT_DESKTOP")
          .map(|v| {
            v.split(':')
              .filter(|s| !s.is_empty())
              .map(|s| s.to_ascii_lowercase())
              .collect()
          })
          .unwrap_or_default(),
      })
    }

    /// The `applications` directories, most important first.
    pub fn application_dirs(&self) -> Vec<PathBuf> {
      std::iter::once(&self.data_home)
        .chain(self.data_dirs.iter())
        .map(|d| d.join("applications"))
        .collect()
    }

    /// Where this app installs its own `.desktop` entry.
    pub fn user_applications_dir(&self) -> PathBuf {
      self.data_home.join("applications")
    }

    /// The `mimeapps.list` files in the spec's lookup order: per directory
    /// (config home, config dirs, then the deprecated data locations), the
    /// desktop-specific files before the generic one.
    pub fn mimeapps_lists(&self) -> Vec<PathBuf> {
      let dirs = std::iter::once(self.config_home.clone())
        .chain(self.config_dirs.iter().cloned())
        .chain(self.application_dirs());
      let mut out = Vec::new();
      for dir in dirs {
        for desktop in &self.current_desktops {
          out.push(dir.join(format!("{desktop}-mimeapps.list")));
        }
        out.push(dir.join("mimeapps.list"));
      }
      out
    }
  }

  /// The MIME type a scheme handler registers for.
  pub fn scheme_mime_type(scheme: &str) -> String {
    format!("x-scheme-handler/{scheme}")
  }

  /// A resolved handler: its desktop file id and the file found for it.
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Handler {
    pub desktop_id: String,
    pub path: PathBuf,
  }

  /// The `;`-separated values of `key` in `[section]` of an ini-style file
  /// (`mimeapps.list`, `mimeinfo.cache`, a `.desktop` entry).
  pub fn ini_list(contents: &str, section: &str, key: &str) -> Vec<String> {
    ini_value(contents, section, key)
      .map(|v| {
        v.split(';')
          .map(str::trim)
          .filter(|s| !s.is_empty())
          .map(str::to_string)
          .collect()
      })
      .unwrap_or_default()
  }

  /// The raw value of `key` in `[section]` (the first occurrence).
  pub fn ini_value(contents: &str, section: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for line in contents.lines() {
      let line = line.trim();
      if line.is_empty() || line.starts_with('#') {
        continue;
      }
      if let Some(name) =
        line.strip_prefix('[').and_then(|l| l.strip_suffix(']'))
      {
        in_section = name == section;
        continue;
      }
      if !in_section {
        continue;
      }
      if let Some((k, v)) = line.split_once('=')
        && k.trim() == key
      {
        return Some(v.trim().to_string());
      }
    }
    None
  }

  /// The handler of `scheme`, as the freedesktop MIME-apps spec resolves
  /// it: the first installed entry among the `[Default Applications]` of the
  /// `mimeapps.list` files, then among their `[Added Associations]`, then
  /// among the `mimeinfo.cache` of the application directories. An entry
  /// whose `.desktop` file is not installed is skipped, as `xdg-open` skips
  /// it. `read` reads a file (`None` if it does not exist).
  pub fn default_handler(
    scheme: &str,
    dirs: &XdgDirs,
    read: &dyn Fn(&Path) -> Option<String>,
  ) -> Option<Handler> {
    let mime = scheme_mime_type(scheme);
    let app_dirs = dirs.application_dirs();
    let find = |id: &str| -> Option<Handler> {
      if id.contains('/') || !id.ends_with(".desktop") {
        return None;
      }
      app_dirs.iter().find_map(|dir| {
        let path = dir.join(id);
        read(&path).map(|_| Handler {
          desktop_id: id.to_string(),
          path,
        })
      })
    };
    let lists: Vec<String> = dirs
      .mimeapps_lists()
      .iter()
      .filter_map(|p| read(p))
      .collect();
    for section in ["Default Applications", "Added Associations"] {
      for list in &lists {
        for id in ini_list(list, section, &mime) {
          if let Some(handler) = find(&id) {
            return Some(handler);
          }
        }
      }
    }
    for dir in &app_dirs {
      if let Some(cache) = read(&dir.join("mimeinfo.cache")) {
        for id in ini_list(&cache, "MIME Cache", &mime) {
          if let Some(handler) = find(&id) {
            return Some(handler);
          }
        }
      }
    }
    None
  }

  /// Undo the desktop-entry string escapes (`\s`, `\n`, `\t`, `\r`, `\\`).
  fn unescape_string_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
      if c != '\\' {
        out.push(c);
        continue;
      }
      match chars.next() {
        Some('s') => out.push(' '),
        Some('n') => out.push('\n'),
        Some('t') => out.push('\t'),
        Some('r') => out.push('\r'),
        Some('\\') => out.push('\\'),
        Some(other) => {
          out.push('\\');
          out.push(other);
        }
        None => out.push('\\'),
      }
    }
    out
  }

  /// Split an `Exec` value (already string-unescaped) into arguments with
  /// the spec's quoting: `"…"` quotes, and inside quotes `\` escapes `"`,
  /// `` ` ``, `$` and `\`.
  fn split_exec(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut has_token = false;
    let mut in_quotes = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
      if in_quotes {
        match c {
          '"' => in_quotes = false,
          '\\' => {
            if let Some(next) = chars.next() {
              current.push(next);
            }
          }
          _ => current.push(c),
        }
      } else {
        match c {
          '"' => {
            in_quotes = true;
            has_token = true;
          }
          ' ' | '\t' => {
            if has_token {
              args.push(std::mem::take(&mut current));
              has_token = false;
            }
          }
          _ => {
            current.push(c);
            has_token = true;
          }
        }
      }
    }
    if has_token {
      args.push(current);
    }
    args
  }

  /// The program a `.desktop` entry's `Exec` runs, skipping an `env`
  /// wrapper and its `NAME=value` assignments (the packager's entries launch
  /// through `env LAUFEY_APP_ID=…`).
  pub fn exec_program(entry: &str) -> Option<String> {
    let exec = ini_value(entry, "Desktop Entry", "Exec")?;
    let args = split_exec(&unescape_string_value(&exec));
    let mut iter = args.into_iter();
    let first = iter.next()?;
    let is_env = first == "env" || first.ends_with("/env");
    let program = if is_env {
      // Skip env's options (`-u NAME` / `-C DIR` take a value) and the
      // `NAME=value` assignments.
      let mut program = None;
      while let Some(arg) = iter.next() {
        match arg.as_str() {
          "-u" | "--unset" | "-C" | "--chdir" => {
            iter.next();
          }
          a if a.starts_with('-') || a.contains('=') => {}
          _ => {
            program = Some(arg);
            break;
          }
        }
      }
      program?
    } else {
      first
    };
    // `%%` is a literal `%` (other field codes don't appear in a program).
    Some(program.replace("%%", "%"))
  }

  /// The schemes a `.desktop` entry's `MimeType` claims.
  pub fn entry_schemes(entry: &str) -> Vec<String> {
    ini_list(entry, "Desktop Entry", "MimeType")
      .into_iter()
      .filter_map(|m| m.strip_prefix("x-scheme-handler/").map(str::to_string))
      .collect()
  }

  /// Characters the desktop-entry spec reserves in an unquoted `Exec`
  /// argument.
  const EXEC_RESERVED: &[char] = &[
    ' ', '\t', '\n', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*',
    '?', '#', '(', ')', '`', '%', '=',
  ];

  /// `arg` as one `Exec` argument: as is when it has no reserved character
  /// (the common case, and the only form xdg-utils 1.1's generic `xdg-open`
  /// runs: it takes the first space-separated word literally), else quoted
  /// with [`quote_exec_arg`].
  fn exec_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains(EXEC_RESERVED) {
      arg.to_string()
    } else {
      quote_exec_arg(arg)
    }
  }

  /// Quote `arg` as one `Exec` argument, then escape it as a desktop-entry
  /// string value. `%` is a field code there, so it is doubled.
  fn quote_exec_arg(arg: &str) -> String {
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    for c in arg.chars() {
      match c {
        '"' | '`' | '$' | '\\' => {
          quoted.push('\\');
          quoted.push(c);
        }
        '%' => quoted.push_str("%%"),
        _ => quoted.push(c),
      }
    }
    quoted.push('"');
    quoted.replace('\\', "\\\\")
  }

  /// The `.desktop` entry this app installs for itself in
  /// `$XDG_DATA_HOME/applications/<app id>.desktop`: it runs `exe` with the
  /// URL (`%u`) and claims `schemes`. Hidden from menus (it registers a
  /// handler, it doesn't install the app). `Err` for an executable path or
  /// name with control characters, which an entry cannot carry.
  pub fn render_entry(
    name: &str,
    app_id: &str,
    exe: &str,
    schemes: &[String],
  ) -> Result<String, String> {
    if exe.chars().any(char::is_control) || name.chars().any(char::is_control) {
      return Err(
        "the executable path or app name contains a control character"
          .to_string(),
      );
    }
    let mime: String = schemes
      .iter()
      .map(|s| format!("{};", scheme_mime_type(s)))
      .collect();
    Ok(format!(
      "[Desktop Entry]\n\
       Type=Application\n\
       Name={name}\n\
       Exec={exec} %u\n\
       Terminal=false\n\
       NoDisplay=true\n\
       StartupWMClass={app_id}\n\
       MimeType={mime}\n\
       {GENERATED_KEY}=true\n",
      exec = exec_arg(exe),
    ))
  }

  /// This app, as its `.desktop` entry names it.
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct ThisApp {
    /// `<app id>.desktop`, when the app has an id.
    pub desktop_id: Option<String>,
    /// The program a launch must run (the AppImage file for an AppImage,
    /// else the running executable), canonicalized.
    pub exe: PathBuf,
    /// Where this app installs its own entry.
    pub own_entry: Option<PathBuf>,
  }

  /// Who handles `scheme`. `handler` is [`default_handler`]'s result, `read`
  /// reads a file, and `resolve` turns an `Exec` program (a path or a bare
  /// name) into a canonical path.
  ///
  /// The handler is this app when it is this app's desktop id (stale when
  /// it is this app's own entry and no longer runs this executable or claims
  /// the scheme, e.g. after the executable moved), or when its entry runs
  /// this executable (a `.deb`/`.rpm` entry under another id).
  pub fn owner(
    scheme: &str,
    handler: Option<&Handler>,
    me: &ThisApp,
    read: &dyn Fn(&Path) -> Option<String>,
    resolve: &dyn Fn(&str) -> Option<PathBuf>,
  ) -> OwnerStatus {
    let Some(handler) = handler else {
      return OwnerStatus::unowned();
    };
    let entry = read(&handler.path);
    let runs_this_exe = entry
      .as_deref()
      .and_then(exec_program)
      .and_then(|program| resolve(&program))
      .is_some_and(|program| program == me.exe);
    let display = Some(handler.desktop_id.clone());
    if me.desktop_id.as_deref() == Some(handler.desktop_id.as_str()) {
      let is_own_entry =
        me.own_entry.as_deref() == Some(handler.path.as_path());
      let stale = is_own_entry
        && !(runs_this_exe
          && entry
            .as_deref()
            .is_some_and(|e| entry_schemes(e).iter().any(|s| s == scheme)));
      return OwnerStatus::this(display, stale);
    }
    if runs_this_exe {
      return OwnerStatus::this(display, false);
    }
    OwnerStatus::other(display)
  }
}

#[cfg(test)]
mod tests {
  use std::cell::Cell;
  use std::cell::RefCell;
  use std::collections::HashMap;

  use super::*;

  /// A registry whose owner changes to `after` once `register` is called.
  struct FakeRegistry {
    before: OwnerStatus,
    after: OwnerStatus,
    fail: Option<String>,
    registered: Cell<bool>,
    take_over: Cell<Option<bool>>,
  }

  impl FakeRegistry {
    fn new(before: OwnerStatus, after: OwnerStatus) -> Self {
      Self {
        before,
        after,
        fail: None,
        registered: Cell::new(false),
        take_over: Cell::new(None),
      }
    }
  }

  impl SchemeRegistry for FakeRegistry {
    fn owner(&self, _scheme: &str) -> OwnerStatus {
      if self.registered.get() {
        self.after.clone()
      } else {
        self.before.clone()
      }
    }
    fn register(&self, _scheme: &str, take_over: bool) -> Result<(), String> {
      self.take_over.set(Some(take_over));
      if let Some(e) = &self.fail {
        return Err(e.clone());
      }
      self.registered.set(true);
      Ok(())
    }
  }

  fn this_fresh() -> OwnerStatus {
    OwnerStatus::this(Some("me".into()), false)
  }

  #[test]
  fn plan_registers_unowned_and_refreshes_stale() {
    for mode in [
      RegisterMode::Startup,
      RegisterMode::Explicit,
      RegisterMode::Force,
    ] {
      assert_eq!(
        plan_registration(&OwnerStatus::unowned(), mode),
        RegisterAction::Write
      );
      assert_eq!(plan_registration(&this_fresh(), mode), RegisterAction::Keep);
      assert_eq!(
        plan_registration(&OwnerStatus::this(None, true), mode),
        RegisterAction::Write
      );
    }
  }

  #[test]
  fn plan_takes_another_apps_scheme_only_when_forced() {
    let other = OwnerStatus::other(Some("them".into()));
    assert_eq!(
      plan_registration(&other, RegisterMode::Startup),
      RegisterAction::LeaveToOther
    );
    assert_eq!(
      plan_registration(&other, RegisterMode::Explicit),
      RegisterAction::LeaveToOther
    );
    assert_eq!(
      plan_registration(&other, RegisterMode::Force),
      RegisterAction::Write
    );
  }

  #[test]
  fn register_unowned_writes_without_taking_over() {
    let reg = FakeRegistry::new(OwnerStatus::unowned(), this_fresh());
    let out = register_scheme(&reg, "acme", RegisterMode::Startup);
    assert!(out.registered && out.wrote);
    assert_eq!(out.reason, None);
    assert_eq!(reg.take_over.get(), Some(false));
  }

  #[test]
  fn register_own_current_registration_writes_nothing() {
    let reg = FakeRegistry::new(this_fresh(), this_fresh());
    let out = register_scheme(&reg, "acme", RegisterMode::Startup);
    assert!(out.registered);
    assert!(!out.wrote);
    assert_eq!(reg.take_over.get(), None);
  }

  #[test]
  fn register_leaves_another_app_alone_unless_forced() {
    let other = OwnerStatus::other(Some("C:\\Other\\other.exe".into()));
    for mode in [RegisterMode::Startup, RegisterMode::Explicit] {
      let reg = FakeRegistry::new(other.clone(), this_fresh());
      let out = register_scheme(&reg, "acme", mode);
      assert!(!out.registered && !out.wrote);
      assert_eq!(out.status, other);
      assert!(out.reason.unwrap().contains("C:\\Other\\other.exe"));
      assert_eq!(reg.take_over.get(), None);
    }
    let reg = FakeRegistry::new(other, this_fresh());
    let out = register_scheme(&reg, "acme", RegisterMode::Force);
    assert!(out.registered && out.wrote);
    assert_eq!(reg.take_over.get(), Some(true));
  }

  #[test]
  fn register_reports_the_owner_after_the_write() {
    // A forced write that the OS overrides (a Windows UserChoice): the write
    // succeeds, the app still isn't the handler, and the reason says why.
    let after = OwnerStatus::other(Some("x".into())).with_reason("UserChoice");
    let reg = FakeRegistry::new(OwnerStatus::other(None), after.clone());
    let out = register_scheme(&reg, "acme", RegisterMode::Force);
    assert!(!out.registered && out.wrote);
    assert_eq!(out.status, after);
    assert_eq!(out.reason.as_deref(), Some("UserChoice"));

    // A write that fails (e.g. xdg-mime missing) keeps its error.
    let mut reg = FakeRegistry::new(OwnerStatus::unowned(), this_fresh());
    reg.fail = Some("xdg-mime was not found".into());
    let out = register_scheme(&reg, "acme", RegisterMode::Explicit);
    assert!(!out.registered);
    assert_eq!(out.status.owner, SchemeOwner::Unowned);
    assert_eq!(out.reason.as_deref(), Some("xdg-mime was not found"));
  }

  #[test]
  fn only_declared_schemes_resolve() {
    let declared = vec!["acme".to_string(), "t3code".to_string()];
    assert_eq!(resolve_declared_scheme("ACME", &declared).unwrap(), "acme");
    assert!(
      resolve_declared_scheme("other", &declared)
        .unwrap_err()
        .contains("not one of the app's deep-link schemes")
    );
    assert!(
      resolve_declared_scheme("https", &declared)
        .unwrap_err()
        .contains("reserved")
    );
    assert!(resolve_declared_scheme("a b", &declared).is_err());
    assert!(resolve_declared_scheme("", &declared).is_err());
    assert!(resolve_declared_scheme("acme", &[]).is_err());
  }

  mod windows_owner {
    use super::super::windows::*;
    use super::super::*;

    const EXE: &str = "C:\\Program Files\\Acme\\Acme.exe";

    fn me() -> ThisApp {
      ThisApp {
        exe: EXE.to_string(),
        app_id: Some("com.acme.app".to_string()),
        icon: format!("\"{EXE}\",0"),
      }
    }

    fn state(user: Option<ClassKey>, machine: Option<ClassKey>) -> SchemeState {
      SchemeState {
        user,
        machine,
        user_choice: None,
      }
    }

    fn foreign(exe: &str) -> ClassKey {
      ClassKey {
        command: Some(command_line(exe)),
        url_protocol: true,
        default_icon: None,
        app_id: None,
      }
    }

    #[test]
    fn command_exe_parses_quoted_and_unquoted() {
      assert_eq!(
        command_exe("\"C:\\A b\\x.exe\" \"%1\"").as_deref(),
        Some("C:\\A b\\x.exe")
      );
      assert_eq!(
        command_exe("C:\\Program Files\\X\\x.exe %1").as_deref(),
        Some("C:\\Program Files\\X\\x.exe")
      );
      assert_eq!(
        command_exe("C:\\bin\\tool.EXE").as_deref(),
        Some("C:\\bin\\tool.EXE")
      );
      assert_eq!(command_exe("rundll32 url.dll").as_deref(), Some("rundll32"));
      assert_eq!(command_exe("\"unterminated"), None);
      assert_eq!(command_exe(""), None);
    }

    #[test]
    fn command_line_ends_the_options_before_the_link() {
      // The link is substituted for %1 unescaped: everything from it on has
      // to be a positional argument, so `--` comes first.
      let cmd = command_line(EXE);
      assert_eq!(cmd, format!("\"{EXE}\" -- \"%1\""));
      let terminator = cmd.find(" -- ").expect("no -- in the command");
      let link = cmd.find("%1").unwrap();
      assert!(terminator < link, "{cmd}");
      assert_eq!(command_exe(&cmd).as_deref(), Some(EXE));
    }

    #[test]
    fn a_registration_without_the_terminator_is_refreshed() {
      // The earlier `"<exe>" "%1"` form, per-user: this app's, stale.
      let mut key = expected_key(&me());
      key.command = Some(format!("\"{EXE}\" \"%1\""));
      let s = owner("acme", &state(Some(key), None), &me());
      assert_eq!(s.owner, SchemeOwner::This);
      assert!(s.stale);
      assert_eq!(
        plan_registration(&s, RegisterMode::Startup),
        RegisterAction::Write
      );
      // The same form machine-wide (an older installer): stale too, so the
      // per-user key that shadows it gets written.
      let mut machine = foreign(EXE);
      machine.command = Some(format!("\"{EXE}\" \"%1\""));
      let s = owner("acme", &state(None, Some(machine)), &me());
      assert_eq!(s.owner, SchemeOwner::This);
      assert!(s.stale);
      assert_eq!(
        plan_registration(&s, RegisterMode::Startup),
        RegisterAction::Write
      );
    }

    #[test]
    fn paths_compare_normalized() {
      assert!(paths_equal(EXE, "c:/program files/acme/ACME.EXE"));
      assert!(paths_equal(&format!("\\\\?\\{EXE}"), EXE));
      assert!(!paths_equal(EXE, "C:\\Acme\\Acme.exe"));
    }

    #[test]
    fn unowned() {
      let s = owner("acme", &SchemeState::default(), &me());
      assert_eq!(s.owner, SchemeOwner::Unowned);
      // A per-user key without a command opens nothing.
      let s = owner("acme", &state(Some(ClassKey::default()), None), &me());
      assert_eq!(s.owner, SchemeOwner::Unowned);
    }

    #[test]
    fn self_current_and_stale() {
      let s = owner("acme", &state(Some(expected_key(&me())), None), &me());
      assert_eq!(s, OwnerStatus::this(Some(EXE.to_string()), false));
      // Case differences in the path are the same registration.
      let mut key = expected_key(&me());
      key.command = Some(command_line(&EXE.to_uppercase()));
      key.default_icon = Some(format!("\"{}\",0", EXE.to_uppercase()));
      let s = owner("acme", &state(Some(key), None), &me());
      assert!(!s.stale && s.owner == SchemeOwner::This);
      // Missing `URL Protocol` or a command without "%1": refresh it.
      let mut key = expected_key(&me());
      key.url_protocol = false;
      let s = owner("acme", &state(Some(key), None), &me());
      assert_eq!(s.owner, SchemeOwner::This);
      assert!(s.stale);
      let mut key = expected_key(&me());
      key.command = Some(format!("\"{EXE}\""));
      assert!(owner("acme", &state(Some(key), None), &me()).stale);
    }

    #[test]
    fn moved_exe_is_self_and_stale() {
      let mut key = foreign("D:\\Old place\\Acme.exe");
      key.app_id = Some("COM.ACME.APP".to_string());
      let s = owner("acme", &state(Some(key.clone()), None), &me());
      assert_eq!(s.owner, SchemeOwner::This);
      assert!(s.stale);
      assert_eq!(s.handler.as_deref(), Some("D:\\Old place\\Acme.exe"));
      // Without an app id of its own the app can't claim it.
      let mut anon = me();
      anon.app_id = None;
      assert_eq!(
        owner("acme", &state(Some(key), None), &anon).owner,
        SchemeOwner::Other
      );
    }

    #[test]
    fn other_per_user() {
      let s = owner(
        "acme",
        &state(Some(foreign("C:\\Other\\other.exe")), None),
        &me(),
      );
      assert_eq!(
        s,
        OwnerStatus::other(Some("C:\\Other\\other.exe".to_string()))
      );
      // A per-user command wins over the machine's, whoever that is.
      let s = owner(
        "acme",
        &state(
          Some(foreign("C:\\Other\\other.exe")),
          Some(expected_key(&me())),
        ),
        &me(),
      );
      assert_eq!(s.owner, SchemeOwner::Other);
    }

    #[test]
    fn machine_only() {
      // Another app registered machine-wide: "other", not shadowed.
      let s = owner(
        "acme",
        &state(None, Some(foreign("C:\\Program Files\\Other\\o.exe"))),
        &me(),
      );
      assert_eq!(s.owner, SchemeOwner::Other);
      assert_eq!(
        s.handler.as_deref(),
        Some("C:\\Program Files\\Other\\o.exe")
      );
      assert!(s.reason.unwrap().contains("HKLM"));
      assert_eq!(
        plan_registration(
          &owner("acme", &state(None, Some(foreign("C:\\o.exe"))), &me()),
          RegisterMode::Startup
        ),
        RegisterAction::LeaveToOther
      );
      // This executable registered machine-wide (by an installer): self, and
      // nothing to write per-user.
      let s = owner("acme", &state(None, Some(foreign(EXE))), &me());
      assert_eq!(s, OwnerStatus::this(Some(EXE.to_string()), false));
    }

    #[test]
    fn user_choice_overrides_the_classes_keys() {
      let mut st = state(Some(expected_key(&me())), None);
      st.user_choice = Some(UserChoice {
        prog_id: "OtherApp.Url".to_string(),
        command: Some("\"C:\\Other\\other.exe\" --url \"%1\"".to_string()),
      });
      let s = owner("acme", &st, &me());
      assert_eq!(s.owner, SchemeOwner::Other);
      assert_eq!(s.handler.as_deref(), Some("C:\\Other\\other.exe"));
      assert!(s.reason.unwrap().contains("UserChoice"));
      // A forced write can't beat it: the plan writes, the owner stays.
      assert_eq!(
        plan_registration(&owner("acme", &st, &me()), RegisterMode::Force),
        RegisterAction::Write
      );

      // A ProgId without a command (an app since uninstalled): reported by
      // its ProgId.
      st.user_choice = Some(UserChoice {
        prog_id: "AppX1234".to_string(),
        command: None,
      });
      let s = owner("acme", &st, &me());
      assert_eq!(s.owner, SchemeOwner::Other);
      assert_eq!(s.handler.as_deref(), Some("AppX1234"));

      // A UserChoice of this app (under another ProgId): self.
      st.user_choice = Some(UserChoice {
        prog_id: "Acme.Url".to_string(),
        command: Some(command_line(EXE)),
      });
      assert_eq!(owner("acme", &st, &me()).owner, SchemeOwner::This);

      // A UserChoice naming the scheme's own key defers to the classes
      // keys.
      let mut st = state(None, None);
      st.user_choice = Some(UserChoice {
        prog_id: "ACME".to_string(),
        command: None,
      });
      assert_eq!(owner("acme", &st, &me()).owner, SchemeOwner::Unowned);
    }
  }

  mod macos_owner {
    use super::super::macos::owner;
    use super::super::*;

    #[test]
    fn by_bundle_id() {
      let ids = ["com.acme.app"];
      assert_eq!(owner(None, &ids).owner, SchemeOwner::Unowned);
      assert_eq!(
        owner(Some("com.ACME.app"), &ids),
        OwnerStatus::this(Some("com.ACME.app".into()), false)
      );
      assert_eq!(
        owner(Some("com.apple.Safari"), &ids),
        OwnerStatus::other(Some("com.apple.Safari".into()))
      );
      // Not running from a bundle and no configured id: anything is other.
      assert_eq!(owner(Some("com.acme.app"), &[]).owner, SchemeOwner::Other);
    }
  }

  mod linux_owner {
    use super::super::linux::*;
    use super::super::*;
    use super::*;

    fn dirs() -> XdgDirs {
      XdgDirs::from_env(|k| {
        match k {
          "HOME" => Some("/home/me"),
          "XDG_CURRENT_DESKTOP" => Some("ubuntu:GNOME"),
          _ => None,
        }
        .map(str::to_string)
      })
      .unwrap()
    }

    struct Files(RefCell<HashMap<PathBuf, String>>);

    impl Files {
      fn new(files: &[(&str, &str)]) -> Self {
        Self(RefCell::new(
          files
            .iter()
            .map(|(p, c)| (PathBuf::from(p), c.to_string()))
            .collect(),
        ))
      }
      fn read(&self, p: &Path) -> Option<String> {
        self.0.borrow().get(p).cloned()
      }
    }

    const OURS: &str =
      "/home/me/.local/share/applications/com.acme.app.desktop";

    fn me() -> ThisApp {
      ThisApp {
        desktop_id: Some("com.acme.app.desktop".to_string()),
        exe: PathBuf::from("/opt/Acme/Acme"),
        own_entry: Some(PathBuf::from(OURS)),
      }
    }

    fn resolve(program: &str) -> Option<PathBuf> {
      match program {
        // The `/usr/bin/<pkg>` symlink a .deb installs.
        "acme" => Some(PathBuf::from("/opt/Acme/Acme")),
        p if p.starts_with('/') => Some(PathBuf::from(p)),
        _ => None,
      }
    }

    fn owner_of(files: &Files) -> OwnerStatus {
      let read = |p: &Path| files.read(p);
      let handler = default_handler("acme", &dirs(), &read);
      owner("acme", handler.as_ref(), &me(), &read, &resolve)
    }

    fn our_entry(exe: &str) -> String {
      render_entry("Acme", "com.acme.app", exe, &["acme".to_string()]).unwrap()
    }

    #[test]
    fn xdg_dirs_defaults_and_order() {
      let d = dirs();
      assert_eq!(d.config_home, PathBuf::from("/home/me/.config"));
      assert_eq!(d.data_home, PathBuf::from("/home/me/.local/share"));
      let lists = d.mimeapps_lists();
      assert_eq!(
        &lists[..3],
        &[
          PathBuf::from("/home/me/.config/ubuntu-mimeapps.list"),
          PathBuf::from("/home/me/.config/gnome-mimeapps.list"),
          PathBuf::from("/home/me/.config/mimeapps.list"),
        ]
      );
      assert_eq!(
        d.application_dirs(),
        vec![
          PathBuf::from("/home/me/.local/share/applications"),
          PathBuf::from("/usr/local/share/applications"),
          PathBuf::from("/usr/share/applications"),
        ]
      );
      // Relative values are ignored; no home and no XDG_*_HOME is no dirs.
      let d = XdgDirs::from_env(|k| {
        match k {
          "HOME" => Some("/h"),
          "XDG_DATA_HOME" => Some("rel"),
          "XDG_DATA_DIRS" => Some("/a:rel:/b"),
          _ => None,
        }
        .map(str::to_string)
      })
      .unwrap();
      assert_eq!(d.data_home, PathBuf::from("/h/.local/share"));
      assert_eq!(d.data_dirs, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
      assert!(XdgDirs::from_env(|_| None).is_none());
    }

    #[test]
    fn unowned() {
      assert_eq!(owner_of(&Files::new(&[])).owner, SchemeOwner::Unowned);
      // A default whose entry isn't installed is skipped.
      let files = Files::new(&[(
        "/home/me/.config/mimeapps.list",
        "[Default Applications]\nx-scheme-handler/acme=gone.desktop;\n",
      )]);
      assert_eq!(owner_of(&files).owner, SchemeOwner::Unowned);
    }

    #[test]
    fn self_via_own_entry_and_stale_after_a_move() {
      let files = Files::new(&[
        (
          "/home/me/.config/mimeapps.list",
          "[Default Applications]\nx-scheme-handler/acme=com.acme.app.desktop\n",
        ),
        (OURS, &our_entry("/opt/Acme/Acme")),
      ]);
      assert_eq!(
        owner_of(&files),
        OwnerStatus::this(Some("com.acme.app.desktop".into()), false)
      );
      // The executable moved: still this app's entry, but stale.
      files
        .0
        .borrow_mut()
        .insert(PathBuf::from(OURS), our_entry("/old/Acme/Acme"));
      let s = owner_of(&files);
      assert_eq!(s.owner, SchemeOwner::This);
      assert!(s.stale);
      // An entry that no longer claims the scheme is stale too.
      files.0.borrow_mut().insert(
        PathBuf::from(OURS),
        render_entry("Acme", "com.acme.app", "/opt/Acme/Acme", &[]).unwrap(),
      );
      assert!(owner_of(&files).stale);
    }

    #[test]
    fn self_via_a_package_entry() {
      // A .deb's entry under the package name that runs this executable
      // through env and the /usr/bin symlink; found through mimeinfo.cache.
      let files = Files::new(&[
        (
          "/usr/share/applications/mimeinfo.cache",
          "[MIME Cache]\nx-scheme-handler/acme=acme.desktop;\n",
        ),
        (
          "/usr/share/applications/acme.desktop",
          "[Desktop Entry]\nExec=env LAUFEY_APP_ID=com.acme.app acme %u\n",
        ),
      ]);
      assert_eq!(
        owner_of(&files),
        OwnerStatus::this(Some("acme.desktop".into()), false)
      );
    }

    #[test]
    fn other_and_lookup_order() {
      let files = Files::new(&[
        (
          "/home/me/.config/gnome-mimeapps.list",
          "[Default Applications]\nx-scheme-handler/acme=electron-t3.desktop\n",
        ),
        (
          "/home/me/.config/mimeapps.list",
          "[Default Applications]\nx-scheme-handler/acme=com.acme.app.desktop\n",
        ),
        (
          "/usr/share/applications/electron-t3.desktop",
          "[Desktop Entry]\nExec=/opt/T3/t3 %U\n",
        ),
        (OURS, &our_entry("/opt/Acme/Acme")),
      ]);
      // The desktop-specific list comes first.
      assert_eq!(
        owner_of(&files),
        OwnerStatus::other(Some("electron-t3.desktop".into()))
      );
      // Added Associations and mimeinfo.cache only count without a default.
      let files = Files::new(&[
        (
          "/home/me/.config/mimeapps.list",
          "[Added Associations]\nx-scheme-handler/acme=electron-t3.desktop;\n",
        ),
        (
          "/usr/share/applications/mimeinfo.cache",
          "[MIME Cache]\nx-scheme-handler/acme=com.acme.app.desktop;\n",
        ),
        (
          "/usr/share/applications/electron-t3.desktop",
          "[Desktop Entry]\nExec=/opt/T3/t3 %U\n",
        ),
        (
          "/usr/share/applications/com.acme.app.desktop",
          "[Desktop Entry]\nExec=/opt/Acme/Acme %u\n",
        ),
      ]);
      assert_eq!(owner_of(&files).owner, SchemeOwner::Other);
    }

    #[test]
    fn exec_program_handles_env_and_quoting() {
      let e =
        |exec: &str| exec_program(&format!("[Desktop Entry]\nExec={exec}\n"));
      assert_eq!(e("/usr/bin/foo %u").as_deref(), Some("/usr/bin/foo"));
      assert_eq!(e("env A=1 B=2 \"My App\" %u").as_deref(), Some("My App"));
      assert_eq!(e("/usr/bin/env -u X A=1 foo").as_deref(), Some("foo"));
      assert_eq!(
        e(r#""/opt/a \\"q\\" \\$x/b" %u"#).as_deref(),
        Some(r#"/opt/a "q" $x/b"#)
      );
      assert_eq!(e("").as_deref(), None);
      assert_eq!(exec_program("[Other]\nExec=x\n"), None);
      // TryExec is not Exec.
      assert_eq!(
        exec_program("[Desktop Entry]\nTryExec=a\nExec=b\n").as_deref(),
        Some("b")
      );
    }

    #[test]
    fn rendered_entry_round_trips() {
      let exe = "/home/me/My $App \"x\" 100%/Acme";
      let entry = render_entry(
        "My App",
        "com.acme.app",
        exe,
        &["acme".to_string(), "acme-dev".to_string()],
      )
      .unwrap();
      assert_eq!(exec_program(&entry).as_deref(), Some(exe));
      assert_eq!(entry_schemes(&entry), vec!["acme", "acme-dev"]);
      // A plain path stays unquoted (xdg-utils' generic xdg-open runs the
      // first word of Exec literally).
      let plain =
        render_entry("A", "a.b", "/opt/Acme/Acme", &["acme".into()]).unwrap();
      assert!(plain.contains("\nExec=/opt/Acme/Acme %u\n"), "{plain}");
      assert_eq!(exec_program(&plain).as_deref(), Some("/opt/Acme/Acme"));
      let spaced = render_entry("A", "a.b", "/opt/My App/a", &[]).unwrap();
      assert!(spaced.contains("\nExec=\"/opt/My App/a\" %u\n"), "{spaced}");
      assert!(entry.contains("NoDisplay=true\n"));
      assert!(entry.contains(&format!("{GENERATED_KEY}=true\n")));
      assert!(render_entry("A", "a.b", "/x\n/y", &[]).is_err());
    }
  }
}
