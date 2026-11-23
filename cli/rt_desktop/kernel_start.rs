// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: the kernel-sandbox startup of a packaged desktop app (DENEXT.md
//! item 489).
//!
//! A desktop app's process hosts its web engine, and the sandbox is applied
//! in place: on Linux it holds only threads made afterwards, so it must be
//! applied before the engine (or anything else) starts one. The runtime is
//! a library the app's host loads, and by the time the host starts it
//! (`laufey_runtime_start`) the engine is running. So the host calls
//! [`laufey_runtime_prestart`](crate::laufey_runtime_prestart) first thing
//! in its `main`: everything that must happen before the sandbox (a pending
//! update, the update watchdog, a self-extracting app's files) happens
//! there, then [`startup`] decides and applies.
//!
//! A host that doesn't know the call (it predates it) starts the runtime as
//! before, and [`startup`] runs from there: macOS can still confine the
//! process (a Seatbelt profile holds every thread at once), Linux can't and
//! reports the app headless-only.
//!
//! What was done before the engine is kept here, so the runtime's own start
//! doesn't do it twice.

#![allow(
  clippy::disallowed_methods,
  reason = "the sandbox acts on the real environment and file system, before the runtime (and its sys) exists"
)]

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::Once;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use deno_lib::kernel_apply::CompiledStart;
use deno_lib::kernel_apply::DesktopExtras;
use deno_lib::kernel_apply::start_desktop_with;
use deno_lib::kernel_desktop;
use deno_lib::kernel_policy::TargetOs;
use deno_lib::kernel_sandbox::Engine;
use deno_lib::kernel_sandbox::KernelSandboxMode;
use denort::binary::StandaloneData;

/// The embedded program, read before the engine started.
static DATA: Mutex<Option<StandaloneData>> = Mutex::new(None);
/// A self-extracting app's files were written before the engine started.
static EXTRACTED: AtomicBool = AtomicBool::new(false);
/// The directory the process started in (before a self-extracting app's
/// `chdir`).
static LAUNCH_CWD: OnceLock<Option<PathBuf>> = OnceLock::new();
/// The engine the host said it is.
static HOST_ENGINE: OnceLock<Option<Engine>> = OnceLock::new();
static STARTED: AtomicBool = AtomicBool::new(false);

/// The engine argument of `laufey_runtime_prestart`: 1 the OS web view, 2
/// CEF, 3 the GPUI host (laufey's `LAUFEY_ENGINE_*`), anything else
/// unknown. An unknown engine (a newer host, or one that doesn't call the
/// prestart) is never "no sandbox": [`startup`] takes the engine baked
/// into the package, else the OS web view's profile.
///
/// 3 = gpui is confirmed by the fork's owner on the condition that no
/// laufey host already passes 3: upstream littledivy/laufey has no
/// `LAUFEY_ENGINE_*` at all, and Brainwires/laufey's `laufey.h` defines
/// only WEBVIEW 1 and CEF 2 (the winit host doesn't call the prestart). The
/// GPUI host must define `LAUFEY_ENGINE_GPUI 3` to match.
pub fn host_engine(value: std::ffi::c_int) -> Option<Engine> {
  match value {
    1 => Some(Engine::Webview),
    2 => Some(Engine::Cef),
    3 => Some(Engine::Gpui),
    _ => None,
  }
}

pub fn set_host_engine(engine: Option<Engine>) {
  let _ = HOST_ENGINE.set(engine);
}

pub fn keep_data(data: StandaloneData) {
  *DATA.lock().unwrap_or_else(|e| e.into_inner()) = Some(data);
}

/// The embedded program the prestart read, once.
pub fn take_data() -> Option<StandaloneData> {
  DATA.lock().unwrap_or_else(|e| e.into_inner()).take()
}

pub fn set_extracted() {
  EXTRACTED.store(true, Ordering::SeqCst);
}

pub fn extracted() -> bool {
  EXTRACTED.load(Ordering::SeqCst)
}

pub fn set_launch_cwd(cwd: Option<PathBuf>) {
  let _ = LAUNCH_CWD.set(cwd);
}

/// The directory the process started in, when the prestart recorded it.
pub fn launch_cwd() -> Option<Option<PathBuf>> {
  LAUNCH_CWD.get().cloned()
}

/// Reads the grants a confined ancestor handed down, once (a fork of a
/// confined app inherits the app's sandbox; `denort`'s `main` does the
/// same).
pub fn load_inherited() {
  static ONCE: Once = Once::new();
  ONCE.call_once(deno_lib::kernel_inherit::load_at_startup);
}

/// The system opener the outside helper runs, when the baked run grant
/// names it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn opener(
  baked: &deno_runtime::deno_permissions::PermissionsOptions,
) -> Option<std::ffi::CString> {
  let candidates: &[&str] = if cfg!(target_os = "macos") {
    &["/usr/bin/open"]
  } else {
    &[
      "/usr/bin/xdg-open",
      "/usr/local/bin/xdg-open",
      "/bin/xdg-open",
    ]
  };
  let path = candidates
    .iter()
    .find(|p| std::path::Path::new(p).is_file())?;
  kernel_desktop::run_grant_names(baked, path)
    .then(|| std::ffi::CString::new(*path).ok())
    .flatten()
}

/// What the outside helper may do for this app: open links (with a run
/// grant for the opener), and start the updater's swap helper for this
/// install, in its two modes. Everything is fixed here, before the fork.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn outside_config(
  baked: &deno_runtime::deno_permissions::PermissionsOptions,
) -> deno_lib::kernel_apply::outside::Config {
  use std::ffi::CString;
  use std::os::unix::ffi::OsStrExt;

  use deno_lib::kernel_apply::outside;
  use deno_runtime::ops::desktop_update::layout;
  use deno_runtime::ops::desktop_update::swap;

  let mut config = outside::Config {
    opener: opener(baked),
    fixed: Vec::new(),
  };
  let install = std::env::current_exe().ok().and_then(|exe| {
    layout::detect_install(&exe, &layout::AppImageEnv::from_env()).ok()
  });
  if let Some(install) = install {
    let c = |bytes: &[u8]| CString::new(bytes).ok();
    for mode in UPDATE_MODES {
      let (args, env) = swap::helper_command(mode, std::process::id());
      let args: Option<Vec<CString>> =
        args.iter().map(|a| c(a.as_bytes())).collect();
      let overrides: Vec<(&str, Option<&str>)> =
        env.iter().map(|(k, v)| (*k, Some(*v))).collect();
      if let (Some(program), Some(args), Some(cwd)) = (
        c(install.exe().as_os_str().as_bytes()),
        args,
        c(install.parent.as_os_str().as_bytes()),
      ) {
        config.fixed.push(outside::FixedCommand {
          program,
          args,
          cwd: Some(cwd),
          env: outside::environment(&overrides),
        });
      }
    }
  }
  config
}

/// The swap helper's modes, in the order of the outside helper's fixed
/// commands.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const UPDATE_MODES: [deno_runtime::ops::desktop_update::swap::HelperMode; 2] = [
  deno_runtime::ops::desktop_update::swap::HelperMode::Apply,
  deno_runtime::ops::desktop_update::swap::HelperMode::Rollback,
];

/// Decides and applies the app's kernel sandbox, once per process. `worker`:
/// a fork of the app with no engine. `Err` is a refusal to start (its
/// message).
pub fn startup(data: &StandaloneData, worker: bool) -> Result<(), String> {
  if STARTED.swap(true, Ordering::SeqCst) {
    return Ok(());
  }
  load_inherited();
  let metadata = &data.metadata;
  let host = HOST_ENGINE.get().copied().flatten();
  let engine = denort::kernel_desktop::engine(data, host);
  let baked = denort::kernel_desktop::baked_options(data);
  let os = TargetOs::current();
  // the harnesses' stage switch: raise-only, so a packaged app reads it
  // like every other runtime. It changes nothing here today: only a strict
  // package (a baked enforce) is confined, and `auto` leaves an app that
  // isn't strict alone at every stage (`ModeReason::DesktopNotStrict`)
  let stage = deno_lib::kernel_sandbox::AutoStage::parse(
    std::env::var(deno_lib::kernel_sandbox::KERNEL_SANDBOX_STAGE_ENV_VAR)
      .ok()
      .as_deref(),
  )
  .ok()
  .flatten();
  let resolution = deno_lib::kernel_sandbox::desktop_resolution(
    metadata.kernel_sandbox,
    metadata.desktop_dev,
    &baked,
    engine,
    stage,
  );
  let applies = matches!(
    resolution.mode,
    KernelSandboxMode::Warn | KernelSandboxMode::Enforce
  );
  let dirs = denort::kernel_desktop::engine_dirs(data);
  // a mode that applies nothing touches nothing: no folder is made, no
  // helper is forked
  let paths = if applies && !worker {
    denort::kernel_desktop::create_app_dirs(&dirs);
    let paths = kernel_desktop::engine_paths(os, engine, &dirs);
    if os == TargetOs::Linux {
      kernel_desktop::engine_dirs_present(paths)
    } else {
      paths
    }
  } else {
    kernel_desktop::EnginePaths::default()
  };
  let grant = kernel_desktop::desktop_grant(&baked, os, &paths, worker);
  let scoped =
    |list: &Option<Vec<String>>| !list.as_ref().is_some_and(|l| l.is_empty());
  // the engine keeps its web data in folders named after the app: without
  // an identifier they can't be granted, and a scoped grant would cut the
  // engine off from them
  let blocked = (applies
    && !worker
    && dirs.app_id.is_none()
    && (scoped(&baked.allow_read) || scoped(&baked.allow_write)))
  .then(|| {
    "the app has no identifier (desktop.app.identifier), so its web engine's own folders can't be named in a scoped grant".to_string()
  });
  #[allow(unused_mut, reason = "the outside helper is Unix-only")]
  let mut extras = DesktopExtras {
    worker,
    baked: Some(&baked),
    exec: paths.exec.clone(),
    install: dirs.install.clone(),
    residuals: grant.residuals.clone(),
    blocked,
    stage,
    ..Default::default()
  };
  #[cfg(any(target_os = "linux", target_os = "macos"))]
  if applies && !worker {
    extras.outside = Some(outside_config(&baked));
  }
  let outcome = start_desktop_with(
    &CompiledStart {
      baked: metadata.kernel_sandbox,
      exec_strict: metadata.kernel_sandbox_exec_strict,
      dev_build: metadata.desktop_dev,
      options: &grant.options,
      cache_base: None,
    },
    metadata.kernel_sandbox == Some(KernelSandboxMode::Enforce),
    engine,
    extras,
  )
  .map_err(|err| err.to_string())?;
  #[cfg(any(target_os = "linux", target_os = "macos"))]
  if outcome.enforcing
    && let Some(outside) = deno_lib::kernel_apply::desktop_outside()
  {
    // the updater's swap helper is started outside the sandbox
    deno_runtime::ops::desktop_update::swap::set_outside_spawner(Box::new(
      move |mode| {
        let index = UPDATE_MODES
          .iter()
          .position(|m| *m == mode)
          .unwrap_or(usize::MAX);
        outside.run_fixed(index)
      },
    ));
  }
  let _ = outcome;
  Ok(())
}

/// The app's kernel-sandbox state for the capability probe.
pub fn report() -> Option<(&'static str, String)> {
  deno_lib::kernel_apply::desktop_report()
    .map(|report| (report.sandbox.as_str(), report.reason.clone()))
}

/// Opens `url` (checked by the op) with the system's opener: through the
/// outside helper in a confined app, else directly.
pub fn open_external(url: &str) -> bool {
  #[cfg(any(target_os = "linux", target_os = "macos"))]
  if let Some(outside) = deno_lib::kernel_apply::desktop_outside() {
    return match outside.open_url(url) {
      Ok(()) => true,
      Err(err) => {
        log::debug!("[desktop] openExternal through the outside helper: {err}");
        false
      }
    };
  }
  let mut command = if cfg!(target_os = "macos") {
    std::process::Command::new("/usr/bin/open")
  } else if cfg!(windows) {
    let mut command = std::process::Command::new("rundll32.exe");
    command.arg("url.dll,FileProtocolHandler");
    command
  } else {
    std::process::Command::new("xdg-open")
  };
  command
    .arg(url)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null());
  match command.spawn() {
    Ok(mut child) => {
      // reaped off the JavaScript thread; the opener exits at once
      std::thread::spawn(move || {
        let _ = child.wait();
      });
      true
    }
    Err(err) => {
      log::debug!("[desktop] openExternal: {err}");
      false
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_host_names_its_engine() {
    assert_eq!(host_engine(1), Some(Engine::Webview));
    assert_eq!(host_engine(2), Some(Engine::Cef));
    assert_eq!(host_engine(3), Some(Engine::Gpui));
    for unknown in [0, 4, -1] {
      assert_eq!(host_engine(unknown), None);
    }
  }
}
