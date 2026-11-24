// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: the kernel sandbox of a packaged desktop app (DENEXT.md item
//! 489): what is decided before anything is applied.
//!
//! A desktop app's process hosts a web engine, so whether the kernel sandbox
//! can hold it depends on the engine and the machine, not only on the mode:
//!
//! | engine | macOS | Linux |
//! | --- | --- | --- |
//! | the OS web view | WKWebView: confined (its web content, network and GPU processes are XPC services outside the profile) | WebKitGTK: confined, when the host calls the runtime before it starts a thread |
//! | CEF | **headless-only**: Chromium's helpers apply their own Seatbelt profile and macOS doesn't stack one | confined where Chromium's namespace sandbox works; **headless-only** where only its setuid helper would (`no_new_privs` disables it) |
//! | GPUI (no web engine) | confined (Metal's shader compiler is an XPC service outside the profile) | confined, when the host calls the runtime before it starts a thread (`laufey_gpui` does) |
//!
//! Windows has no enforcer for desktop apps (`none`).
//!
//! `headless-only` means the app's own process is not confined: the kernel
//! holds only what runs without the engine (the app's forks of itself, a
//! program compiled without a window). The app still starts: a strict
//! package says so on stderr and in the capability probe
//! ([`DesktopReport`]), it does not refuse.
//!
//! This module is pure but for [`engine_dirs_present`] and
//! [`exec_helpers`], which look at the file system.

use std::path::Path;
use std::path::PathBuf;

use deno_runtime::deno_permissions::PermissionsOptions;
use deno_sandbox::plan::EnforcePlan;
use deno_sandbox::plan::FsRule;
use deno_sandbox::plan::SbFilter;
use deno_sandbox::plan::SbScope;
use serde::Serialize;

use crate::kernel_policy::TargetOs;
use crate::kernel_sandbox::Engine;

/// What holds a packaged desktop app's process, as the capability probe
/// reports it (`Deno.desktop.platformFeatures()`: `kernelSandbox`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DesktopSandbox {
  /// The kernel holds the app's process, its native code and its children
  /// to the baked grant.
  #[serde(rename = "kernel")]
  Kernel,
  /// The app's own process isn't confined (its engine can't run inside the
  /// sandbox here); only what runs without the engine is.
  #[serde(rename = "headless-only")]
  HeadlessOnly,
  /// No kernel sandbox: turned off, a development build, nothing to
  /// narrow, `auto` (which applies nothing to an app that isn't packaged
  /// strict), or a system without an enforcer for desktop apps.
  #[serde(rename = "none")]
  None,
}

impl DesktopSandbox {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Kernel => "kernel",
      Self::HeadlessOnly => "headless-only",
      Self::None => "none",
    }
  }
}

/// A desktop app's kernel-sandbox state and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopReport {
  pub sandbox: DesktopSandbox,
  pub reason: String,
  pub engine: Engine,
  /// What the kernel leaves to Deno's checks in this app, one line each.
  pub residuals: Vec<String>,
}

/// What [`engine_nests`] decides from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineFacts {
  pub os: TargetOs,
  pub engine: Engine,
  /// This runtime has an enforcer for this OS and architecture.
  pub enforcer_built: bool,
  /// The process has exactly one thread: nothing of the engine started yet
  /// (Linux: Landlock confines the calling thread and the threads it makes
  /// later, never the ones that exist).
  pub single_threaded: bool,
  /// An unprivileged process can make a user namespace here (Chromium's
  /// namespace sandbox).
  pub user_namespaces: bool,
  /// The app is a fork of itself with no engine (a worker, a sidecar).
  pub worker: bool,
}

/// Why a desktop app's own process isn't confined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotNested {
  pub sandbox: DesktopSandbox,
  pub reason: String,
}

/// Whether the app's process can be confined in place with its engine in
/// it. `Err` says what the app gets instead and why; it is never a refusal
/// to start.
pub fn engine_nests(facts: &EngineFacts) -> Result<(), NotNested> {
  let not = |sandbox, reason: &str| {
    Err(NotNested {
      sandbox,
      reason: reason.to_string(),
    })
  };
  let confinable_os = matches!(facts.os, TargetOs::Linux | TargetOs::Macos);
  if !confinable_os || !facts.enforcer_built {
    return not(
      DesktopSandbox::None,
      "this runtime has no kernel enforcer for desktop apps on this system; Deno's permission checks hold the app",
    );
  }
  if facts.worker {
    // no engine in this process: it confines itself like any compiled
    // program (or inherits the app's sandbox)
    return Ok(());
  }
  match (facts.os, facts.engine) {
    (TargetOs::Macos, Engine::Cef) => not(
      DesktopSandbox::HeadlessOnly,
      "CEF on macOS: Chromium's helper processes apply their own Seatbelt profile and macOS doesn't stack a second one, so the app's own process isn't confined",
    ),
    (TargetOs::Macos, Engine::Webview) => Ok(()),
    // no web engine and no helper process: a Seatbelt profile holds every
    // thread of the process at once
    (TargetOs::Macos, Engine::Gpui) => Ok(()),
    (TargetOs::Linux, _) if !facts.single_threaded => not(
      DesktopSandbox::HeadlessOnly,
      "the app's host started its web engine before the runtime could confine the process (this host doesn't call the runtime first), and Linux confines only threads made afterwards",
    ),
    (TargetOs::Linux, Engine::Cef) if !facts.user_namespaces => not(
      DesktopSandbox::HeadlessOnly,
      "CEF with only Chromium's setuid sandbox helper: unprivileged user namespaces are restricted here, and the kernel sandbox's no_new_privs would disable the helper and leave web content unsandboxed, so the app's own process isn't confined",
    ),
    (TargetOs::Linux, _) => Ok(()),
    _ => unreachable!("only Linux and macOS reach the engine table"),
  }
}

/// What a desktop app's package gets for `os` and `engine`, as far as it is
/// known when the app is packaged (`deno compile` / `deno desktop` print
/// it): the sandbox and one line. Linux CEF depends on the machine it runs
/// on.
pub fn packaged_guarantee(
  os: TargetOs,
  engine: Engine,
) -> (DesktopSandbox, &'static str) {
  match (os, engine) {
    (TargetOs::Macos, Engine::Webview) => (
      DesktopSandbox::Kernel,
      "the app's process is confined in place (WKWebView); WebKit's own web content, network and GPU processes stay outside the profile",
    ),
    (TargetOs::Macos, Engine::Cef) => (
      DesktopSandbox::HeadlessOnly,
      "CEF on macOS can't run inside a Seatbelt profile: the app's own process is not confined (headless guarantees only)",
    ),
    (TargetOs::Linux, Engine::Webview) => (
      DesktopSandbox::Kernel,
      "the app's process is confined in place (WebKitGTK) by a host that calls the runtime before its engine starts",
    ),
    (TargetOs::Linux, Engine::Cef) => (
      DesktopSandbox::Kernel,
      "the app's process is confined in place where Chromium's namespace sandbox is available; on a machine with only its setuid helper (Ubuntu 23.10 and later by default) the app's own process is not confined (headless guarantees only)",
    ),
    (TargetOs::Macos, Engine::Gpui) => (
      DesktopSandbox::Kernel,
      "the app's process is confined in place (GPUI: no web engine); Metal's shader compiler is an XPC service outside the profile",
    ),
    (TargetOs::Linux, Engine::Gpui) => (
      DesktopSandbox::Kernel,
      "the app's process is confined in place (GPUI: no web engine) by a host that calls the runtime before it starts a thread",
    ),
    _ => (
      DesktopSandbox::None,
      "desktop apps have no kernel enforcer on this system: Deno's permission checks hold the app",
    ),
  }
}

// ---------------------------------------------------------------------------
// What the engine needs
// ---------------------------------------------------------------------------

/// The folders a desktop app's engine paths are built from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineDirs {
  pub home: Option<PathBuf>,
  /// The app identifier (the bundle id / the `.desktop` id): the engine
  /// keeps its web data in folders named after it.
  pub app_id: Option<String>,
  /// The app's data folder (`$DENO_DESKTOP_DATA`; the engine's profile is
  /// beneath it).
  pub data: Option<PathBuf>,
  /// The app's cache folder in the OS cache folder (the parent of
  /// `$DENO_DESKTOP_CACHE`).
  pub cache: Option<PathBuf>,
  /// The temp folder.
  pub temp: Option<PathBuf>,
  /// macOS: the per-user folder that holds the temp and cache folders
  /// (`/private/var/folders/xx/yyyy`).
  pub user_folder: Option<PathBuf>,
  /// The install: the `.app` bundle or the folder of the executable (the
  /// runtime library, the engine's resources and helper programs).
  pub install: Option<PathBuf>,
  /// Linux: `$XDG_RUNTIME_DIR` (the Wayland, D-Bus, PipeWire and
  /// accessibility sockets; dconf's shared memory, PulseAudio's and
  /// WebKitGTK's own folders).
  pub runtime_dir: Option<PathBuf>,
  /// Linux: `$XAUTHORITY`.
  pub xauthority: Option<PathBuf>,
  /// Linux: `$XDG_CONFIG_HOME` and `$XDG_CACHE_HOME` and `$XDG_DATA_HOME`
  /// (`None`: the defaults under the home folder).
  pub config_home: Option<PathBuf>,
  pub cache_home: Option<PathBuf>,
  pub data_home: Option<PathBuf>,
}

/// The paths a web engine in the app's process needs whatever the grant.
/// Each is for the kernel only: Deno's own checks are not widened by them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnginePaths {
  pub read: Vec<PathBuf>,
  /// Read and written.
  pub write: Vec<PathBuf>,
  /// Linux: folders whose programs the engine runs (its helper processes),
  /// whatever the run grant.
  pub exec: Vec<PathBuf>,
}

/// Linux: where distributions install a library's helper programs.
const LINUX_LIBEXEC_BASES: &[&str] = &[
  "/usr/libexec",
  "/usr/lib",
  "/usr/lib64",
  "/usr/lib/x86_64-linux-gnu",
  "/usr/lib/aarch64-linux-gnu",
];

/// Linux: the helper folders beneath each of [`LINUX_LIBEXEC_BASES`]
/// (WebKitGTK's web, network and GPU processes; GStreamer's plugin
/// scanner).
const LINUX_HELPER_DIRS: &[&str] = &[
  "webkit2gtk-4.1",
  "webkit2gtk-4.0",
  "webkitgtk-6.0",
  "gstreamer-1.0",
  "gstreamer1.0/gstreamer-1.0",
];

/// The engine's paths for `os` and `engine` (pure: nothing is checked for
/// existence).
///
/// [`Engine::Gpui`] has no web engine. It keeps the OS toolkit's own reads
/// (AppKit and CoreText on macOS; the display server, fonts and cursor
/// themes on Linux) and the GPU, and drops what only a web engine needs:
/// WebKit's and Chromium's data folders, the keychain (the GPUI host has no
/// secure store), spelling dictionaries, GTK, dconf, GStreamer and
/// PulseAudio, the sound devices, and the web engines' helper programs.
/// What GPUI adds is narrow: on macOS, Metal's shader cache in the app's
/// own folder of the per-user cache (`<user folder>/C/<app id>`).
///
/// For every engine the write grant stays in the app's own places: on
/// macOS the app's folder of the per-user cache and the temp folder, never
/// the per-user folder whole; on Linux no shared shader cache
/// (`~/.cache/mesa_shader_cache`, `~/.nv`) is writable (Mesa and NVIDIA
/// then run without a disk cache: slower first frames, nothing else, rather
/// than one app writing a cache every GPU client of the user loads),
/// Chromium's shared certificate database (`~/.pki`) and GStreamer's
/// registry are read only, and `$XDG_RUNTIME_DIR` is read with only the
/// toolkits' own folders written.
///
/// Not narrowed here: a Unix socket is connected to whatever the file
/// rules say (Landlock holds no rule for it; the plan's socket policy does),
/// so the session bus stays reachable on Linux, and with it what the
/// session's services do for a caller: `systemd --user` starts a program
/// outside the sandbox on request (`StartTransientUnit`). A confined Linux
/// desktop app is held for files, programs it starts itself and the
/// network, not against the session's services.
pub fn engine_paths(
  os: TargetOs,
  engine: Engine,
  dirs: &EngineDirs,
) -> EnginePaths {
  let mut paths = EnginePaths::default();
  let abs = |p: &&str| PathBuf::from(*p);
  // a web engine (WebKit, Chromium) rather than the GPUI host
  let web = engine != Engine::Gpui;
  match os {
    TargetOs::Macos => {
      // system files AppKit, CoreText, ColorSync and WebKit read (the
      // frameworks themselves are in the runtime's implicit set); GPUI
      // goes through the same AppKit and CoreText, so it reads them too
      paths.read.extend(
        [
          "/Library",
          "/usr",
          "/private/etc",
          "/private/var/db",
          "/private/var/run",
          "/dev",
        ]
        .iter()
        .map(abs),
      );
      if let Some(home) = &dirs.home {
        let library = home.join("Library");
        // AppKit's preferences, fonts, keyboard layouts and input methods
        // (IME), colors and key bindings, for every engine
        for name in [
          "Preferences",
          "Fonts",
          "Keyboard Layouts",
          "Input Methods",
          "Colors",
          "ColorSync",
          "KeyBindings",
        ] {
          paths.read.push(library.join(name));
        }
        if web {
          // WebKit's spell checking
          for name in ["Spelling", "Dictionaries"] {
            paths.read.push(library.join(name));
          }
          // the login keychain's file is read and written by the app's own
          // process (the items stay protected by securityd)
          paths.write.push(library.join("Keychains"));
        }
        if let Some(id) = &dirs.app_id {
          if web {
            paths.write.extend([
              library.join("Caches").join(id),
              library.join("WebKit").join(id),
              library.join("HTTPStorages").join(id),
              library
                .join("HTTPStorages")
                .join(format!("{id}.binarycookies")),
              library.join("Cookies").join(format!("{id}.binarycookies")),
            ]);
          }
          // AppKit's window restoration, for every engine
          paths.write.push(
            library
              .join("Saved Application State")
              .join(format!("{id}.savedState")),
          );
        }
      }
      if let (Some(user_folder), Some(id)) = (&dirs.user_folder, &dirs.app_id) {
        // The app's own folder of the per-user cache,
        // `<DARWIN_USER_CACHE_DIR>/<bundle id>` (the packaged app's bundle
        // id is its identifier): Metal's shader cache (`com.apple.metal`)
        // and WebKit's and CFNetwork's caches, for every engine. The temp
        // folder (`T`) is granted below like every engine's. Never the
        // per-user folder whole: `0` and the rest of `C` hold every other
        // app's caches and the per-user daemons' state (the LaunchServices
        // database among them).
        paths.write.push(user_folder.join("C").join(id));
      }
    }
    TargetOs::Linux => {
      // toolkits, fonts, themes, GPU and device enumeration
      paths.read.extend(
        ["/usr", "/etc", "/sys", "/proc", "/var/cache/fontconfig"]
          .iter()
          .map(abs),
      );
      // the X11 socket folder and DMA-BUF heaps (GPU buffers), for every
      // engine
      paths
        .read
        .extend(["/tmp/.X11-unix", "/dev/dma_heap"].iter().map(abs));
      if web {
        // udev's device database, the system bus, the session manager's
        // ICE sockets (GTK, GStreamer, WebKit's helpers)
        paths.read.extend(
          ["/run/udev", "/run/dbus", "/var/lib/dbus", "/tmp/.ICE-unix"]
            .iter()
            .map(abs),
        );
      }
      // the GPU (Mesa's DRM nodes, udmabuf, NVIDIA's nodes), for every
      // engine
      paths.write.extend(
        [
          "/dev/dri",
          "/dev/udmabuf",
          "/dev/nvidiactl",
          "/dev/nvidia0",
          "/dev/nvidia-uvm",
          "/dev/nvidia-uvm-tools",
          "/dev/nvidia-modeset",
        ]
        .iter()
        .map(abs),
      );
      if web {
        // sound (WebKit's and Chromium's media)
        paths.write.push(PathBuf::from("/dev/snd"));
      }
      // `$XDG_RUNTIME_DIR`: read (the Wayland, session bus, PipeWire and
      // accessibility sockets are connected to, the display's authority
      // files read), never written whole: it also holds the systemd user
      // manager's runtime units and other programs' sockets and state.
      // Written beneath it: what the toolkits keep there themselves
      // (dconf's shared memory, PulseAudio's runtime files, WebKitGTK's
      // own folder), for a web engine.
      paths.read.extend(dirs.runtime_dir.clone());
      if web && let Some(runtime_dir) = &dirs.runtime_dir {
        for name in ["dconf", "pulse", "webkitgtk"] {
          paths.write.push(runtime_dir.join(name));
        }
      }
      paths.read.extend(dirs.xauthority.clone());
      if let Some(home) = &dirs.home {
        let config = dirs.config_home.clone().unwrap_or(home.join(".config"));
        let cache = dirs.cache_home.clone().unwrap_or(home.join(".cache"));
        let data = dirs
          .data_home
          .clone()
          .unwrap_or(home.join(".local").join("share"));
        // fonts and cursor themes, for every engine
        paths.read.push(config.join("fontconfig"));
        for name in ["fonts", "icons"] {
          paths.read.push(data.join(name));
        }
        for name in [".fonts", ".fonts.conf", ".icons", ".Xauthority"] {
          paths.read.push(home.join(name));
        }
        paths.write.push(cache.join("fontconfig"));
        if web {
          for name in [
            "gtk-3.0",
            "gtk-4.0",
            "dconf",
            "pulse",
            "ibus",
            "user-dirs.dirs",
            "mimeapps.list",
            "kdeglobals",
          ] {
            paths.read.push(config.join(name));
          }
          for name in ["themes", "mime", "glib-2.0"] {
            paths.read.push(data.join(name));
          }
          paths.read.push(home.join(".themes"));
          // GStreamer's plugin registry is read; without write it is not
          // updated (plugins are scanned again at each start). No shared
          // GPU shader cache (`mesa_shader_cache`, `nvidia`, `~/.nv`) is
          // written, as for the GPUI host: every GPU client of the user
          // loads what is in them.
          paths.read.push(cache.join("gstreamer-1.0"));
        }
        if engine == Engine::Cef {
          // Chromium's certificate database (NSS, which the user's
          // browsers share): the authorities and client certificates the
          // user added are read; Chromium can't open it for writing and
          // goes on without a persistent database of its own
          paths.read.push(home.join(".pki"));
        }
      }
      if engine == Engine::Cef {
        // Chromium's namespace sandbox writes its id maps in each new
        // process's own /proc entry, which only a rule on /proc covers (a
        // rule on /proc/self names this process's entry alone)
        paths.write.push(PathBuf::from("/proc"));
      }
      if web {
        for base in LINUX_LIBEXEC_BASES {
          for helper in LINUX_HELPER_DIRS {
            paths.exec.push(Path::new(base).join(helper));
          }
        }
      }
      // CEF's helper processes are the executable itself, started again;
      // every engine's app starts itself again for its workers, sidecars
      // and the updater's helper
      paths.exec.extend(dirs.install.clone());
    }
    _ => return paths,
  }
  // the app's own folders and the temp folder: the engine's profile and
  // caches, scratch files, the embedded file system's code cache
  paths.write.extend(dirs.data.clone());
  paths.write.extend(dirs.cache.clone());
  paths.write.extend(dirs.temp.clone());
  // the install is read (and code is loaded from it), never written
  paths.read.extend(dirs.install.clone());
  paths.read.sort();
  paths.read.dedup();
  paths.write.sort();
  paths.write.dedup();
  paths.exec.sort();
  paths.exec.dedup();
  paths
}

/// `paths` with only what exists (Linux: a Landlock rule names an open
/// file, so a path that doesn't exist can have none).
pub fn engine_dirs_present(mut paths: EnginePaths) -> EnginePaths {
  #[allow(
    clippy::disallowed_methods,
    reason = "the sandbox acts on the real file system"
  )]
  let present = |p: &PathBuf| p.exists();
  paths.read.retain(present);
  paths.write.retain(present);
  paths.exec.retain(present);
  paths
}

/// The grant the kernel holds a desktop app to, and what it leaves to
/// Deno's checks.
#[derive(Debug, Clone)]
pub struct DesktopGrant {
  pub options: PermissionsOptions,
  pub residuals: Vec<String>,
}

fn extend_scope(scope: &mut Option<Vec<String>>, paths: &[PathBuf]) {
  if paths.is_empty() {
    return;
  }
  match scope {
    // unscoped: nothing to add
    Some(list) if list.is_empty() => {}
    Some(list) => {
      for path in paths {
        let entry = path.to_string_lossy().into_owned();
        if !list.contains(&entry) {
          list.push(entry);
        }
      }
    }
    None => {
      *scope = Some(
        paths
          .iter()
          .map(|p| p.to_string_lossy().into_owned())
          .collect(),
      );
    }
  }
}

/// The kernel's grant for a desktop app: `options` (the baked permissions
/// as the runtime applies them, without the loopback hosts it adds for the
/// app's own servers) widened by what the engine needs.
///
/// The network is not narrowed by host in a desktop app. A host-scoped
/// grant needs the supervising broker, which holds a process with no IP
/// socket of its own: a process that hosts a web engine can't be that
/// process. So any network grant leaves the kernel's network open (a
/// residual, reported). With no network grant at all, macOS keeps the app
/// to the loopback interface (the Seatbelt desktop profile); Linux can't
/// (Landlock holds ports, not addresses, and the engine's processes are
/// inside the domain), and neither can a fork of the app, whose profile has
/// no loopback rule.
pub fn desktop_grant(
  options: &PermissionsOptions,
  os: TargetOs,
  paths: &EnginePaths,
  worker: bool,
) -> DesktopGrant {
  let mut options = options.clone();
  let mut residuals = Vec::new();
  let unscoped =
    |list: &Option<Vec<String>>| list.as_ref().is_some_and(|l| l.is_empty());
  let any_net = options.allow_net.is_some()
    || options.allow_net_connect.is_some()
    || options.allow_net_listen.is_some();
  let has_net_deny = options.deny_net.is_some()
    || options.deny_net_connect.is_some()
    || options.deny_net_listen.is_some();
  let loopback_only = !any_net && os == TargetOs::Macos && !worker;
  if !loopback_only {
    let fully_open = unscoped(&options.allow_net) && !has_net_deny;
    if !fully_open {
      residuals.push(if any_net {
        "network: the app's --allow-net / --deny-net rules are held by Deno's checks only; the kernel leaves the network open (host rules need the supervising broker, which a process hosting a web engine can't run under)".to_string()
      } else {
        "network: the app has no network grant, and Deno's checks refuse it; the kernel leaves the network open here (the page's loopback relay and the web engine's processes need IP sockets, and this system can't hold them to the loopback interface)".to_string()
      });
    }
    options.allow_net = Some(Vec::new());
    options.allow_net_connect = None;
    options.allow_net_listen = None;
    options.deny_net = None;
    options.deny_net_connect = None;
    options.deny_net_listen = None;
  }
  let mut read = paths.read.clone();
  read.extend(paths.write.iter().cloned());
  extend_scope(&mut options.allow_read, &read);
  extend_scope(&mut options.allow_write, &paths.write);
  // the set is fixed: no prompt can widen it
  options.prompt = false;
  DesktopGrant { options, residuals }
}

/// What stays out of the kernel's hands in every confined desktop app, for
/// the report (the guarantee matrix lists the same).
pub fn standing_residuals(os: TargetOs, engine: Engine) -> Vec<String> {
  let mut out = Vec::new();
  match os {
    TargetOs::Macos if engine == Engine::Gpui => {
      out.push("GPU: Metal's shader compiler is an XPC service outside the app's profile, and the GPU (IOKit) is not narrowed".to_string());
      out.push("mach services, sysctl reads and signals are not narrowed (AppKit and Metal use them)".to_string());
    }
    TargetOs::Macos => {
      out.push("web engine: WebKit's web content, network and GPU processes are XPC services outside the app's profile; native code can drive the web view to fetch what the web view may reach".to_string());
      out.push("mach services, sysctl reads and signals are not narrowed (AppKit and WebKit use them)".to_string());
    }
    TargetOs::Linux => {
      out.push("session bus: the D-Bus session bus stays reachable (portals, notifications, the tray, the Secret Service), and a service on it can start programs outside the sandbox".to_string());
      out.push("Unix sockets: every socket the user can reach by file mode is reachable (X11, Wayland, PipeWire)".to_string());
      if engine == Engine::Cef {
        out.push("namespaces: user, PID and network namespaces can be made (Chromium's namespace sandbox), and /proc is writable (its id maps)".to_string());
      }
    }
    _ => {}
  }
  out
}

// ---------------------------------------------------------------------------
// The plan a desktop app's process applies
// ---------------------------------------------------------------------------

/// The variables a desktop app's environment keeps under an environment
/// scrub besides the plan's own (a trailing `*`: a prefix): the session,
/// the display, the toolkits and the host's launch configuration. Without
/// them no window opens.
pub const DESKTOP_ENV_KEEP: &[&str] = &[
  "DISPLAY",
  "WAYLAND_DISPLAY",
  "WAYLAND_SOCKET",
  "XAUTHORITY",
  "XDG_*",
  "DBUS_*",
  "DESKTOP_SESSION",
  "SESSION_MANAGER",
  "GTK_*",
  "GDK_*",
  "GIO_*",
  "GSETTINGS_*",
  "G_MESSAGES_DEBUG",
  "QT_*",
  "XMODIFIERS",
  "XCURSOR_*",
  "IBUS_*",
  "AT_SPI_*",
  "NO_AT_BRIDGE",
  "WEBKIT_*",
  "GST_*",
  "FONTCONFIG_*",
  "LIBGL_*",
  "MESA_*",
  "EGL_*",
  "__GL*",
  "__NV*",
  "__EGL*",
  "VK_*",
  "PULSE_*",
  "PIPEWIRE_*",
  "LAUFEY_*",
  "DENO_DESKTOP_*",
  "APPIMAGE",
  "APPDIR",
  "ARGV0",
  "OWD",
  // an AppImage's launcher points the loader at the libraries it carries
  "LD_LIBRARY_PATH",
  "LIBVA_*",
  "USER",
  "LOGNAME",
  "SHELL",
  "COLORTERM",
  "__CF*",
  "CFFIXED_USER_HOME",
  "XPC_*",
  "COMMAND_MODE",
];

fn sb_covers(scope: &SbScope, path: &str) -> bool {
  match scope {
    SbScope::All => true,
    SbScope::Filters { filters } => filters.iter().any(|f| match f {
      SbFilter::Subpath(root) => Path::new(path).starts_with(root),
      SbFilter::Literal(file) => file == path,
      SbFilter::Regex(_) => false,
    }),
  }
}

/// Makes `plan` (a headless program's, compiled for the desktop grant) the
/// plan of a process that hosts a web engine:
///
/// - Linux: Unix sockets and route netlink stay (the display, the session
///   bus, the toolkits' network monitor); abstract sockets aren't scoped
///   (X11's and some session buses' are abstract); signals aren't limited
///   to the process itself and `execve` isn't refused outright (the engine
///   starts and signals its helper processes); with no run grant, only the
///   programs beneath `exec` may be executed.
/// - macOS: code may also be mapped from the install (the runtime library
///   and the engine's frameworks), unless the write grant covers it.
/// - both: an environment scrub keeps [`DESKTOP_ENV_KEEP`]; a scrub is
///   dropped altogether when the process already has other threads
///   (`scrub`: false), since removing variables under them is a data race.
pub fn desktop_plan(
  plan: &mut EnforcePlan,
  exec: &[PathBuf],
  install: Option<&Path>,
  scrub: bool,
) {
  if let Some(linux) = &mut plan.linux {
    linux.seccomp.unix_sockets = true;
    linux.seccomp.netlink_route = true;
    linux.seccomp.signal_self = false;
    linux.seccomp.refuse_exec = false;
    linux.scope_abstract_unix = false;
    if linux.handled.exec {
      for path in exec {
        match linux.rules.iter_mut().find(|r| &r.path == path) {
          Some(rule) => {
            rule.read = true;
            rule.exec = true;
          }
          None => linux.rules.push(FsRule {
            path: path.clone(),
            read: true,
            write: false,
            exec: true,
          }),
        }
      }
      linux.rules.sort();
    }
  }
  if let Some(macos) = &mut plan.macos
    && let Some(install) = install.and_then(|p| p.to_str())
    && let SbScope::Filters { filters } = &mut macos.map_exec
    && !sb_covers(&macos.write, install)
  {
    let filter = SbFilter::Subpath(install.to_string());
    if !filters.contains(&filter) {
      filters.push(filter);
    }
  }
  match &mut plan.env_keep {
    Some(_) if !scrub => plan.env_keep = None,
    Some(keep) => {
      for name in DESKTOP_ENV_KEEP {
        if !keep.iter().any(|k| k == name) {
          keep.push((*name).to_string());
        }
      }
    }
    None => {}
  }
}

/// The helper folders of [`EnginePaths::exec`] that exist.
pub fn exec_helpers(paths: &EnginePaths) -> Vec<PathBuf> {
  engine_dirs_present(EnginePaths {
    exec: paths.exec.clone(),
    ..Default::default()
  })
  .exec
}

/// Whether the baked run grant lets the app run `program` (by its name or
/// its path): the outside helper opens URLs only for an app that may run
/// the system opener.
pub fn run_grant_names(options: &PermissionsOptions, program: &str) -> bool {
  let name = Path::new(program)
    .file_name()
    .and_then(|n| n.to_str())
    .unwrap_or(program);
  let names = |list: &Vec<String>| {
    list.iter().any(|entry| entry == program || entry == name)
  };
  let allowed = match &options.allow_run {
    None => false,
    Some(list) if list.is_empty() => true,
    Some(list) => names(list),
  };
  let denied = match &options.deny_run {
    None => false,
    Some(list) if list.is_empty() => true,
    Some(list) => names(list),
  };
  allowed && !denied
}

#[cfg(test)]
mod tests {
  use deno_sandbox::plan::Handled;
  use deno_sandbox::plan::LinuxPlan;
  use deno_sandbox::plan::MacosPlan;
  use deno_sandbox::plan::NetPlan;
  use deno_sandbox::plan::PLAN_FORMAT;
  use deno_sandbox::plan::SbUnix;
  use deno_sandbox::plan::SeccompPlan;

  use super::*;

  fn facts(os: TargetOs, engine: Engine) -> EngineFacts {
    EngineFacts {
      os,
      engine,
      enforcer_built: true,
      single_threaded: true,
      user_namespaces: true,
      worker: false,
    }
  }

  // the engine table (plan §1.1): which engines the app's process can be
  // confined with, and what the others are reported as
  #[test]
  fn the_engine_table() {
    use TargetOs::*;
    assert_eq!(engine_nests(&facts(Macos, Engine::Webview)), Ok(()));
    assert_eq!(engine_nests(&facts(Linux, Engine::Webview)), Ok(()));
    assert_eq!(engine_nests(&facts(Linux, Engine::Cef)), Ok(()));
    // macOS CEF: headless-only, whatever the machine
    let not = engine_nests(&facts(Macos, Engine::Cef)).unwrap_err();
    assert_eq!(not.sandbox, DesktopSandbox::HeadlessOnly);
    assert!(not.reason.contains("CEF on macOS"), "{}", not.reason);
    // Linux CEF with only the setuid helper: headless-only
    let not = engine_nests(&EngineFacts {
      user_namespaces: false,
      ..facts(Linux, Engine::Cef)
    })
    .unwrap_err();
    assert_eq!(not.sandbox, DesktopSandbox::HeadlessOnly);
    assert!(
      not.reason.contains("setuid sandbox helper"),
      "{}",
      not.reason
    );
    // WebKitGTK needs no user namespace
    assert_eq!(
      engine_nests(&EngineFacts {
        user_namespaces: false,
        ..facts(Linux, Engine::Webview)
      }),
      Ok(())
    );
    // Linux, the engine already started (a host that doesn't call first):
    // never a partial confinement
    for engine in [Engine::Webview, Engine::Cef] {
      let not = engine_nests(&EngineFacts {
        single_threaded: false,
        ..facts(Linux, engine)
      })
      .unwrap_err();
      assert_eq!(not.sandbox, DesktopSandbox::HeadlessOnly);
      assert!(not.reason.contains("before the runtime"), "{}", not.reason);
    }
    // macOS applies process-wide: threads don't matter
    assert_eq!(
      engine_nests(&EngineFacts {
        single_threaded: false,
        ..facts(Macos, Engine::Webview)
      }),
      Ok(())
    );
    // no enforcer: none
    for f in [
      facts(Windows, Engine::Webview),
      facts(Windows, Engine::Cef),
      facts(Other, Engine::Webview),
      EngineFacts {
        enforcer_built: false,
        ..facts(Linux, Engine::Webview)
      },
    ] {
      assert_eq!(engine_nests(&f).unwrap_err().sandbox, DesktopSandbox::None);
    }
    // a fork of the app has no engine: it nests wherever there is an
    // enforcer, macOS CEF included
    for engine in [Engine::Webview, Engine::Cef] {
      for os in [Macos, Linux] {
        assert_eq!(
          engine_nests(&EngineFacts {
            worker: true,
            single_threaded: false,
            user_namespaces: false,
            ..facts(os, engine)
          }),
          Ok(())
        );
      }
    }
    assert_eq!(
      engine_nests(&EngineFacts {
        worker: true,
        ..facts(Windows, Engine::Cef)
      })
      .unwrap_err()
      .sandbox,
      DesktopSandbox::None
    );
  }

  #[test]
  fn what_a_package_is_told() {
    use DesktopSandbox::*;
    use TargetOs::*;
    assert_eq!(packaged_guarantee(Macos, Engine::Webview).0, Kernel);
    assert_eq!(packaged_guarantee(Macos, Engine::Cef).0, HeadlessOnly);
    assert_eq!(packaged_guarantee(Linux, Engine::Webview).0, Kernel);
    let (sandbox, line) = packaged_guarantee(Linux, Engine::Cef);
    assert_eq!(sandbox, Kernel);
    assert!(line.contains("setuid helper"), "{line}");
    assert_eq!(packaged_guarantee(Windows, Engine::Cef).0, None);
    assert_eq!(
      serde_json::to_string(&[Kernel, HeadlessOnly, None]).unwrap(),
      r#"["kernel","headless-only","none"]"#
    );
    assert_eq!(HeadlessOnly.as_str(), "headless-only");
  }

  fn dirs() -> EngineDirs {
    EngineDirs {
      home: Some("/home/u".into()),
      app_id: Some("com.acme.app".into()),
      data: Some("/home/u/.local/share/com.acme.app".into()),
      cache: Some("/home/u/.cache/com.acme.app".into()),
      temp: Some("/tmp".into()),
      user_folder: None,
      install: Some("/opt/acme".into()),
      runtime_dir: Some("/run/user/1000".into()),
      xauthority: Some("/run/user/1000/xauth".into()),
      config_home: None,
      cache_home: None,
      data_home: None,
    }
  }

  #[test]
  fn linux_engine_paths() {
    let webview = engine_paths(TargetOs::Linux, Engine::Webview, &dirs());
    let p = |s: &str| PathBuf::from(s);
    for read in [
      "/usr",
      "/etc",
      "/sys",
      "/proc",
      "/opt/acme",
      "/home/u/.config/fontconfig",
      "/run/user/1000/xauth",
    ] {
      assert!(webview.read.contains(&p(read)), "{read}");
    }
    for write in [
      "/tmp",
      "/run/user/1000/dconf",
      "/run/user/1000/pulse",
      "/run/user/1000/webkitgtk",
      "/dev/dri",
      "/home/u/.local/share/com.acme.app",
      "/home/u/.cache/com.acme.app",
      "/home/u/.cache/fontconfig",
    ] {
      assert!(webview.write.contains(&p(write)), "{write}");
    }
    // the install is never written, the home folder never granted whole
    assert!(!webview.write.contains(&p("/opt/acme")));
    assert!(!webview.read.contains(&p("/home/u")));
    assert!(!webview.write.contains(&p("/home/u")));
    // /proc is written only under Chromium, whose sandbox maps ids there
    assert!(!webview.write.contains(&p("/proc")));
    assert!(!webview.read.contains(&p("/home/u/.pki")));
    let cef = engine_paths(TargetOs::Linux, Engine::Cef, &dirs());
    assert!(cef.write.contains(&p("/proc")));
    // the write grant stays in the app's own places: the runtime folder is
    // read (its sockets, the authority files), with the toolkits' own
    // folders written; the shared certificate database and GStreamer's
    // registry are read; no shared shader cache is written
    let gpui = engine_paths(TargetOs::Linux, Engine::Gpui, &dirs());
    for paths in [&webview, &cef, &gpui] {
      assert!(paths.read.contains(&p("/run/user/1000")));
      for shared in [
        "/run/user/1000",
        "/run/user/1000/systemd",
        "/home/u/.pki",
        "/home/u/.nv",
        "/home/u/.cache",
        "/home/u/.cache/mesa_shader_cache",
        "/home/u/.cache/mesa_shader_cache_db",
        "/home/u/.cache/nvidia",
        "/home/u/.cache/gstreamer-1.0",
      ] {
        assert!(!paths.write.contains(&p(shared)), "{shared}");
      }
    }
    assert!(cef.read.contains(&p("/home/u/.pki")));
    assert!(webview.read.contains(&p("/home/u/.cache/gstreamer-1.0")));
    assert!(!gpui.write.iter().any(|w| w.starts_with("/run/user/1000")));
    // the helper programs: WebKitGTK's, and the executable's own folder
    assert!(webview.exec.contains(&p("/usr/libexec/webkit2gtk-4.1")));
    assert!(
      webview
        .exec
        .contains(&p("/usr/lib/x86_64-linux-gnu/webkit2gtk-4.1"))
    );
    assert!(cef.exec.contains(&p("/opt/acme")));
    // XDG overrides move the per-user folders
    let moved = engine_paths(
      TargetOs::Linux,
      Engine::Webview,
      &EngineDirs {
        config_home: Some("/x/config".into()),
        cache_home: Some("/x/cache".into()),
        ..dirs()
      },
    );
    assert!(moved.read.contains(&p("/x/config/gtk-3.0")));
    assert!(moved.write.contains(&p("/x/cache/fontconfig")));
    assert!(!moved.read.contains(&p("/home/u/.config/gtk-3.0")));
  }

  #[test]
  fn macos_engine_paths() {
    let d = EngineDirs {
      home: Some("/Users/u".into()),
      app_id: Some("com.acme.app".into()),
      data: Some("/Users/u/Library/Application Support/com.acme.app".into()),
      cache: Some("/Users/u/Library/Caches/com.acme.app".into()),
      temp: Some("/private/var/folders/ab/cd/T".into()),
      user_folder: Some("/private/var/folders/ab/cd".into()),
      install: Some("/Applications/Acme.app".into()),
      ..Default::default()
    };
    let paths = engine_paths(TargetOs::Macos, Engine::Webview, &d);
    let p = |s: &str| PathBuf::from(s);
    for read in [
      "/Library",
      "/private/etc",
      "/Users/u/Library/Preferences",
      "/Users/u/Library/Fonts",
      "/Applications/Acme.app",
    ] {
      assert!(paths.read.contains(&p(read)), "{read}");
    }
    for write in [
      "/Users/u/Library/Keychains",
      "/Users/u/Library/WebKit/com.acme.app",
      "/Users/u/Library/HTTPStorages/com.acme.app",
      "/Users/u/Library/Caches/com.acme.app",
      "/Users/u/Library/Saved Application State/com.acme.app.savedState",
      "/Users/u/Library/Application Support/com.acme.app",
      "/private/var/folders/ab/cd/C/com.acme.app",
      "/private/var/folders/ab/cd/T",
    ] {
      assert!(paths.write.contains(&p(write)), "{write}");
    }
    // of the per-user folder only the app's own cache folder and the temp
    // folder: never the folder whole, the other apps' caches or the
    // per-user daemons' state
    for shared in [
      "/private/var/folders/ab/cd",
      "/private/var/folders/ab/cd/C",
      "/private/var/folders/ab/cd/0",
    ] {
      assert!(!paths.write.contains(&p(shared)), "{shared}");
    }
    assert!(paths.exec.is_empty());
    // never the Library folder whole, nor another app's web data
    assert!(!paths.read.contains(&p("/Users/u/Library")));
    assert!(!paths.write.contains(&p("/Users/u/Library/WebKit")));
    assert!(!paths.write.contains(&p("/Applications/Acme.app")));
    // without an identifier the engine's own folders can't be named
    let anonymous = engine_paths(
      TargetOs::Macos,
      Engine::Webview,
      &EngineDirs {
        app_id: None,
        ..d.clone()
      },
    );
    assert!(
      !anonymous
        .write
        .iter()
        .any(|w| w.starts_with("/Users/u/Library/WebKit"))
    );
    assert!(
      !anonymous
        .write
        .iter()
        .any(|w| w.starts_with("/private/var/folders/ab/cd/C")
          || w == &p("/private/var/folders/ab/cd"))
    );
    // no enforcer, no paths
    assert_eq!(
      engine_paths(TargetOs::Windows, Engine::Cef, &d),
      EnginePaths::default()
    );
  }

  /// The gpui host (DENEXT.md item 513) gets its own profile: confined on
  /// both OSes, with the toolkit, fonts and GPU, none of a web engine's
  /// folders or helpers, and Metal's shader cache only in the app's own
  /// folder of the per-user cache. It is a strict subset of the web
  /// view's.
  #[test]
  fn gpui_gets_its_own_profile() {
    use TargetOs::*;
    let p = |s: &str| PathBuf::from(s);
    // the engine table and what a package is told
    assert_eq!(engine_nests(&facts(Macos, Engine::Gpui)), Ok(()));
    assert_eq!(engine_nests(&facts(Linux, Engine::Gpui)), Ok(()));
    assert_eq!(
      engine_nests(&EngineFacts {
        single_threaded: false,
        ..facts(Linux, Engine::Gpui)
      })
      .unwrap_err()
      .sandbox,
      DesktopSandbox::HeadlessOnly
    );
    assert_eq!(
      engine_nests(&facts(Windows, Engine::Gpui))
        .unwrap_err()
        .sandbox,
      DesktopSandbox::None
    );
    for os in [Macos, Linux] {
      let (sandbox, line) = packaged_guarantee(os, Engine::Gpui);
      assert_eq!(sandbox, DesktopSandbox::Kernel, "{os:?}");
      assert!(line.contains("GPUI"), "{line}");
    }
    assert!(
      standing_residuals(Macos, Engine::Gpui)
        .iter()
        .all(|r| !r.contains("WebKit"))
    );

    // macOS
    let d = EngineDirs {
      home: Some("/Users/u".into()),
      app_id: Some("com.acme.app".into()),
      data: Some("/Users/u/Library/Application Support/com.acme.app".into()),
      cache: Some("/Users/u/Library/Caches/com.acme.app".into()),
      temp: Some("/private/var/folders/ab/cd/T".into()),
      user_folder: Some("/private/var/folders/ab/cd".into()),
      install: Some("/Applications/Acme.app".into()),
      ..Default::default()
    };
    let gpui = engine_paths(Macos, Engine::Gpui, &d);
    for read in [
      "/Library",
      "/Users/u/Library/Fonts",
      "/Users/u/Library/Input Methods",
      "/Applications/Acme.app",
    ] {
      assert!(gpui.read.contains(&p(read)), "{read}");
    }
    for write in [
      // Metal's shader cache, in the app's own folder only
      "/private/var/folders/ab/cd/C/com.acme.app",
      "/private/var/folders/ab/cd/T",
      "/Users/u/Library/Saved Application State/com.acme.app.savedState",
      "/Users/u/Library/Application Support/com.acme.app",
    ] {
      assert!(gpui.write.contains(&p(write)), "{write}");
    }
    for absent in [
      // never the whole per-user folder, nor its shared cache
      "/private/var/folders/ab/cd",
      "/private/var/folders/ab/cd/C",
      "/Users/u/Library/Keychains",
      "/Users/u/Library/WebKit/com.acme.app",
      "/Users/u/Library/HTTPStorages/com.acme.app",
    ] {
      assert!(!gpui.write.contains(&p(absent)), "{absent}");
    }
    assert!(!gpui.read.contains(&p("/Users/u/Library/Spelling")));
    assert!(gpui.exec.is_empty());
    // without an identifier there is no Metal cache folder to name
    let anonymous = engine_paths(
      Macos,
      Engine::Gpui,
      &EngineDirs {
        app_id: None,
        ..d.clone()
      },
    );
    assert!(
      !anonymous
        .write
        .iter()
        .any(|w| w.starts_with("/private/var/folders/ab/cd/C"))
    );

    // Linux
    let gpui = engine_paths(Linux, Engine::Gpui, &dirs());
    for read in [
      "/usr",
      "/sys",
      "/tmp/.X11-unix",
      "/home/u/.config/fontconfig",
      "/home/u/.local/share/fonts",
      "/home/u/.icons",
      "/run/user/1000/xauth",
    ] {
      assert!(gpui.read.contains(&p(read)), "{read}");
    }
    for write in [
      "/dev/dri",
      "/dev/nvidiactl",
      "/run/user/1000",
      "/home/u/.cache/fontconfig",
      "/home/u/.cache/com.acme.app",
    ] {
      assert!(gpui.write.contains(&p(write)), "{write}");
    }
    for absent in [
      "/dev/snd",
      "/proc",
      "/home/u/.cache/mesa_shader_cache",
      "/home/u/.cache/gstreamer-1.0",
      "/home/u/.nv",
      "/home/u/.pki",
    ] {
      assert!(!gpui.write.contains(&p(absent)), "{absent}");
    }
    for absent in [
      "/run/dbus",
      "/home/u/.config/gtk-3.0",
      "/home/u/.config/dconf",
    ] {
      assert!(!gpui.read.contains(&p(absent)), "{absent}");
    }
    // no web engine helpers; the app may still start itself again
    assert_eq!(gpui.exec, vec![p("/opt/acme")]);

    // strictly narrower than the web view's, on both OSes
    for (os, d) in [(Macos, d.clone()), (Linux, dirs())] {
      let web = engine_paths(os, Engine::Webview, &d);
      let gpui = engine_paths(os, Engine::Gpui, &d);
      let covered = |path: &PathBuf, list: &[PathBuf]| {
        list.iter().any(|w| path.starts_with(w))
      };
      for path in &gpui.read {
        assert!(covered(path, &web.read), "{os:?} read {path:?}");
      }
      for path in &gpui.write {
        assert!(covered(path, &web.write), "{os:?} write {path:?}");
      }
      for path in &gpui.exec {
        assert!(covered(path, &web.exec), "{os:?} exec {path:?}");
      }
      assert!(
        gpui.read.len() + gpui.write.len() + gpui.exec.len()
          < web.read.len() + web.write.len() + web.exec.len(),
        "{os:?}"
      );
    }
  }

  fn grant(read: &[&str]) -> PermissionsOptions {
    PermissionsOptions {
      allow_read: Some(read.iter().map(|s| s.to_string()).collect()),
      prompt: true,
      ..Default::default()
    }
  }

  #[test]
  fn the_kernel_grant_adds_the_engine_and_never_narrows() {
    let paths = EnginePaths {
      read: vec!["/usr".into()],
      write: vec!["/tmp".into()],
      exec: vec![],
    };
    let g = desktop_grant(&grant(&["/data"]), TargetOs::Macos, &paths, false);
    assert_eq!(
      g.options.allow_read,
      Some(vec!["/data".into(), "/usr".into(), "/tmp".into()])
    );
    assert_eq!(g.options.allow_write, Some(vec!["/tmp".to_string()]));
    assert!(!g.options.prompt);
    // macOS, no network grant: left to the profile's loopback rule
    assert_eq!(g.options.allow_net, None);
    assert!(g.residuals.is_empty(), "{:?}", g.residuals);
    // an unscoped grant stays unscoped
    let mut all = grant(&[]);
    all.allow_write = Some(vec![]);
    let g = desktop_grant(&all, TargetOs::Linux, &paths, false);
    assert_eq!(g.options.allow_read, Some(vec![]));
    assert_eq!(g.options.allow_write, Some(vec![]));
    // Linux with no network grant: open, and said
    assert_eq!(g.options.allow_net, Some(vec![]));
    assert_eq!(g.residuals.len(), 1);
    assert!(g.residuals[0].contains("no network grant"));
    // a host-scoped grant, with a deny: open for the kernel, every
    // direction flag and deny dropped from the kernel's view, and said
    let mut hosts = grant(&["/data"]);
    hosts.allow_net_connect = Some(vec!["api.example.com:443".into()]);
    hosts.deny_net = Some(vec!["evil.example".into()]);
    for os in [TargetOs::Macos, TargetOs::Linux] {
      let g = desktop_grant(&hosts, os, &paths, false);
      assert_eq!(g.options.allow_net, Some(vec![]));
      assert_eq!(g.options.allow_net_connect, None);
      assert_eq!(g.options.deny_net, None);
      assert!(g.residuals[0].contains("--allow-net"), "{:?}", g.residuals);
    }
    // an unscoped network grant with no deny is exact: nothing to say
    let mut open = grant(&["/data"]);
    open.allow_net = Some(vec![]);
    let g = desktop_grant(&open, TargetOs::Linux, &paths, false);
    assert!(g.residuals.is_empty());
    // a fork of the app on macOS has no loopback rule: open
    let g = desktop_grant(&grant(&["/data"]), TargetOs::Macos, &paths, true);
    assert_eq!(g.options.allow_net, Some(vec![]));
    // no read grant at all: the engine's paths become the scope
    let none = PermissionsOptions::default();
    let g = desktop_grant(&none, TargetOs::Macos, &paths, false);
    assert_eq!(
      g.options.allow_read,
      Some(vec!["/usr".to_string(), "/tmp".to_string()])
    );
  }

  fn linux_plan(exec_handled: bool) -> EnforcePlan {
    EnforcePlan {
      format: PLAN_FORMAT,
      linux: Some(LinuxPlan {
        handled: Handled {
          read: true,
          write: true,
          exec: exec_handled,
        },
        rules: vec![FsRule {
          path: "/usr/libexec/webkit2gtk-4.1".into(),
          read: true,
          write: false,
          exec: false,
        }],
        expand: Vec::new(),
        expand_write: Vec::new(),
        net: NetPlan::Open,
        prebound: Vec::new(),
        scope_signal: true,
        scope_abstract_unix: true,
        seccomp: SeccompPlan {
          attack_surface: true,
          clone3_enosys: true,
          refuse_exec: true,
          signal_self: true,
          ..Default::default()
        },
      }),
      macos: None,
      windows: None,
      env_keep: Some(vec!["PATH".into(), "HOME".into()]),
      probe: None,
      module_files: Vec::new(),
      expanded: Vec::new(),
      config_env: Vec::new(),
      cache_overlay: None,
      broker: None,
      expanded_write: Vec::new(),
      entries: Vec::new(),
      member_configs: Vec::new(),
      listed_dirs: Vec::new(),
    }
  }

  #[test]
  fn the_desktop_plan_on_linux() {
    let mut plan = linux_plan(true);
    let exec = vec![
      PathBuf::from("/usr/libexec/webkit2gtk-4.1"),
      "/opt/acme".into(),
    ];
    desktop_plan(&mut plan, &exec, None, true);
    let linux = plan.linux.as_ref().unwrap();
    assert!(linux.seccomp.unix_sockets && linux.seccomp.netlink_route);
    assert!(!linux.seccomp.signal_self && !linux.seccomp.refuse_exec);
    assert!(!linux.scope_abstract_unix);
    // what a desktop app keeps of the headless plan
    assert!(linux.seccomp.attack_surface && linux.seccomp.clone3_enosys);
    assert!(linux.scope_signal);
    assert!(linux.handled.exec);
    // the helpers may be executed: an existing rule gains it, a new one is
    // read and exec only
    assert_eq!(linux.rules.len(), 2);
    assert!(linux.rules.iter().all(|r| r.exec && r.read && !r.write));
    // the environment scrub keeps the session
    let keep = plan.env_keep.as_ref().unwrap();
    for name in ["PATH", "DISPLAY", "WAYLAND_DISPLAY", "XDG_*", "LAUFEY_*"] {
      assert!(keep.iter().any(|k| k == name), "{name}");
    }
    assert!(plan.keeps_env("XDG_RUNTIME_DIR"));
    assert!(!plan.keeps_env("AWS_SECRET_ACCESS_KEY"));
    // a run grant: exec isn't handled, so no exec rule is added
    let mut plan = linux_plan(false);
    desktop_plan(&mut plan, &exec, None, true);
    assert_eq!(plan.linux.as_ref().unwrap().rules.len(), 1);
    // other threads exist: no scrub at all
    let mut plan = linux_plan(true);
    desktop_plan(&mut plan, &[], None, false);
    assert_eq!(plan.env_keep, None);
  }

  #[test]
  fn the_desktop_plan_on_macos_maps_code_from_the_install() {
    let macos = |write: SbScope| MacosPlan {
      read: SbScope::All,
      write,
      deny_read: vec![],
      deny_write: vec![],
      reallow_read: vec![],
      reallow_write: vec![],
      map_exec: SbScope::Filters { filters: vec![] },
      exec: false,
      exec_only: vec![],
      net: NetPlan::Denied,
      unix: SbUnix::Denied,
      denied_sysctls: vec![],
      system_ca: false,
      ttys: vec![],
      deny_only: false,
    };
    let with = |write: SbScope| {
      let mut plan = linux_plan(true);
      plan.linux = None;
      plan.macos = Some(macos(write));
      desktop_plan(
        &mut plan,
        &[],
        Some(Path::new("/Applications/Acme.app")),
        true,
      );
      plan.macos.unwrap().map_exec
    };
    let install = SbFilter::Subpath("/Applications/Acme.app".into());
    assert_eq!(
      with(SbScope::Filters {
        filters: vec![SbFilter::Subpath("/Users/u/data".into())]
      }),
      SbScope::Filters {
        filters: vec![install]
      }
    );
    // a write grant over the install: no code from a folder the app writes
    for write in [
      SbScope::All,
      SbScope::Filters {
        filters: vec![SbFilter::Subpath("/Applications".into())],
      },
    ] {
      assert_eq!(with(write), SbScope::Filters { filters: vec![] });
    }
  }

  #[test]
  fn the_opener_needs_a_run_grant() {
    let run = |allow: Option<&[&str]>, deny: Option<&[&str]>| {
      let list =
        |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
      PermissionsOptions {
        allow_run: allow.map(list),
        deny_run: deny.map(list),
        ..Default::default()
      }
    };
    assert!(!run_grant_names(&run(None, None), "/usr/bin/open"));
    assert!(run_grant_names(&run(Some(&[]), None), "/usr/bin/open"));
    assert!(run_grant_names(
      &run(Some(&["open"]), None),
      "/usr/bin/open"
    ));
    assert!(run_grant_names(
      &run(Some(&["/usr/bin/open"]), None),
      "/usr/bin/open"
    ));
    assert!(!run_grant_names(
      &run(Some(&["git"]), None),
      "/usr/bin/open"
    ));
    assert!(!run_grant_names(
      &run(Some(&[]), Some(&["open"])),
      "/usr/bin/open"
    ));
    assert!(!run_grant_names(&run(Some(&["open"]), Some(&[])), "open"));
  }
}
