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
  verified: Option<VerifiedUpdate>,
  sink: Option<DownloadSink>,
  archive: Option<PathBuf>,
  staged: Option<String>,
  staging: bool,
  /// An apply helper was started for this process and not withdrawn.
  applying: bool,
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
      st.staged_digest = None;
      st.apply_pid = None;
      swap::write_state(layout, &st).map_err(js)?;
    }
    Phase::Idle => {}
  }
  let staging = layout.staging_dir();
  if !swap::remove_path(&staging) {
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
  session.archive = None;
  session.staged = None;
  // Only a configured app has a staging directory of its own: never delete
  // anything next to a plain `deno` executable.
  if let Ok((_, ready)) = gated {
    swap::remove_path(&ready.layout.staging_dir());
  }
}

/// Append a chunk; past the declared size the download is aborted.
#[op2(fast)]
pub fn op_desktop_app_update_write(
  state: &mut OpState,
  #[buffer] chunk: &[u8],
) -> Result<f64, JsErrorBox> {
  update_write(state, chunk)
}

fn update_write(state: &mut OpState, chunk: &[u8]) -> Result<f64, JsErrorBox> {
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

/// Close the download and match size + SHA-256.
#[op2(fast)]
pub fn op_desktop_app_update_finish(
  state: &mut OpState,
) -> Result<(), JsErrorBox> {
  let sink = session(state).0.borrow_mut().sink.take();
  let Some(sink) = sink else {
    return Err(js(UpdateError::new(
      Code::NotStaged,
      "no download in progress",
    )));
  };
  match sink.finish() {
    Ok(path) => {
      session(state).0.borrow_mut().archive = Some(path);
      Ok(())
    }
    Err(e) => {
      abort_download(state);
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
  let (ready, update, archive) =
    stage_start(&mut state.borrow_mut()).map_err(js)?;
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
      swap::remove_path(&ready.layout.staging_dir());
      Err(js(e))
    }
  }
}

/// The synchronous half of [`op_desktop_app_update_stage`]: one stage at a
/// time, only of a verified and fully downloaded update; marks the session
/// as staging.
fn stage_start(
  s: &mut OpState,
) -> Result<(Ready, VerifiedUpdate, PathBuf), UpdateError> {
  let (_, ready) = gate(s)?;
  let cell = session(s);
  let mut session = cell.0.borrow_mut();
  if session.staging {
    return Err(UpdateError::new(Code::Busy, "already staging"));
  }
  let (Some(update), Some(archive)) =
    (session.verified.clone(), session.archive.clone())
  else {
    return Err(UpdateError::new(
      Code::NotStaged,
      "nothing downloaded: call check() and download() first",
    ));
  };
  session.staging = true;
  Ok((ready, update, archive))
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
      x.top_is_dir
        && x.top.ends_with(".app")
        && exe.is_file()
        && layout::is_install_copy(InstallKind::MacBundle, &staged)
    }
    InstallKind::AppDir => {
      x.top_is_dir
        && exe.is_file()
        && layout::is_install_copy(InstallKind::AppDir, &staged)
    }
    InstallKind::AppImage => !x.top_is_dir,
  };
  if !shape_ok {
    return error::err(
      Code::BundleMismatch,
      format!(
        "the archive does not hold this app (expected {} with {})",
        match layout.kind {
          InstallKind::MacBundle =>
            "a packaged .app bundle (with \
                                     Contents/Resources/.deno-desktop-app)",
          InstallKind::AppDir =>
            "a packaged app directory (with \
                                  .deno-desktop-app)",
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
  }
  let signature = verify_os_signature(layout, &staged, allow_unsigned_dev)?;
  // What the helper re-checks before it swaps (the tree verified above).
  let staged_digest = swap::tree_digest(&staged)
    .map_err(|e| UpdateError::io(staged.display(), e))?;
  let _ = std::fs::remove_file(archive);
  let previous = swap::read_state(layout);
  let mut st = UpdateState::new(layout);
  st.phase = Phase::Staged;
  st.from = Some(from.to_string());
  st.to = Some(to.to_string());
  st.entry = Some(x.top);
  st.staged_digest = Some(staged_digest);
  st.rejected = previous.and_then(|p| p.rejected);
  swap::write_state(layout, &st)?;
  Ok(signature)
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
/// quits the app (the helper waits for it to exit). With `withdraw` (the
/// quit was refused), withdraw this process's request instead: the waiting
/// helper stands down rather than swap whenever the app exits later.
#[op2(fast)]
pub fn op_desktop_app_update_apply(
  state: &mut OpState,
  withdraw: bool,
) -> Result<(), JsErrorBox> {
  if withdraw {
    return update_withdraw(state);
  }
  update_apply(state)
}

fn update_withdraw(state: &mut OpState) -> Result<(), JsErrorBox> {
  let (_, ready) = gate(state).map_err(js)?;
  session(state).0.borrow_mut().applying = false;
  if let Some(mut st) = swap::read_state(&ready.layout)
    && st.apply_pid == Some(std::process::id())
  {
    st.apply_pid = None;
    swap::write_state(&ready.layout, &st).map_err(js)?;
  }
  Ok(())
}

fn update_apply(state: &mut OpState) -> Result<(), JsErrorBox> {
  let (config, ready) = gate(state).map_err(js)?;
  if session(state).0.borrow().applying {
    return Err(js(UpdateError::new(
      Code::Busy,
      "the update is already being applied: the app is quitting",
    )));
  }
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
  st.apply_pid = Some(std::process::id());
  swap::write_state(&layout, &st).map_err(js)?;
  swap::spawn_helper(&layout, swap::HelperMode::Apply)
    .map_err(|e| js(UpdateError::io("starting the update helper", e)))?;
  session(state).0.borrow_mut().applying = true;
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
  // Only before a `--`: after it every argument is positional (a deep link
  // delivered as `"<exe>" -- "%1"` can't pose as a marker).
  let mut options_ended = false;
  for a in args {
    if options_ended {
      out.push(a.clone());
    } else if a == "--" {
      options_ended = true;
      out.push(a.clone());
    } else if let Some(v) = a.strip_prefix(swap::UPDATED_FROM_ARG) {
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
  fn markers_after_the_terminator_are_positional() {
    let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let (args, from, rolled) = split_launch_markers(&a(&[
      "--denext-updated-from=1.0.0",
      "--",
      "myapp://x",
      "--denext-update-rolled-back=9.9.9",
    ]));
    assert_eq!(from.as_deref(), Some("1.0.0"));
    assert_eq!(rolled, None);
    assert_eq!(
      args,
      a(&["--", "myapp://x", "--denext-update-rolled-back=9.9.9"])
    );
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
    if cfg!(target_os = "macos") {
      let resources = root.join("Fake.app/Contents/Resources");
      std::fs::create_dir_all(&resources).unwrap();
      std::fs::write(resources.join(layout::INSTALL_MARKER), b"").unwrap();
    } else {
      std::fs::write(exe.with_file_name(layout::INSTALL_MARKER), b"").unwrap();
    }
    let layout = layout::detect_install(&exe, &AppImageEnv::default()).unwrap();
    (exe, layout)
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

  fn verified(size: u64) -> VerifiedUpdate {
    VerifiedUpdate {
      version: "2.0.0".into(),
      min_version: None,
      required: false,
      platform: "test".into(),
      url: "https://example.com/app.tar.gz".into(),
      sha256: "00".repeat(32),
      size,
      release_notes: None,
      published_at: "2026-10-02T00:00:00Z".into(),
    }
  }

  fn configured(tmp: &tempfile::TempDir) -> (OpState, InstallLayout) {
    let (exe, layout) = fake_app(tmp);
    let mut state = OpState::new(None);
    state.put(host_config(exe, Some("key")));
    (state, layout)
  }

  fn expect_code<T>(r: Result<T, JsErrorBox>, code: &str) {
    let msg = match r {
      Ok(_) => panic!("expected {code}, got Ok"),
      Err(e) => code_of(e),
    };
    assert!(msg.contains(code), "expected {code}: {msg}");
  }

  // begin(): only a verified update; one download or stage at a time; never
  // while an installed update awaits confirmation.
  #[test]
  fn begin_needs_a_verified_update_and_an_idle_session() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut state, layout) = configured(&tmp);
    expect_code(update_begin(&mut state), "not_staged");

    session(&mut state).0.borrow_mut().verified = Some(verified(4));
    let out = update_begin(&mut state).unwrap();
    assert_eq!(out.version, "2.0.0");
    assert!(layout.staging_dir().join("download.part").exists());
    // A second begin() while that download is open is busy, and leaves it.
    expect_code(update_begin(&mut state), "busy");
    assert!(session(&mut state).0.borrow().sink.is_some());

    // So is begin() while a stage runs.
    abort_download(&mut state);
    session(&mut state).0.borrow_mut().staging = true;
    expect_code(update_begin(&mut state), "busy");
    session(&mut state).0.borrow_mut().staging = false;

    for phase in [Phase::Swapped, Phase::Swapping, Phase::RollingBack] {
      let mut st = UpdateState::new(&layout);
      st.phase = phase;
      st.to = Some("2.0.0".into());
      swap::write_state(&layout, &st).unwrap();
      expect_code(update_begin(&mut state), "busy");
      // The awaiting install is left as it was.
      assert_eq!(swap::read_state(&layout).unwrap().phase, phase);
    }
  }

  // A stage left by an earlier run is never applied blind: begin() starts
  // over from Idle.
  #[test]
  fn begin_resets_an_earlier_runs_stage() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut state, layout) = configured(&tmp);
    let mut st = UpdateState::new(&layout);
    st.phase = Phase::Staged;
    st.to = Some("1.5.0".into());
    st.entry = Some("old-entry".into());
    swap::write_state(&layout, &st).unwrap();

    session(&mut state).0.borrow_mut().verified = Some(verified(4));
    update_begin(&mut state).unwrap();
    let st = swap::read_state(&layout).unwrap();
    assert_eq!(st.phase, Phase::Idle);
    assert_eq!(st.entry, None);
    let session = session(&mut state).0.borrow();
    assert!(session.sink.is_some());
    assert_eq!(session.archive, None);
    assert_eq!(session.staged, None);
  }

  // write() / finish() without a download: not_staged, and the session is
  // left alone; a write past the declared size aborts the download.
  #[test]
  fn write_and_finish_need_a_download() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut state, layout) = configured(&tmp);
    let write = update_write;
    expect_code(write(&mut state, b"x"), "not_staged");
    session(&mut state).0.borrow_mut().verified = Some(verified(4));
    update_begin(&mut state).unwrap();
    assert_eq!(write(&mut state, b"ab").unwrap(), 2.0);
    // Past the declared size: refused, and the download is dropped.
    assert!(write(&mut state, b"cdefg").is_err());
    assert!(session(&mut state).0.borrow().sink.is_none());
    assert!(!layout.staging_dir().exists());
    expect_code(write(&mut state, b"x"), "not_staged");
  }

  // stage(): one at a time, only of a downloaded update.
  #[test]
  fn stage_needs_a_download_and_no_other_stage() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut state, _layout) = configured(&tmp);
    let code = |r: Result<_, UpdateError>| r.err().unwrap().code;
    assert_eq!(code(stage_start(&mut state)), Code::NotStaged);
    session(&mut state).0.borrow_mut().verified = Some(verified(4));
    assert_eq!(code(stage_start(&mut state)), Code::NotStaged);
    session(&mut state).0.borrow_mut().archive =
      Some(tmp.path().join("download.part"));
    let (_, update, _) = stage_start(&mut state).unwrap();
    assert_eq!(update.version, "2.0.0");
    assert!(session(&mut state).0.borrow().staging);
    // A second stage while the first runs is busy.
    assert_eq!(code(stage_start(&mut state)), Code::Busy);
  }

  // apply(): only what this run staged, and only while the install still
  // records that stage.
  #[test]
  fn apply_needs_this_runs_stage() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut state, layout) = configured(&tmp);
    expect_code(update_apply(&mut state), "not_staged");

    session(&mut state).0.borrow_mut().staged = Some("2.0.0".into());
    // Nothing recorded on disk: the stage is gone.
    expect_code(update_apply(&mut state), "not_staged");
    // A stage of another version, or a phase other than Staged, is not it.
    for (phase, to) in [
      (Phase::Staged, "3.0.0"),
      (Phase::Idle, "2.0.0"),
      (Phase::Swapped, "2.0.0"),
    ] {
      let mut st = UpdateState::new(&layout);
      st.phase = phase;
      st.to = Some(to.into());
      swap::write_state(&layout, &st).unwrap();
      expect_code(update_apply(&mut state), "not_staged");
      assert_eq!(swap::read_state(&layout).unwrap().phase, phase);
    }
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
