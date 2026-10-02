// Copyright 2018-2026 the Deno authors. MIT license.

//! Full-app self-update for desktop apps (`Deno.desktop.updater`).
//!
//! The whole signed app is replaced, never patched in place: patching a file
//! inside a signed macOS bundle breaks its code signature (and
//! notarization), so a real update swaps the entire bundle. The flow:
//!
//! 1. `check(manifestUrl)` fetches the signed manifest and verifies it
//!    ([`manifest`]): ECDSA P-256 against the public key BAKED INTO THE APP
//!    at package time (there is no unsigned path), this app's identifier, a
//!    version strictly newer than the running one (no downgrade, no
//!    reinstall), not a version that was rolled back, this platform's entry,
//!    an https URL.
//! 2. `download()` streams the archive into a staging directory next to the
//!    install, refusing a byte past the manifest's size, then matches size
//!    and SHA-256 ([`archive::DownloadSink`]).
//! 3. `stage()` extracts it with the safe extractor ([`archive`]), checks it
//!    is the same app shape, and runs the OS code-signature check against the
//!    running app ([`oscheck`]). Only then is the update `staged`.
//! 4. `applyAndRelaunch()` starts the helper, which waits for the app to
//!    exit, swaps the install atomically and relaunches ([`swap`]).
//! 5. The new version calls `confirm()`; an update that is not confirmed by
//!    its next launch is rolled back.
//!
//! Nothing is written to the install location before step 4, and the helper
//! only ever moves what step 3 verified.

#![allow(
  clippy::disallowed_methods,
  reason = "the updater stages and swaps the app install, outside any user \
            permission sandbox, by design"
)]

pub mod archive;
pub mod error;
pub mod layout;
pub mod manifest;
pub mod oscheck;
pub mod swap;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use deno_core::OpState;
use deno_core::op2;
use deno_error::JsErrorBox;
use serde::Serialize;

use self::archive::DownloadSink;
use self::error::UpdateError;
use self::error::UpdateErrorCode as Code;
use self::layout::AppImageEnv;
use self::layout::InstallKind;
use self::layout::InstallLayout;
use self::manifest::ManifestVerdict;
use self::manifest::VerifiedUpdate;
use self::oscheck::SignatureReport;
use self::swap::Phase;
use self::swap::UpdateState;

/// The `Deno.desktop.updater` JavaScript, run after the desktop API is up.
pub const APP_UPDATER_JS: &str = include_str!("updater.js");

/// What the runtime knows about this app, put into `OpState` at startup.
#[derive(Debug, Clone, Default)]
pub struct AppUpdateConfig {
  /// `desktop.app.identifier`.
  pub app_id: Option<String>,
  /// deno.json `version`.
  pub version: Option<String>,
  /// The baked update public key (base64 SPKI or PEM, ECDSA P-256).
  pub public_key: Option<String>,
  /// The running executable (`None`: `std::env::current_exe()`).
  pub current_exe: Option<PathBuf>,
  /// This launch's arguments, for the relaunch (markers removed).
  pub launch_args: Vec<String>,
  /// `--denext-updated-from=<v>`: launched by the helper after an update.
  pub updated_from: Option<String>,
  /// `--denext-update-rolled-back=<v>`: launched after a rollback.
  pub rolled_back_from: Option<String>,
  /// This is the new version's trial launch (unconfirmed).
  pub trial: bool,
}

/// One update at a time, per process.
#[derive(Default)]
struct Session {
  /// What the last `check()` offered: what the next `download()` fetches.
  verified: Option<VerifiedUpdate>,
  /// The update `download()` began with, which the download and the stage
  /// stay bound to. A `check()` while they run (a periodic check, another
  /// window) replaces `verified`, and used to make the stage label the
  /// archive downloaded for one version with another version.
  in_flight: Option<VerifiedUpdate>,
  sink: Option<DownloadSink>,
  archive: Option<PathBuf>,
  staged: Option<String>,
  staging: bool,
}

#[derive(Default)]
struct SessionCell(RefCell<Session>);

/// Everything an update step needs, or the reason it cannot run.
struct Ready {
  layout: InstallLayout,
  platform: String,
  app_id: String,
  version: String,
  public_key: String,
}

fn current_exe(config: &AppUpdateConfig) -> Result<PathBuf, UpdateError> {
  match &config.current_exe {
    Some(p) => Ok(p.clone()),
    None => std::env::current_exe()
      .map_err(|e| UpdateError::io("the running executable", e)),
  }
}

fn layout_of(config: &AppUpdateConfig) -> Result<InstallLayout, UpdateError> {
  layout::detect_install(&current_exe(config)?, &AppImageEnv::from_env())
}

fn ready(config: &AppUpdateConfig) -> Result<Ready, UpdateError> {
  let Some(app_id) = config.app_id.clone() else {
    return error::err(
      Code::NotConfigured,
      "the app has no identifier (desktop.app.identifier)",
    );
  };
  let Some(version) = config.version.clone() else {
    return error::err(
      Code::NotConfigured,
      "the app has no version (deno.json \"version\")",
    );
  };
  let Some(public_key) = config.public_key.clone() else {
    return error::err(
      Code::NotConfigured,
      "no update public key is baked into the app (desktop.update.publicKey \
       in deno.json, or \"update\": { \"publicKey\" } in \
       .deno-desktop/app.json); full-app updates are always signed",
    );
  };
  let exe = current_exe(config)?;
  let layout = layout::detect_install(&exe, &AppImageEnv::from_env())?;
  let backend = layout::detect_backend(&exe);
  let Some(mut platform) = layout::platform_key(backend) else {
    return error::err(Code::NotConfigured, "unsupported platform");
  };
  if layout.kind == InstallKind::AppImage {
    platform.push_str("-appimage");
  }
  Ok(Ready {
    layout,
    platform,
    app_id,
    version,
    public_key,
  })
}

fn js(e: UpdateError) -> JsErrorBox {
  JsErrorBox::generic(e.to_string())
}

/// The update configuration of the packaged desktop app this runtime runs.
///
/// Only the desktop runtime (`cli/rt_desktop`) puts an [`AppUpdateConfig`]
/// into the `OpState`. Everywhere else (a plain `deno run`, `deno serve`, a
/// worker) the `Deno.desktop.updater` ops are still reachable through
/// `Deno[Deno.internal].core.ops`, so they must not touch the running
/// executable's install (its path, its update state file, its staging
/// directory) there: that would read and delete files with no permission.
fn host(state: &OpState) -> Result<AppUpdateConfig, UpdateError> {
  state
    .try_borrow::<AppUpdateConfig>()
    .cloned()
    .ok_or_else(|| {
      UpdateError::new(
        Code::NotConfigured,
        "full-app updates are only available in a packaged desktop app",
      )
    })
}

/// [`host`] plus everything an update step needs ([`ready`]): an identifier,
/// a version and a baked update public key. Every step that reads or writes
/// the install goes through this gate.
fn gate(state: &OpState) -> Result<(AppUpdateConfig, Ready), UpdateError> {
  let config = host(state)?;
  let ready = ready(&config)?;
  Ok((config, ready))
}

fn session(state: &mut OpState) -> &SessionCell {
  if state.try_borrow::<SessionCell>().is_none() {
    state.put(SessionCell::default());
  }
  state.borrow::<SessionCell>()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InfoOut {
  configured: bool,
  reason: Option<String>,
  version: Option<String>,
  app_id: Option<String>,
  platform: Option<String>,
  install: Option<String>,
  kind: Option<InstallKind>,
  phase: Option<Phase>,
  pending_version: Option<String>,
  staged_version: Option<String>,
  rejected: Option<String>,
  last_error: Option<String>,
  updated_from: Option<String>,
  rolled_back_from: Option<String>,
  trial: bool,
}

#[op2]
#[serde]
pub fn op_desktop_app_update_info(state: &mut OpState) -> InfoOut {
  update_info(state)
}

fn update_info(state: &mut OpState) -> InfoOut {
  let config = host(state).unwrap_or_default();
  let staged_version = session(state).0.borrow().staged.clone();
  let ready = gate(state).map(|(_, ready)| ready);
  // The install (its path, kind and update state) is reported only to an
  // app the updater can act for: outside a packaged, configured app this
  // would hand out the running executable's location and read files next to
  // it without a read permission.
  let layout = ready.as_ref().ok().map(|r| r.layout.clone());
  let st = layout.as_ref().and_then(swap::read_state);
  InfoOut {
    configured: ready.is_ok(),
    reason: ready.as_ref().err().map(|e| e.to_string()),
    version: config.version.clone(),
    app_id: config.app_id.clone(),
    platform: ready.as_ref().ok().map(|r| r.platform.clone()),
    install: layout
      .as_ref()
      .map(|l| l.install.to_string_lossy().into_owned()),
    kind: layout.as_ref().map(|l| l.kind),
    phase: st.as_ref().map(|s| s.phase),
    pending_version: st
      .as_ref()
      .filter(|s| s.phase == Phase::Swapped)
      .and_then(|s| s.to.clone()),
    staged_version,
    rejected: st.as_ref().and_then(|s| s.rejected.clone()),
    last_error: st.as_ref().and_then(|s| s.last_error.clone()),
    updated_from: config.updated_from,
    rolled_back_from: config.rolled_back_from,
    trial: config.trial,
  }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckOut {
  available: bool,
  version: String,
  current_version: String,
  update: Option<VerifiedUpdate>,
}

/// Verify a fetched manifest (the raw bytes) and remember the update it
/// offers for `download()`.
#[op2]
#[serde]
pub fn op_desktop_app_update_check(
  state: &mut OpState,
  #[buffer] manifest_bytes: &[u8],
  allow_insecure_loopback: bool,
) -> Result<CheckOut, JsErrorBox> {
  update_check(state, manifest_bytes, allow_insecure_loopback)
}

fn update_check(
  state: &mut OpState,
  manifest_bytes: &[u8],
  allow_insecure_loopback: bool,
) -> Result<CheckOut, JsErrorBox> {
  let (_, ready) = gate(state).map_err(js)?;
  let rejected = swap::read_state(&ready.layout).and_then(|s| s.rejected);
  let verdict = manifest::verify_manifest(
    manifest_bytes,
    &manifest::Expectations {
      public_key: &ready.public_key,
      app_id: &ready.app_id,
      running_version: &ready.version,
      platform: &ready.platform,
      rejected_version: rejected.as_deref(),
      allow_insecure_loopback,
    },
  )
  .map_err(js)?;
  let mut session = session(state).0.borrow_mut();
  match verdict {
    ManifestVerdict::Available(update) => {
      session.verified = Some(update.clone());
      Ok(CheckOut {
        available: true,
        version: update.version.clone(),
        current_version: ready.version,
        update: Some(update),
      })
    }
    ManifestVerdict::UpToDate { version } => {
      session.verified = None;
      Ok(CheckOut {
        available: false,
        version,
        current_version: ready.version,
        update: None,
      })
    }
  }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginOut {
  url: String,
  size: f64,
  version: String,
}

/// Prepare the staging directory and open the download sink for the
/// verified update.
#[op2]
#[serde]
pub fn op_desktop_app_update_begin(
  state: &mut OpState,
) -> Result<BeginOut, JsErrorBox> {
  update_begin(state)
}

fn update_begin(state: &mut OpState) -> Result<BeginOut, JsErrorBox> {
  let (_, ready) = gate(state).map_err(js)?;
  let mut session = session(state).0.borrow_mut();
  if session.sink.is_some() || session.staging {
    return Err(js(UpdateError::new(
      Code::Busy,
      "an update is already downloading or staging",
    )));
  }
  let Some(update) = session.verified.clone() else {
    return Err(js(UpdateError::new(
      Code::NotStaged,
      "no verified update: call check() first",
    )));
  };
  let layout = &ready.layout;
  layout::check_writable(layout).map_err(js)?;
  let mut st =
    swap::read_state(layout).unwrap_or_else(|| UpdateState::new(layout));
  match st.phase {
    Phase::Swapped | Phase::Swapping | Phase::RollingBack => {
      return Err(js(UpdateError::new(
        Code::Busy,
        "an installed update is awaiting confirmation; confirm() it first",
      )));
    }
    Phase::Staged => {
      // A stage from an earlier run is never applied blind: start over.
      st.phase = Phase::Idle;
      st.entry = None;
      swap::write_state(layout, &st).map_err(js)?;
    }
    Phase::Idle => {}
  }
  let staging = layout.staging_dir();
  if !swap::discard_path(layout, &staging) {
    return Err(js(UpdateError::io(staging.display(), "cannot clear")));
  }
  std::fs::create_dir(&staging).map_err(|e| {
    js(UpdateError::new(
      Code::InstallNotWritable,
      format!("{}: {e}", staging.display()),
    ))
  })?;
  let sink = DownloadSink::create(
    staging.join("download.part"),
    update.size,
    &update.sha256,
  )
  .map_err(js)?;
  session.sink = Some(sink);
  session.in_flight = Some(update.clone());
  session.archive = None;
  session.staged = None;
  Ok(BeginOut {
    url: update.url,
    size: update.size as f64,
    version: update.version,
  })
}

fn abort_download(state: &mut OpState) {
  let gated = gate(state);
  let mut session = session(state).0.borrow_mut();
  session.sink = None;
  session.in_flight = None;
  session.archive = None;
  session.staged = None;
  // Only a configured app has a staging directory of its own: never delete
  // anything next to a plain `deno` executable.
  if let Ok((_, ready)) = gated {
    swap::discard_path(&ready.layout, &ready.layout.staging_dir());
  }
}

/// Append a chunk; past the declared size the download is aborted.
#[op2(fast)]
pub fn op_desktop_app_update_write(
  state: &mut OpState,
  #[buffer] chunk: &[u8],
) -> Result<f64, JsErrorBox> {
  let result = {
    let mut session = session(state).0.borrow_mut();
    match session.sink.as_mut() {
      Some(sink) => sink.write(chunk).map(|_| sink.written() as f64),
      None => Err(UpdateError::new(Code::NotStaged, "no download in progress")),
    }
  };
  result.map_err(|e| {
    if e.code != Code::NotStaged {
      abort_download(state);
    }
    js(e)
  })
}

/// Close the download and match size + SHA-256. The file is synced to disk
/// and hashed on the blocking pool, not the JavaScript thread.
#[op2]
pub async fn op_desktop_app_update_finish(
  state: Rc<RefCell<OpState>>,
) -> Result<(), JsErrorBox> {
  let sink = session(&mut state.borrow_mut()).0.borrow_mut().sink.take();
  let Some(sink) = sink else {
    return Err(js(UpdateError::new(
      Code::NotStaged,
      "no download in progress",
    )));
  };
  let finished = deno_core::unsync::spawn_blocking(move || sink.finish())
    .await
    .map_err(|e| UpdateError::io("finish", e))
    .and_then(|r| r);
  let mut s = state.borrow_mut();
  match finished {
    Ok(path) => {
      session(&mut s).0.borrow_mut().archive = Some(path);
      Ok(())
    }
    Err(e) => {
      abort_download(&mut s);
      Err(js(e))
    }
  }
}

/// Drop a download (or a stage) and its staging directory.
#[op2(fast)]
pub fn op_desktop_app_update_abort(state: &mut OpState) {
  abort_download(state);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageOut {
  version: String,
  signature: SignatureReport,
}

/// Extract the verified archive and check the staged app (shape + OS code
/// signature). Only a staged app can be applied.
#[op2]
#[serde]
pub async fn op_desktop_app_update_stage(
  state: Rc<RefCell<OpState>>,
  allow_unsigned_dev: bool,
) -> Result<StageOut, JsErrorBox> {
  let (ready, update, archive) = {
    let mut s = state.borrow_mut();
    let (_, ready) = gate(&s).map_err(js)?;
    let cell = session(&mut s);
    let mut session = cell.0.borrow_mut();
    if session.staging {
      return Err(js(UpdateError::new(Code::Busy, "already staging")));
    }
    let (Some(update), Some(archive)) =
      (session.in_flight.clone(), session.archive.clone())
    else {
      return Err(js(UpdateError::new(
        Code::NotStaged,
        "nothing downloaded: call check() and download() first",
      )));
    };
    session.staging = true;
    (ready, update, archive)
  };
  let layout = ready.layout.clone();
  let from = ready.version.clone();
  let to = update.version.clone();
  let size = update.size;
  let result = deno_core::unsync::spawn_blocking(move || {
    stage_blocking(&layout, &archive, size, &from, &to, allow_unsigned_dev)
  })
  .await
  .map_err(|e| UpdateError::io("stage", e))
  .and_then(|r| r);
  let mut s = state.borrow_mut();
  let cell = session(&mut s);
  let mut session = cell.0.borrow_mut();
  session.staging = false;
  session.archive = None;
  match result {
    Ok(signature) => {
      session.staged = Some(update.version.clone());
      Ok(StageOut {
        version: update.version,
        signature,
      })
    }
    Err(e) => {
      session.staged = None;
      drop(session);
      swap::discard_path(&ready.layout, &ready.layout.staging_dir());
      Err(js(e))
    }
  }
}

fn stage_blocking(
  layout: &InstallLayout,
  archive: &std::path::Path,
  size: u64,
  from: &str,
  to: &str,
  allow_unsigned_dev: bool,
) -> Result<SignatureReport, UpdateError> {
  let extract = layout.extract_dir();
  swap::remove_path(&extract);
  let max = size
    .saturating_mul(32)
    .saturating_add(64 * 1024 * 1024)
    .min(archive::MAX_EXTRACTED_BYTES);
  let x = archive::extract_tar_gz(archive, &extract, max)?;
  let staged = extract.join(&x.top);
  let exe = layout.exe_in(&staged);
  let shape_ok = match layout.kind {
    InstallKind::MacBundle => {
      x.top_is_dir && x.top.ends_with(".app") && exe.is_file()
    }
    InstallKind::AppDir => x.top_is_dir && exe.is_file(),
    InstallKind::AppImage => !x.top_is_dir,
  };
  if !shape_ok {
    return error::err(
      Code::BundleMismatch,
      format!(
        "the archive does not hold this app (expected {} with {})",
        match layout.kind {
          InstallKind::MacBundle => "a .app bundle",
          InstallKind::AppDir => "an app directory",
          InstallKind::AppImage => "a single AppImage file",
        },
        layout.exe_rel.display()
      ),
    );
  }
  #[cfg(unix)]
  if layout.kind == InstallKind::AppImage {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
      .map_err(|e| UpdateError::io(staged.display(), e))?;
  } else if !is_executable(&exe) {
    // The archive's modes are kept as packed. An executable packed without
    // its execute bit would be swapped in and then fail to start at all, so
    // not even the trial's rollback could run: refuse it here.
    return error::err(
      Code::BundleMismatch,
      format!(
        "{} in the archive is not executable (packed without its mode bits?)",
        layout.exe_rel.display()
      ),
    );
  }
  let signature = verify_os_signature(layout, &staged, allow_unsigned_dev)?;
  let _ = std::fs::remove_file(archive);
  let previous = swap::read_state(layout);
  let mut st = UpdateState::new(layout);
  st.phase = Phase::Staged;
  st.from = Some(from.to_string());
  st.to = Some(to.to_string());
  st.entry = Some(x.top);
  st.rejected = previous.and_then(|p| p.rejected);
  swap::write_state(layout, &st)?;
  Ok(signature)
}

/// Whether the owner may execute `path` (Unix).
#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
  use std::os::unix::fs::PermissionsExt;
  std::fs::metadata(path)
    .map(|m| m.is_file() && m.permissions().mode() & 0o100 != 0)
    .unwrap_or(false)
}

fn verify_os_signature(
  layout: &InstallLayout,
  staged: &std::path::Path,
  allow_unsigned_dev: bool,
) -> Result<SignatureReport, UpdateError> {
  if cfg!(target_os = "macos") {
    oscheck::verify_macos(
      &oscheck::SystemRunner,
      &layout.install,
      staged,
      allow_unsigned_dev,
    )
  } else if cfg!(windows) {
    let exe = layout.exe_in(staged);
    let mut files = vec![exe.clone()];
    // The runtime the executable loads: `<App>.dll` next to `<App>.exe`.
    let dll = exe.with_extension("dll");
    if dll.is_file() {
      files.push(dll);
    }
    oscheck::verify_windows(
      &oscheck::authenticode_signer,
      &layout.exe(),
      &files,
      allow_unsigned_dev,
    )
  } else {
    Ok(SignatureReport {
      mode: "none",
      identity: None,
    })
  }
}

/// Record the relaunch arguments and start the helper; the caller then
/// quits the app (the helper waits for it to exit).
#[op2(fast)]
pub fn op_desktop_app_update_apply(
  state: &mut OpState,
) -> Result<(), JsErrorBox> {
  update_apply(state)
}

fn update_apply(state: &mut OpState) -> Result<(), JsErrorBox> {
  let (config, ready) = gate(state).map_err(js)?;
  let staged = session(state).0.borrow().staged.clone();
  let Some(version) = staged else {
    return Err(js(UpdateError::new(
      Code::NotStaged,
      "nothing staged in this run: call check(), download() and stage()",
    )));
  };
  let layout = ready.layout;
  let Some(mut st) = swap::read_state(&layout)
    .filter(|s| s.phase == Phase::Staged && s.to.as_deref() == Some(&version))
  else {
    return Err(js(UpdateError::new(
      Code::NotStaged,
      "the staged update is gone",
    )));
  };
  st.relaunch_args = config
    .launch_args
    .iter()
    .filter(|a| !swap::is_update_marker(a))
    .cloned()
    .collect();
  st.last_error = None;
  swap::write_state(&layout, &st).map_err(js)?;
  swap::spawn_helper(&layout, swap::HelperMode::Apply)
    .map_err(|e| js(UpdateError::io("starting the update helper", e)))?;
  Ok(())
}

/// Confirm the running version after an update (deletes the previous app).
#[op2(fast)]
pub fn op_desktop_app_update_confirm(
  state: &mut OpState,
) -> Result<bool, JsErrorBox> {
  update_confirm(state)
}

fn update_confirm(state: &mut OpState) -> Result<bool, JsErrorBox> {
  // Confirming needs the app (its identity), not the update key: an updated
  // version that no longer bakes a key must still be able to confirm itself,
  // or the next launch would roll it back. Outside a packaged app, nothing
  // was ever swapped by this runtime and nothing next to the executable is
  // touched.
  let Ok(config) = host(state) else {
    return Ok(false);
  };
  if config.app_id.is_none() {
    return Ok(false);
  }
  let layout = match layout_of(&config) {
    Ok(l) => l,
    // Not a replaceable install: nothing was ever swapped here.
    Err(_) => return Ok(false),
  };
  swap::confirm(&layout).map_err(js)
}

/// Run at the very start of a desktop app's process (before the runtime or
/// any window): handle the helper's argv, then the startup watchdog. Returns
/// `Some(exit_code)` when the process must exit now; `None` to start
/// normally. `trial` is set on the first launch of a new version.
pub fn early_startup(args: &[String], trial: &mut bool) -> Option<i32> {
  let exe = std::env::current_exe().ok()?;
  if let Some((mode, pid)) = swap::parse_helper_args(args) {
    return Some(
      match layout::detect_install(&exe, &AppImageEnv::from_env()) {
        Ok(layout) => swap::run_helper(&layout, mode, pid),
        Err(_) => 1,
      },
    );
  }
  // Not an installed app (a dev run, a translocated copy, ...): nothing to
  // watch.
  let layout = layout::detect_install(&exe, &AppImageEnv::from_env()).ok()?;
  match swap::startup_action(&layout) {
    swap::StartupAction::Continue { trial: t } => {
      *trial = t;
      None
    }
    swap::StartupAction::RollBack => {
      swap::log_line(
        &layout,
        "unconfirmed or interrupted update: rolling back",
      );
      match swap::spawn_helper(&layout, swap::HelperMode::Rollback) {
        Ok(()) => Some(0),
        Err(e) => {
          swap::log_line(&layout, &format!("could not start the helper: {e}"));
          None
        }
      }
    }
  }
}

/// Split this launch's arguments into the relaunch arguments and the
/// updater's markers: `(args, updated_from, rolled_back_from)`.
pub fn split_launch_markers(
  args: &[String],
) -> (Vec<String>, Option<String>, Option<String>) {
  let mut out = Vec::new();
  let mut from = None;
  let mut rolled = None;
  for a in args {
    if let Some(v) = a.strip_prefix(swap::UPDATED_FROM_ARG) {
      from = Some(v.to_string());
    } else if let Some(v) = a.strip_prefix(swap::ROLLED_BACK_ARG) {
      rolled = Some(v.to_string());
    } else {
      out.push(a.clone());
    }
  }
  (out, from, rolled)
}

deno_core::extension!(
  deno_desktop_update,
  ops = [
    op_desktop_app_update_info,
    op_desktop_app_update_check,
    op_desktop_app_update_begin,
    op_desktop_app_update_write,
    op_desktop_app_update_finish,
    op_desktop_app_update_abort,
    op_desktop_app_update_stage,
    op_desktop_app_update_apply,
    op_desktop_app_update_confirm,
  ],
);

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn splits_markers() {
    let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let (args, from, rolled) = split_launch_markers(&a(&[
      "myapp://x",
      "--denext-updated-from=1.0.0",
      "file.txt",
    ]));
    assert_eq!(args, a(&["myapp://x", "file.txt"]));
    assert_eq!(from.as_deref(), Some("1.0.0"));
    assert_eq!(rolled, None);
    let (_, _, rolled) =
      split_launch_markers(&a(&["--denext-update-rolled-back=2.0.0"]));
    assert_eq!(rolled.as_deref(), Some("2.0.0"));
  }

  #[test]
  fn not_configured_without_key_id_or_version() {
    let base = AppUpdateConfig {
      app_id: Some("com.example.app".into()),
      version: Some("1.0.0".into()),
      public_key: Some("x".into()),
      ..Default::default()
    };
    for broken in [
      AppUpdateConfig {
        app_id: None,
        ..base.clone()
      },
      AppUpdateConfig {
        version: None,
        ..base.clone()
      },
      AppUpdateConfig {
        public_key: None,
        ..base.clone()
      },
    ] {
      assert_eq!(ready(&broken).err().unwrap().code, Code::NotConfigured);
    }
  }

  /// A fake packaged app under a temp dir: the executable's path, and the
  /// install layout `detect_install` derives from it.
  fn fake_app(tmp: &tempfile::TempDir) -> (PathBuf, InstallLayout) {
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let exe = if cfg!(target_os = "macos") {
      root.join("Fake.app/Contents/MacOS/fake")
    } else {
      root
        .join("Fake")
        .join(if cfg!(windows) { "fake.exe" } else { "fake" })
    };
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, b"").unwrap();
    let layout = layout::detect_install(&exe, &AppImageEnv::default()).unwrap();
    (exe, layout)
  }

  #[cfg(unix)]
  #[test]
  fn stage_refuses_an_executable_without_its_mode_bits() {
    let tmp = tempfile::tempdir().unwrap();
    let (_exe, layout) = fake_app(&tmp);
    let top = layout.install.file_name().unwrap().to_owned();
    let pack = |mode: u32| {
      let archive = tmp.path().join(format!("app-{mode:o}.tar.gz"));
      let gz = flate2::write::GzEncoder::new(
        std::fs::File::create(&archive).unwrap(),
        flate2::Compression::fast(),
      );
      let mut tar = tar::Builder::new(gz);
      let mut dirs = PathBuf::new();
      let exe_rel = std::path::Path::new(&top).join(&layout.exe_rel);
      for c in exe_rel.parent().unwrap().components() {
        dirs.push(c);
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Directory);
        h.set_mode(0o755);
        h.set_size(0);
        h.set_cksum();
        tar.append_data(&mut h, &dirs, std::io::empty()).unwrap();
      }
      let mut h = tar::Header::new_gnu();
      h.set_mode(mode);
      h.set_size(4);
      h.set_cksum();
      tar.append_data(&mut h, &exe_rel, &b"#!/x"[..]).unwrap();
      tar.into_inner().unwrap().finish().unwrap();
      let size = std::fs::metadata(&archive).unwrap().len();
      (archive, size)
    };
    let (archive, size) = pack(0o644);
    let e = stage_blocking(&layout, &archive, size, "1.0.0", "2.0.0", true)
      .unwrap_err();
    assert_eq!(e.code, Code::BundleMismatch, "{}", e.message);
    assert!(e.message.contains("not executable"), "{}", e.message);
    // The same app with its execute bit passes that check (whatever the
    // OS signature check then says about a fake app).
    let (archive, size) = pack(0o755);
    if let Err(e) =
      stage_blocking(&layout, &archive, size, "1.0.0", "2.0.0", true)
    {
      assert!(!e.message.contains("not executable"), "{}", e.message);
    }
  }

  fn host_config(exe: PathBuf, public_key: Option<&str>) -> AppUpdateConfig {
    AppUpdateConfig {
      app_id: Some("com.example.fake".into()),
      version: Some("1.0.0".into()),
      public_key: public_key.map(str::to_string),
      current_exe: Some(exe),
      ..Default::default()
    }
  }

  fn code_of(e: JsErrorBox) -> String {
    e.to_string()
  }

  // Outside a packaged desktop app (a plain `deno run`, where the ops are
  // still reachable through `Deno[Deno.internal].core.ops`) every step
  // refuses before it looks at the running executable's install.
  #[test]
  fn ops_refuse_without_a_desktop_host() {
    let mut state = OpState::new(None);
    let info = update_info(&mut state);
    assert!(!info.configured);
    assert!(
      info
        .reason
        .as_deref()
        .unwrap()
        .contains("packaged desktop app")
    );
    assert_eq!(info.install, None);
    assert_eq!(info.kind, None);
    assert_eq!(info.phase, None);
    assert_eq!(info.app_id, None);

    let refused = |r: Result<(), JsErrorBox>| {
      let msg = code_of(r.unwrap_err());
      assert!(msg.contains("not_configured"), "{msg}");
    };
    refused(update_check(&mut state, b"{}", false).map(|_| ()));
    refused(update_begin(&mut state).map(|_| ()));
    refused(update_apply(&mut state));
    assert!(!update_confirm(&mut state).unwrap());
    abort_download(&mut state);
  }

  // A packaged app without an update key: the updater is off, and neither
  // the install's path nor its update state is reported or touched.
  #[test]
  fn unconfigured_app_keeps_its_install_private() {
    let tmp = tempfile::tempdir().unwrap();
    let (exe, layout) = fake_app(&tmp);
    let staging = layout.staging_dir();
    std::fs::create_dir_all(&staging).unwrap();
    let mut st = UpdateState::new(&layout);
    st.phase = Phase::Swapped;
    st.to = Some("2.0.0".into());
    swap::write_state(&layout, &st).unwrap();

    let mut state = OpState::new(None);
    state.put(host_config(exe, None));
    let info = update_info(&mut state);
    assert!(!info.configured);
    assert!(info.reason.as_deref().unwrap().contains("public key"));
    assert_eq!(info.app_id.as_deref(), Some("com.example.fake"));
    assert_eq!(info.install, None);
    assert_eq!(info.phase, None);
    assert_eq!(info.pending_version, None);

    abort_download(&mut state);
    assert!(
      staging.exists(),
      "abort deleted an unconfigured app's files"
    );
    let msg = code_of(update_begin(&mut state).err().unwrap());
    assert!(msg.contains("not_configured"), "{msg}");
  }

  // Confirming needs only the app's identity: an updated version that no
  // longer bakes a key still confirms itself instead of rolling back.
  #[test]
  fn confirm_needs_the_app_not_the_key() {
    let tmp = tempfile::tempdir().unwrap();
    let (exe, layout) = fake_app(&tmp);
    let mut st = UpdateState::new(&layout);
    st.phase = Phase::Swapped;
    st.to = Some("2.0.0".into());
    swap::write_state(&layout, &st).unwrap();

    let mut state = OpState::new(None);
    state.put(AppUpdateConfig {
      app_id: None,
      ..host_config(exe.clone(), None)
    });
    assert!(!update_confirm(&mut state).unwrap());
    assert_eq!(swap::read_state(&layout).unwrap().phase, Phase::Swapped);

    let mut state = OpState::new(None);
    state.put(host_config(exe, None));
    assert!(update_confirm(&mut state).unwrap());
    assert_eq!(swap::read_state(&layout).unwrap().phase, Phase::Idle);
  }

  // A check() while a download runs offers another version; the download
  // (and its stage) stay bound to the version they began with.
  #[test]
  fn a_check_during_a_download_does_not_relabel_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (exe, _layout) = fake_app(&tmp);
    let mut state = OpState::new(None);
    state.put(host_config(exe, Some("key")));
    let update = |version: &str| VerifiedUpdate {
      version: version.into(),
      min_version: None,
      required: false,
      platform: "x".into(),
      url: format!("https://u.example/{version}.tar.gz"),
      sha256: "0".repeat(64),
      size: 10,
      release_notes: None,
      published_at: "2026-01-01T00:00:00Z".into(),
    };
    session(&mut state).0.borrow_mut().verified = Some(update("2.0.0"));
    let begun = update_begin(&mut state).unwrap();
    assert_eq!(begun.version, "2.0.0");
    // A periodic check() finds 3.0.0 meanwhile.
    session(&mut state).0.borrow_mut().verified = Some(update("3.0.0"));
    let cell = session(&mut state);
    let s = cell.0.borrow();
    assert_eq!(s.in_flight.as_ref().unwrap().version, "2.0.0");
    drop(s);
    // Dropping the download unbinds it.
    abort_download(&mut state);
    assert!(session(&mut state).0.borrow().in_flight.is_none());
  }

  // A configured app sees its own install and owns its staging directory.
  #[test]
  fn configured_app_reports_its_install_and_clears_staging() {
    let tmp = tempfile::tempdir().unwrap();
    let (exe, layout) = fake_app(&tmp);
    let staging = layout.staging_dir();
    std::fs::create_dir_all(&staging).unwrap();

    let mut state = OpState::new(None);
    state.put(host_config(exe, Some("key")));
    let info = update_info(&mut state);
    assert!(info.configured, "{:?}", info.reason);
    assert_eq!(
      info.install.as_deref(),
      Some(layout.install.to_string_lossy().as_ref())
    );
    abort_download(&mut state);
    assert!(!staging.exists());
  }
}
