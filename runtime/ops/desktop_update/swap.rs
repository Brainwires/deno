// Copyright 2018-2026 the Deno authors. MIT license.

//! Swapping the install for the staged app, rolling back, and the state that
//! makes both survive a crash.
//!
//! A running app cannot replace itself, so the swap runs in a HELPER: the
//! app's own executable started as `<exe> run denext-update-helper <mode>
//! <pid>` (handled by the runtime before anything else starts; `run <arg>` is
//! also the laufey hosts' headless form, so no window or Dock icon appears).
//! It takes no paths: it finds the install from its own location and reads
//! the state file next to it.
//!
//! **The state file** (`.<name>.denext-update.json`, next to the install)
//! moves through `staged` → `swapping` → `swapped` → `idle` (confirmed), or
//! → `rollingBack` → `idle` (with `rejected` set). It is written atomically
//! (a temporary file renamed over it) BEFORE each step, so whatever was
//! interrupted is visible to the next process.
//!
//! **The swap.** macOS and Linux exchange the staged directory (or AppImage
//! file) with the install in ONE atomic rename (`renamex_np(RENAME_SWAP)` /
//! `renameat2(RENAME_EXCHANGE)`), then move the previous app to `<name>.old`:
//! there is no moment without an app at the install path. Where the file
//! system cannot exchange (and on Windows, which has no directory exchange)
//! it is two renames: install → `.old`, staged → install, undone on failure;
//! Windows retries a sharing violation (an antivirus scan) for a few seconds.
//! The helper's working directory is the install's parent, never inside it.
//!
//! **Confirm or roll back.** The new app is relaunched with
//! `--denext-updated-from=<version>`. Its first launch is its trial
//! (`launches` 0 → 1); [`confirm`] marks it good and deletes `.old`. If a
//! launch starts while the update is still unconfirmed from an earlier
//! launch (it crashed, hung, or never confirmed), the runtime spawns the
//! helper in `rollback` mode and exits; the helper puts `.old` back,
//! records the version as `rejected` (it is never offered again), and
//! relaunches the previous app with `--denext-update-rolled-back=<version>`.
//! An interrupted swap or rollback is recovered the same way.

#![allow(
  clippy::disallowed_methods,
  reason = "the updater swaps the app install, outside any user permission \
            sandbox, by design"
)]

use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use serde::Deserialize;
use serde::Serialize;

use super::error::UpdateError;
use super::error::UpdateErrorCode as Code;
use super::error::err;
use super::layout::InstallKind;
use super::layout::InstallLayout;

/// The helper's argv marker: `<exe> run denext-update-helper <mode> <pid>`.
pub const HELPER_ARG: &str = "denext-update-helper";
/// Passed to the relaunched new app.
pub const UPDATED_FROM_ARG: &str = "--denext-updated-from=";
/// Passed to the relaunched previous app after a rollback.
pub const ROLLED_BACK_ARG: &str = "--denext-update-rolled-back=";
/// How long the helper waits for the app to exit.
pub const HELPER_WAIT: Duration = Duration::from_secs(300);
/// The watchdog stops spawning recovery helpers after this many attempts.
pub const MAX_HELPER_ATTEMPTS: u32 = 3;
/// Windows: how long the helper then waits for the app's other processes
/// (a CEF subprocess outliving its browser process, an earlier launch) to
/// leave the install.
#[cfg(windows)]
pub const HELPER_WAIT_INSTALL_PROCESSES: Duration = Duration::from_secs(60);
const STATE_SCHEMA: u32 = 1;

/// Where an update is.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
  /// Nothing pending.
  #[default]
  Idle,
  /// Verified and extracted next to the install; waiting for apply.
  Staged,
  /// The helper is swapping (interrupted if seen at startup).
  Swapping,
  /// Swapped; the new version is on trial until confirmed.
  Swapped,
  /// The helper is rolling back (interrupted if seen at startup).
  RollingBack,
}

/// The state file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateState {
  pub schema: u32,
  pub phase: Phase,
  /// The install this state belongs to (refused if it differs).
  pub install: String,
  /// The version being replaced.
  pub from: Option<String>,
  /// The version being installed.
  pub to: Option<String>,
  /// The staged app's name inside the extraction directory.
  pub entry: Option<String>,
  /// Unix `dev:ino` of the install before the swap (tells an interrupted
  /// atomic exchange apart from one that never happened).
  pub install_id: Option<String>,
  /// Launches of the swapped-in version so far (its trial is launch 1).
  pub launches: u32,
  /// The trial launch's process: while it runs, another launch (a second
  /// instance) is not a sign that the trial failed.
  pub trial_pid: Option<u32>,
  /// Recovery helpers the watchdog started for the current phase.
  pub helper_attempts: u32,
  /// The last version rolled back after failing to start.
  pub rejected: Option<String>,
  /// The arguments to relaunch the app with.
  pub relaunch_args: Vec<String>,
  /// `.old` / staging still need deleting.
  pub cleanup: bool,
  /// Why the last step failed, if it did.
  pub last_error: Option<String>,
}

impl UpdateState {
  pub fn new(layout: &InstallLayout) -> Self {
    Self {
      schema: STATE_SCHEMA,
      install: layout.install.to_string_lossy().into_owned(),
      ..Default::default()
    }
  }

  /// The staged app's path, when one is named.
  pub fn staged_path(&self, layout: &InstallLayout) -> Option<PathBuf> {
    let entry = self.entry.as_deref()?;
    // The entry is a single plain name (it came from the safe extractor).
    if entry.is_empty()
      || entry.contains(['/', '\\', ':', '\0'])
      || entry == "."
      || entry == ".."
    {
      return None;
    }
    Some(layout.extract_dir().join(entry))
  }
}

/// Read the state for `layout` (`None`: no state, unreadable, or another
/// install's).
pub fn read_state(layout: &InstallLayout) -> Option<UpdateState> {
  let bytes = std::fs::read(layout.state_path()).ok()?;
  let state: UpdateState = serde_json::from_slice(&bytes).ok()?;
  (state.schema == STATE_SCHEMA
    && Path::new(&state.install) == layout.install.as_path())
  .then_some(state)
}

/// Write the state atomically (temporary file + rename, synced).
pub fn write_state(
  layout: &InstallLayout,
  state: &UpdateState,
) -> Result<(), UpdateError> {
  let path = layout.state_path();
  let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
  let bytes = serde_json::to_vec_pretty(state)
    .map_err(|e| UpdateError::io("state", e))?;
  let write = || -> std::io::Result<()> {
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    drop(f);
    rename_retry(&tmp, &path)
  };
  write().map_err(|e| {
    let _ = std::fs::remove_file(&tmp);
    UpdateError::io(path.display(), e)
  })
}

fn exists(path: &Path) -> bool {
  std::fs::symlink_metadata(path).is_ok()
}

/// `dev:ino` of `path` (Unix), else `None`.
pub fn file_id(path: &Path) -> Option<String> {
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::symlink_metadata(path).ok()?;
    Some(format!("{}:{}", m.dev(), m.ino()))
  }
  #[cfg(not(unix))]
  {
    let _ = path;
    None
  }
}

/// Atomically exchange `a` and `b` (both must exist). `Ok(false)` when the
/// OS or file system cannot (then nothing changed).
pub fn exchange(a: &Path, b: &Path) -> std::io::Result<bool> {
  #[cfg(any(target_os = "macos", target_os = "linux"))]
  {
    use std::os::unix::ffi::OsStrExt;
    let ca = std::ffi::CString::new(a.as_os_str().as_bytes())?;
    let cb = std::ffi::CString::new(b.as_os_str().as_bytes())?;
    #[cfg(target_os = "macos")]
    // SAFETY: valid NUL-terminated paths.
    let rc =
      unsafe { libc::renamex_np(ca.as_ptr(), cb.as_ptr(), libc::RENAME_SWAP) };
    #[cfg(target_os = "linux")]
    // SAFETY: valid NUL-terminated paths; renameat2's documented signature.
    let rc = unsafe {
      libc::syscall(
        libc::SYS_renameat2,
        libc::AT_FDCWD,
        ca.as_ptr(),
        libc::AT_FDCWD,
        cb.as_ptr(),
        libc::RENAME_EXCHANGE,
      ) as libc::c_int
    };
    if rc == 0 {
      return Ok(true);
    }
    let e = std::io::Error::last_os_error();
    match e.raw_os_error() {
      Some(libc::ENOTSUP) | Some(libc::EINVAL) | Some(libc::ENOSYS) => {
        Ok(false)
      }
      #[allow(unreachable_patterns, reason = "EOPNOTSUPP == ENOTSUP on Linux")]
      Some(libc::EOPNOTSUPP) => Ok(false),
      _ => Err(e),
    }
  }
  #[cfg(not(any(target_os = "macos", target_os = "linux")))]
  {
    let _ = (a, b);
    Ok(false)
  }
}

/// `rename`, retried on Windows while something (an antivirus scan, a
/// just-exited process) briefly holds the path.
pub fn rename_retry(from: &Path, to: &Path) -> std::io::Result<()> {
  let deadline = Instant::now() + Duration::from_secs(10);
  loop {
    match std::fs::rename(from, to) {
      Ok(()) => return Ok(()),
      Err(e) if cfg!(windows) && Instant::now() < deadline => {
        // ERROR_ACCESS_DENIED (5) / ERROR_SHARING_VIOLATION (32).
        if matches!(e.raw_os_error(), Some(5) | Some(32)) {
          std::thread::sleep(Duration::from_millis(200));
          continue;
        }
        return Err(e);
      }
      Err(e) => return Err(e),
    }
  }
}

/// Remove a file or directory tree; `true` when nothing is left.
pub fn remove_path(path: &Path) -> bool {
  let Ok(m) = std::fs::symlink_metadata(path) else {
    return true;
  };
  for _ in 0..if cfg!(windows) { 10 } else { 1 } {
    let r = if m.file_type().is_dir() {
      std::fs::remove_dir_all(path)
    } else {
      std::fs::remove_file(path)
    };
    if r.is_ok() || !exists(path) {
      return true;
    }
    std::thread::sleep(Duration::from_millis(200));
  }
  false
}

/// A seam for fault injection in tests: called before each rename step with
/// its name; an `Err` aborts the step as if the rename had failed.
pub type FaultHook<'a> = &'a dyn Fn(&str) -> std::io::Result<()>;

fn no_fault(_: &str) -> std::io::Result<()> {
  Ok(())
}

fn step(
  hook: FaultHook,
  name: &str,
  f: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
  hook(name)?;
  f()
}

/// The helper's `apply`: swap the staged app into place. On failure the
/// install is left as it was (the previous app) and the state goes back to
/// `staged` with `last_error`. On success the state is `swapped` (trial
/// pending) and the staging directory is removed.
pub fn apply_swap(
  layout: &InstallLayout,
  state: &mut UpdateState,
) -> Result<(), UpdateError> {
  apply_swap_with(layout, state, &no_fault)
}

pub fn apply_swap_with(
  layout: &InstallLayout,
  state: &mut UpdateState,
  hook: FaultHook,
) -> Result<(), UpdateError> {
  if state.phase != Phase::Staged {
    return err(Code::NotStaged, "nothing is staged");
  }
  let Some(staged) = state.staged_path(layout) else {
    return err(Code::NotStaged, "the state names no staged app");
  };
  if !exists(&staged) || !exists(&layout.install) {
    return err(Code::NotStaged, "the staged app or the install is missing");
  }
  let old = layout.old_path();
  if exists(&old) && !remove_path(&old) {
    return err(
      Code::Io,
      format!("cannot remove a leftover {}", old.display()),
    );
  }
  state.phase = Phase::Swapping;
  state.install_id = file_id(&layout.install);
  state.last_error = None;
  write_state(layout, state)?;

  let result = (|| -> std::io::Result<()> {
    let exchanged = if hook("exchange").is_ok() {
      exchange(&staged, &layout.install)?
    } else {
      false
    };
    if exchanged {
      // The install is the new app; `staged` holds the previous one.
      if let Err(e) = step(hook, "old", || rename_retry(&staged, &old)) {
        // Put the previous app back (exchange again) before failing.
        let _ = exchange(&staged, &layout.install);
        return Err(e);
      }
      return Ok(());
    }
    step(hook, "install->old", || rename_retry(&layout.install, &old))?;
    if let Err(e) = step(hook, "staged->install", || {
      rename_retry(&staged, &layout.install)
    }) {
      // Undo: the previous app back at the install path.
      let _ = rename_retry(&old, &layout.install);
      return Err(e);
    }
    Ok(())
  })();

  match result {
    Ok(()) => {
      state.phase = Phase::Swapped;
      state.launches = 0;
      state.helper_attempts = 0;
      state.entry = None;
      write_state(layout, state)?;
      remove_path(&layout.staging_dir());
      Ok(())
    }
    Err(e) => {
      state.phase = Phase::Staged;
      state.last_error = Some(format!("swap failed: {e}"));
      let _ = write_state(layout, state);
      err(Code::Io, format!("the swap failed and was undone: {e}"))
    }
  }
}

/// The helper's `rollback`: restore `.old` after a version failed its trial
/// (`swapped`), or recover an interrupted `swapping` / `rollingBack`.
/// Returns the version that was rolled back (`None` when nothing was
/// swapped in, e.g. an interrupted swap that had not happened yet).
pub fn rollback(
  layout: &InstallLayout,
  state: &mut UpdateState,
) -> Result<Option<String>, UpdateError> {
  rollback_with(layout, state, &no_fault)
}

pub fn rollback_with(
  layout: &InstallLayout,
  state: &mut UpdateState,
  hook: FaultHook,
) -> Result<Option<String>, UpdateError> {
  let old = layout.old_path();
  let failed = layout.failed_path();
  let install = &layout.install;
  // A version that failed its trial is rejected. The mark is recorded when
  // the rollback starts, so a resumed rollback still knows.
  let reject = match state.phase {
    Phase::Swapped => {
      state.rejected = state.to.clone();
      true
    }
    Phase::RollingBack => state.to.is_some() && state.rejected == state.to,
    Phase::Swapping => {
      // Was the swap interrupted after it happened?
      let swapped = if !exists(install) {
        // Mid two-rename: the install is at `.old`.
        false
      } else if cfg!(windows) || state.install_id.is_none() {
        exists(&old)
      } else {
        file_id(install) != state.install_id
      };
      if exists(install) && !swapped {
        // Never happened: the previous app is in place, the staged app is
        // intact. Back to `staged` (it may be applied again).
        state.phase = Phase::Staged;
        state.last_error =
          Some("the swap was interrupted before it ran".into());
        write_state(layout, state)?;
        return Ok(None);
      }
      if swapped && !exists(&old) {
        // An atomic exchange whose follow-up rename was interrupted: the
        // previous app is still in the staging directory.
        if let Some(staged) = state.staged_path(layout)
          && exists(&staged)
        {
          rename_retry(&staged, &old)
            .map_err(|e| UpdateError::io(old.display(), e))?;
        }
      }
      false
    }
    Phase::Idle | Phase::Staged => {
      return err(Code::NotStaged, "there is no update to roll back");
    }
  };
  let to = state.to.clone();
  state.phase = Phase::RollingBack;
  state.install_id = file_id(install);
  write_state(layout, state)?;

  // Restore: the failed install aside, `.old` back. Each step is checked so
  // a rerun after an interruption picks up where this one stopped.
  let restore = (|| -> std::io::Result<()> {
    if exists(&old) {
      if exists(install) {
        if exists(&failed) {
          remove_path(&failed);
        }
        step(hook, "install->failed", || rename_retry(install, &failed))?;
      }
      if let Err(e) = step(hook, "old->install", || rename_retry(&old, install))
      {
        // Never leave the install path empty.
        if !exists(install) && exists(&failed) {
          let _ = rename_retry(&failed, install);
        }
        return Err(e);
      }
    } else if !exists(install) && exists(&failed) {
      // Nothing to restore; at least put an app back.
      rename_retry(&failed, install)?;
    }
    Ok(())
  })();
  if let Err(e) = restore {
    state.last_error = Some(format!("rollback failed: {e}"));
    let _ = write_state(layout, state);
    return err(Code::Io, format!("the rollback failed: {e}"));
  }
  // On Windows the helper runs from the failed install's own executable, which
  // cannot be deleted while it runs: leave `cleanup` set and the next start's
  // watchdog finishes it.
  let failed_gone = remove_path(&failed);
  let staging_gone = remove_path(&layout.staging_dir());
  state.phase = Phase::Idle;
  state.launches = 0;
  state.helper_attempts = 0;
  state.entry = None;
  state.install_id = None;
  state.cleanup = !(failed_gone && staging_gone);
  state.last_error = None;
  write_state(layout, state)?;
  Ok(if reject { to } else { None })
}

/// Confirm the running (swapped-in) version: the state goes `idle` first,
/// then `.old` and any staging are deleted (a crash in between leaves
/// `cleanup`, finished at the next start). `Ok(false)` when there was
/// nothing to confirm.
pub fn confirm(layout: &InstallLayout) -> Result<bool, UpdateError> {
  let Some(mut state) = read_state(layout) else {
    return Ok(false);
  };
  if state.phase != Phase::Swapped {
    return Ok(false);
  }
  state.phase = Phase::Idle;
  state.launches = 0;
  state.helper_attempts = 0;
  state.rejected = None;
  state.cleanup = true;
  state.last_error = None;
  write_state(layout, &state)?;
  cleanup(layout, &mut state);
  Ok(true)
}

/// Delete `.old`, a failed install and staging; clear `cleanup` when done.
pub fn cleanup(layout: &InstallLayout, state: &mut UpdateState) {
  let done = remove_path(&layout.old_path())
    & remove_path(&layout.failed_path())
    & remove_path(&layout.staging_dir());
  if done && state.cleanup {
    state.cleanup = false;
    let _ = write_state(layout, state);
  }
}

/// What the runtime does at startup about a pending update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupAction {
  /// Start normally. `trial` is set on the first launch of a new version.
  Continue { trial: bool },
  /// Spawn the helper in `rollback` mode and exit without starting.
  RollBack,
}

/// The startup watchdog: decide (and record) what this launch does. It never
/// fails the launch: a state it cannot read or write means "continue".
pub fn startup_action(layout: &InstallLayout) -> StartupAction {
  let Some(mut state) = read_state(layout) else {
    return StartupAction::Continue { trial: false };
  };
  match state.phase {
    Phase::Idle => {
      if state.cleanup {
        cleanup(layout, &mut state);
      }
      StartupAction::Continue { trial: false }
    }
    Phase::Staged => StartupAction::Continue { trial: false },
    Phase::Swapped if state.launches == 0 => {
      state.launches = 1;
      state.trial_pid = Some(std::process::id());
      // If this cannot be recorded a crash goes undetected, but the launch
      // still starts (the update stays unconfirmed until confirm()).
      let _ = write_state(layout, &state);
      StartupAction::Continue { trial: true }
    }
    Phase::Swapped
      if state
        .trial_pid
        .is_some_and(|pid| !wait_for_exit(pid, Duration::ZERO)) =>
    {
      // The trial launch is still running: this is a second instance.
      StartupAction::Continue { trial: false }
    }
    Phase::Swapped | Phase::Swapping | Phase::RollingBack => {
      if state.helper_attempts >= MAX_HELPER_ATTEMPTS {
        // Give up rather than loop: start whatever is installed.
        state.phase = Phase::Idle;
        state.last_error = Some(
          "recovery did not complete after several attempts; left as is".into(),
        );
        state.cleanup = false;
        let _ = write_state(layout, &state);
        return StartupAction::Continue { trial: false };
      }
      state.helper_attempts += 1;
      // Only roll back when the attempt is recorded (no unbounded loop).
      if write_state(layout, &state).is_ok() {
        StartupAction::RollBack
      } else {
        StartupAction::Continue { trial: false }
      }
    }
  }
}

/// Parse the helper's argv (`<exe> run denext-update-helper <mode> <pid>`).
pub fn parse_helper_args(args: &[String]) -> Option<(HelperMode, u32)> {
  if args.len() != 5 || args[1] != "run" || args[2] != HELPER_ARG {
    return None;
  }
  let mode = match args[3].as_str() {
    "apply" => HelperMode::Apply,
    "rollback" => HelperMode::Rollback,
    _ => return None,
  };
  Some((mode, args[4].parse().ok()?))
}

/// What the helper was started to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperMode {
  Apply,
  Rollback,
}

impl HelperMode {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Apply => "apply",
      Self::Rollback => "rollback",
    }
  }
}

/// Append a line to the helper log next to the install (the helper has no
/// console). Capped: a log over 256 KiB is truncated first.
pub fn log_line(layout: &InstallLayout, line: &str) {
  let path = layout
    .parent
    .join(format!(".{}.denext-update.log", layout.name));
  if std::fs::metadata(&path).is_ok_and(|m| m.len() > 256 * 1024) {
    let _ = std::fs::remove_file(&path);
  }
  if let Ok(mut f) = std::fs::OpenOptions::new()
    .create(true)
    .append(true)
    .open(&path)
  {
    let now = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map(|d| d.as_secs())
      .unwrap_or(0);
    let _ = writeln!(f, "{now} [{}] {line}", std::process::id());
  }
}

/// Run the helper. Returns the process exit code.
pub fn run_helper(layout: &InstallLayout, mode: HelperMode, pid: u32) -> i32 {
  log_line(
    layout,
    &format!("helper {} waiting for pid {pid}", mode.as_str()),
  );
  if !wait_for_exit(pid, HELPER_WAIT) {
    log_line(layout, "the app did not exit in time; nothing changed");
    return 1;
  }
  // Windows refuses to rename a directory while a process holds one of its
  // files open without delete sharing: the app's CEF subprocesses (which
  // open the .pak / ICU data that way) can outlive the process that was
  // waited for by seconds, and a crashed trial's by longer. Wait for every
  // process running from the install to leave, so the renames below don't
  // run out of retries. (This helper runs from the install too; it is not
  // waited for.)
  #[cfg(windows)]
  if !wait_for_processes_in(&layout.install, HELPER_WAIT_INSTALL_PROCESSES) {
    log_line(
      layout,
      "processes still run from the install; trying the swap anyway",
    );
  }
  let Some(mut state) = read_state(layout) else {
    log_line(layout, "no update state for this install");
    return 1;
  };
  match mode {
    HelperMode::Apply => match apply_swap(layout, &mut state) {
      Ok(()) => {
        let from = state.from.clone().unwrap_or_default();
        log_line(layout, &format!("swapped {from} -> {:?}", state.to));
        relaunch(layout, &state, Some(format!("{UPDATED_FROM_ARG}{from}")))
      }
      Err(e) => {
        log_line(layout, &format!("apply failed: {e}"));
        // The previous app is in place (or the state could not be read):
        // bring it back up only when the state was recorded.
        if read_state(layout).is_some_and(|s| s.phase == Phase::Staged) {
          relaunch(layout, &state, None);
        }
        1
      }
    },
    HelperMode::Rollback => match rollback(layout, &mut state) {
      Ok(rolled_back) => {
        log_line(layout, &format!("rolled back {rolled_back:?}"));
        relaunch(
          layout,
          &state,
          rolled_back.map(|v| format!("{ROLLED_BACK_ARG}{v}")),
        )
      }
      Err(e) => {
        log_line(layout, &format!("rollback failed: {e}"));
        1
      }
    },
  }
}

/// Wait up to `timeout` for process `pid` to exit.
pub fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
  #[cfg(unix)]
  {
    let deadline = Instant::now() + timeout;
    loop {
      // SAFETY: signal 0 only checks for existence.
      let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
      if !alive {
        return true;
      }
      if Instant::now() >= deadline {
        return false;
      }
      std::thread::sleep(Duration::from_millis(100));
    }
  }
  #[cfg(windows)]
  {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::OpenProcess;
    use windows_sys::Win32::System::Threading::PROCESS_SYNCHRONIZE;
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    // SAFETY: plain Win32 calls; the handle is closed.
    unsafe {
      let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
      if h.is_null() {
        return true; // gone (or never existed)
      }
      let r = WaitForSingleObject(h, timeout.as_millis() as u32);
      CloseHandle(h);
      r == WAIT_OBJECT_0
    }
  }
}

/// Windows: wait up to `timeout` until no process other than this one runs
/// an executable from inside `dir`. `true` when none is left.
#[cfg(windows)]
pub fn wait_for_processes_in(dir: &Path, timeout: Duration) -> bool {
  let deadline = Instant::now() + timeout;
  let prefix = windows_path_key(dir, true);
  loop {
    let running = processes_in(&prefix);
    if running.is_empty() {
      return true;
    }
    let now = Instant::now();
    let done = now >= deadline;
    // Wait on one of them (at most a second, then look again: others may
    // have started or ended meanwhile).
    let wait = if done {
      0
    } else {
      (deadline - now).as_millis().min(1000) as u32
    };
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    // SAFETY: handles opened by processes_in, each closed once.
    unsafe {
      WaitForSingleObject(running[0], wait);
      for h in running {
        CloseHandle(h);
      }
    }
    if done {
      return false;
    }
  }
}

/// A path as a case-insensitive comparison key: `\\?\` stripped, `/` as
/// `\`, lower-cased, and (`dir`) ending in `\`.
#[cfg(windows)]
fn windows_path_key(path: &Path, dir: bool) -> String {
  let mut s = path.to_string_lossy().replace('/', "\\");
  if let Some(rest) = s.strip_prefix("\\\\?\\UNC\\") {
    s = format!("\\\\{rest}");
  } else if let Some(rest) = s.strip_prefix("\\\\?\\") {
    s = rest.to_string();
  }
  let mut s = s.to_lowercase();
  if dir && !s.ends_with('\\') {
    s.push('\\');
  }
  s
}

/// Handles (SYNCHRONIZE) of the processes other than this one whose
/// executable is under `prefix` (a [`windows_path_key`] of a directory).
#[cfg(windows)]
fn processes_in(prefix: &str) -> Vec<windows_sys::Win32::Foundation::HANDLE> {
  use windows_sys::Win32::Foundation::CloseHandle;
  use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
  use windows_sys::Win32::System::Diagnostics::ToolHelp::CreateToolhelp32Snapshot;
  use windows_sys::Win32::System::Diagnostics::ToolHelp::PROCESSENTRY32W;
  use windows_sys::Win32::System::Diagnostics::ToolHelp::Process32FirstW;
  use windows_sys::Win32::System::Diagnostics::ToolHelp::Process32NextW;
  use windows_sys::Win32::System::Diagnostics::ToolHelp::TH32CS_SNAPPROCESS;
  use windows_sys::Win32::System::Threading::OpenProcess;
  use windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;
  use windows_sys::Win32::System::Threading::PROCESS_SYNCHRONIZE;
  use windows_sys::Win32::System::Threading::QueryFullProcessImageNameW;
  let me = std::process::id();
  let mut out = Vec::new();
  // SAFETY: a process snapshot walked with a correctly sized entry; every
  // handle is closed or returned.
  unsafe {
    let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
    if snap == INVALID_HANDLE_VALUE {
      return out;
    }
    let mut entry: PROCESSENTRY32W = std::mem::zeroed();
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    let mut more = Process32FirstW(snap, &mut entry) != 0;
    while more {
      let pid = entry.th32ProcessID;
      if pid != 0 && pid != me {
        let h = OpenProcess(
          PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
          0,
          pid,
        );
        if !h.is_null() {
          let mut buf = vec![0u16; 32768];
          let mut len = buf.len() as u32;
          let inside =
            QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) != 0
              && windows_path_key(
                Path::new(&String::from_utf16_lossy(&buf[..len as usize])),
                false,
              )
              .starts_with(prefix);
          if inside {
            out.push(h);
          } else {
            CloseHandle(h);
          }
        }
      }
      more = Process32NextW(snap, &mut entry) != 0;
    }
    CloseHandle(snap);
  }
  out
}

/// Start `program` detached from this process (its own process group /
/// no console, outliving us), with the working directory `cwd`.
pub fn spawn_detached(
  program: &Path,
  args: &[String],
  cwd: &Path,
  env: &[(&str, Option<&str>)],
) -> std::io::Result<()> {
  let mut cmd = std::process::Command::new(program);
  cmd
    .args(args)
    .current_dir(cwd)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null());
  for (k, v) in env {
    match v {
      Some(v) => cmd.env(k, v),
      None => cmd.env_remove(k),
    };
  }
  #[cfg(unix)]
  {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
    cmd.spawn().map(|_| ())
  }
  #[cfg(windows)]
  {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    cmd.creation_flags(
      DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB,
    );
    match cmd.spawn() {
      Ok(_) => Ok(()),
      // A job that forbids breakaway: start inside it instead.
      Err(_) => {
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        cmd.spawn().map(|_| ())
      }
    }
  }
}

/// Start the helper (`<exe> run denext-update-helper <mode> <our pid>`): the
/// installed executable (an AppImage's file, not its mount), working
/// directory the install's parent, single-instance forwarding off (it must
/// not be handed to the running app as a second instance).
pub fn spawn_helper(
  layout: &InstallLayout,
  mode: HelperMode,
) -> std::io::Result<()> {
  let exe = layout.exe();
  // The laufey macOS hosts' headless (`run <arg>`) path finds the runtime
  // only through LAUFEY_RUNTIME_PATH or a co-located `<exe>.dylib`, while a
  // bundle ships `libruntime.dylib` (which only their windowed path
  // searches): point the helper at it. Windows and Linux hosts find their
  // co-located `<App>.dll` / `<App>.so` themselves.
  let runtime = if layout.kind == InstallKind::MacBundle
    && std::env::var_os("LAUFEY_RUNTIME_PATH").is_none()
  {
    bundle_runtime_path(&exe)
  } else {
    None
  };
  let runtime = runtime.as_ref().map(|p| p.to_string_lossy().into_owned());
  let mut env = vec![("LAUFEY_SINGLE_INSTANCE", Some("0"))];
  if let Some(path) = runtime.as_deref() {
    env.push(("LAUFEY_RUNTIME_PATH", Some(path)));
  }
  spawn_detached(
    &exe,
    &[
      "run".into(),
      HELPER_ARG.into(),
      mode.as_str().into(),
      std::process::id().to_string(),
    ],
    &layout.parent,
    &env,
  )
}

/// The runtime library a macOS bundle's host loads, in the host's own search
/// order: `Contents/Frameworks/libruntime.dylib`, then
/// `Contents/MacOS/libruntime.dylib` (`exe` is `Contents/MacOS/<exe>`).
pub fn bundle_runtime_path(exe: &Path) -> Option<PathBuf> {
  let macos = exe.parent()?;
  let candidates = [
    macos.parent()?.join("Frameworks").join("libruntime.dylib"),
    macos.join("libruntime.dylib"),
  ];
  candidates.into_iter().find(|p| p.is_file())
}

/// Relaunch the installed app with the recorded arguments plus `marker`.
fn relaunch(
  layout: &InstallLayout,
  state: &UpdateState,
  marker: Option<String>,
) -> i32 {
  let mut args: Vec<String> = state
    .relaunch_args
    .iter()
    .filter(|a| !is_update_marker(a))
    .cloned()
    .collect();
  args.extend(marker);
  let env = [("LAUFEY_SINGLE_INSTANCE", None)];
  let r = if layout.kind == InstallKind::MacBundle {
    // Through LaunchServices, so the bundle's LSEnvironment and activation
    // apply as for any launch.
    let mut open_args = vec![
      "-n".to_string(),
      layout.install.to_string_lossy().into_owned(),
    ];
    if !args.is_empty() {
      open_args.push("--args".into());
      open_args.extend(args);
    }
    spawn_detached(Path::new("/usr/bin/open"), &open_args, &layout.parent, &env)
  } else {
    spawn_detached(&layout.exe(), &args, &layout.parent, &env)
  };
  match r {
    Ok(()) => 0,
    Err(e) => {
      log_line(layout, &format!("relaunch failed: {e}"));
      1
    }
  }
}

/// Whether `arg` is one of the updater's relaunch markers.
pub fn is_update_marker(arg: &str) -> bool {
  arg.starts_with(UPDATED_FROM_ARG) || arg.starts_with(ROLLED_BACK_ARG)
}

#[cfg(test)]
mod tests {
  use super::*;

  /// The helper's wait for processes running from the install: a process
  /// started from a copy of a system executable in a scratch directory keeps
  /// the wait from finishing until it exits; one elsewhere does not count.
  #[cfg(windows)]
  #[test]
  fn waits_for_processes_running_from_the_install() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let system = std::env::var("SystemRoot").unwrap_or("C:\\Windows".into());
    let ping = dir.join("ping.exe");
    std::fs::copy(Path::new(&system).join("System32").join("ping.exe"), &ping)
      .unwrap();
    assert!(wait_for_processes_in(&dir, Duration::ZERO));
    let mut child = std::process::Command::new(&ping)
      .args(["-n", "4", "127.0.0.1"])
      .stdout(std::process::Stdio::null())
      .spawn()
      .unwrap();
    // A key in the `\\?\` form names the same directory.
    let verbatim = PathBuf::from(format!("\\\\?\\{}", dir.display()));
    assert!(!wait_for_processes_in(
      &verbatim,
      Duration::from_millis(300)
    ));
    assert!(wait_for_processes_in(&dir, Duration::from_secs(30)));
    child.wait().unwrap();
    assert_eq!(
      windows_path_key(Path::new("\\\\?\\C:\\A/b"), true),
      "c:\\a\\b\\"
    );
  }

  fn exchange_supported(dir: &Path) -> bool {
    let a = dir.join(".xa");
    let b = dir.join(".xb");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    let ok = exchange(&a, &b).unwrap_or(false);
    std::fs::remove_dir(&a).unwrap();
    std::fs::remove_dir(&b).unwrap();
    ok
  }

  struct Fixture {
    _tmp: tempfile::TempDir,
    layout: InstallLayout,
  }

  /// An install `App` holding `version`, and a staged `App` holding `new`.
  fn fixture(staged: Option<&str>) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let parent = std::fs::canonicalize(tmp.path()).unwrap();
    let install = parent.join("App");
    std::fs::create_dir_all(&install).unwrap();
    std::fs::write(install.join("version"), "1.0.0").unwrap();
    let layout = InstallLayout {
      kind: InstallKind::AppDir,
      install,
      parent,
      name: "App".into(),
      exe_rel: PathBuf::from("app"),
    };
    let mut state = UpdateState::new(&layout);
    if let Some(v) = staged {
      let dir = layout.extract_dir().join("App");
      std::fs::create_dir_all(&dir).unwrap();
      std::fs::write(dir.join("version"), v).unwrap();
      state.phase = Phase::Staged;
      state.from = Some("1.0.0".into());
      state.to = Some(v.into());
      state.entry = Some("App".into());
    }
    write_state(&layout, &state).unwrap();
    Fixture { _tmp: tmp, layout }
  }

  /// Point the recorded trial at a process that has exited.
  fn mark_trial_exited(l: &InstallLayout) {
    let mut child = if cfg!(windows) {
      std::process::Command::new("cmd")
        .args(["/C", "exit 0"])
        .spawn()
    } else {
      std::process::Command::new("true").spawn()
    }
    .unwrap();
    let pid = child.id();
    child.wait().unwrap();
    let mut s = read_state(l).unwrap();
    s.trial_pid = Some(pid);
    write_state(l, &s).unwrap();
  }

  fn installed(l: &InstallLayout) -> String {
    std::fs::read_to_string(l.install.join("version")).unwrap()
  }

  #[test]
  fn apply_confirm_round_trip() {
    let f = fixture(Some("2.0.0"));
    let l = &f.layout;
    let mut s = read_state(l).unwrap();
    apply_swap(l, &mut s).unwrap();
    assert_eq!(installed(l), "2.0.0");
    assert!(l.old_path().join("version").exists());
    assert!(!l.staging_dir().exists());
    assert_eq!(read_state(l).unwrap().phase, Phase::Swapped);
    // First launch: the trial.
    assert_eq!(startup_action(l), StartupAction::Continue { trial: true });
    assert!(confirm(l).unwrap());
    assert!(!l.old_path().exists());
    let s = read_state(l).unwrap();
    assert_eq!(s.phase, Phase::Idle);
    assert!(!s.cleanup);
    assert_eq!(startup_action(l), StartupAction::Continue { trial: false });
    assert!(!confirm(l).unwrap());
  }

  #[test]
  fn unconfirmed_second_launch_rolls_back_and_rejects() {
    let f = fixture(Some("2.0.0"));
    let l = &f.layout;
    let mut s = read_state(l).unwrap();
    apply_swap(l, &mut s).unwrap();
    assert_eq!(startup_action(l), StartupAction::Continue { trial: true });
    // While the trial (here: this test process) runs, another launch is a
    // second instance, not a failed trial.
    assert_eq!(startup_action(l), StartupAction::Continue { trial: false });
    // Crash before confirm(): the next launch rolls back.
    mark_trial_exited(l);
    assert_eq!(startup_action(l), StartupAction::RollBack);
    let mut s = read_state(l).unwrap();
    assert_eq!(rollback(l, &mut s).unwrap().as_deref(), Some("2.0.0"));
    assert_eq!(installed(l), "1.0.0");
    assert!(!l.old_path().exists());
    assert!(!l.failed_path().exists());
    let s = read_state(l).unwrap();
    assert_eq!(s.phase, Phase::Idle);
    assert_eq!(s.rejected.as_deref(), Some("2.0.0"));
    assert_eq!(startup_action(l), StartupAction::Continue { trial: false });
  }

  #[test]
  fn interrupted_swap_is_undone() {
    // Every rename step of the swap fails in turn: the previous app stays
    // (or is put back) at the install path and the state is `staged`.
    for failing in ["old", "install->old", "staged->install"] {
      let f = fixture(Some("2.0.0"));
      let l = &f.layout;
      if failing == "old" && !exchange_supported(&l.parent) {
        continue; // no atomic exchange here: the step does not exist
      }
      let mut s = read_state(l).unwrap();
      let no_exchange = failing != "old";
      let hook = |name: &str| -> std::io::Result<()> {
        if (name == "exchange" && no_exchange) || name == failing {
          return Err(std::io::Error::other("injected"));
        }
        Ok(())
      };
      let e = apply_swap_with(l, &mut s, &hook).unwrap_err();
      assert_eq!(e.code, Code::Io, "{failing}");
      assert_eq!(installed(l), "1.0.0", "{failing}");
      let s = read_state(l).unwrap();
      assert_eq!(s.phase, Phase::Staged, "{failing}");
      assert!(s.last_error.is_some());
      // The staged app is intact for a retry.
      assert_eq!(
        std::fs::read_to_string(l.extract_dir().join("App/version")).unwrap(),
        "2.0.0",
        "{failing}"
      );
      assert!(!l.old_path().exists(), "{failing}");
    }
  }

  #[test]
  fn helper_killed_mid_two_rename_swap_recovers() {
    // The helper died between install -> .old and staged -> install: no
    // install. The next run of the helper (from .old's copy, or a rerun)
    // restores the previous app.
    let f = fixture(Some("2.0.0"));
    let l = &f.layout;
    let mut s = read_state(l).unwrap();
    s.phase = Phase::Swapping;
    s.install_id = file_id(&l.install);
    write_state(l, &s).unwrap();
    std::fs::rename(&l.install, l.old_path()).unwrap();
    assert_eq!(startup_action(l), StartupAction::RollBack);
    let mut s = read_state(l).unwrap();
    assert_eq!(rollback(l, &mut s).unwrap(), None);
    assert_eq!(installed(l), "1.0.0");
    let s = read_state(l).unwrap();
    assert_eq!(s.phase, Phase::Idle);
    assert_eq!(
      s.rejected, None,
      "an interrupted swap is not the version's fault"
    );
  }

  #[cfg(any(target_os = "macos", target_os = "linux"))]
  #[test]
  fn helper_killed_after_atomic_exchange_recovers() {
    let f = fixture(Some("2.0.0"));
    let l = &f.layout;
    let mut s = read_state(l).unwrap();
    s.phase = Phase::Swapping;
    s.install_id = file_id(&l.install);
    write_state(l, &s).unwrap();
    let staged = s.staged_path(l).unwrap();
    if !exchange(&staged, &l.install).unwrap() {
      return; // this file system cannot exchange
    }
    // Killed before `staged -> .old`: the new app is installed, the old one
    // sits in staging.
    let mut s = read_state(l).unwrap();
    assert_eq!(rollback(l, &mut s).unwrap(), None);
    assert_eq!(installed(l), "1.0.0");
    assert_eq!(read_state(l).unwrap().phase, Phase::Idle);
  }

  #[test]
  fn swap_interrupted_before_it_ran_keeps_the_stage() {
    let f = fixture(Some("2.0.0"));
    let l = &f.layout;
    let mut s = read_state(l).unwrap();
    s.phase = Phase::Swapping;
    s.install_id = file_id(&l.install);
    write_state(l, &s).unwrap();
    let mut s = read_state(l).unwrap();
    assert_eq!(rollback(l, &mut s).unwrap(), None);
    assert_eq!(installed(l), "1.0.0");
    assert_eq!(read_state(l).unwrap().phase, Phase::Staged);
  }

  #[test]
  fn interrupted_rollback_resumes() {
    for failing in ["install->failed", "old->install"] {
      let f = fixture(Some("2.0.0"));
      let l = &f.layout;
      let mut s = read_state(l).unwrap();
      apply_swap(l, &mut s).unwrap();
      let mut s = read_state(l).unwrap();
      let hook = |name: &str| -> std::io::Result<()> {
        if name == failing {
          return Err(std::io::Error::other("injected"));
        }
        Ok(())
      };
      assert!(rollback_with(l, &mut s, &hook).is_err(), "{failing}");
      // Never an empty install path.
      assert!(l.install.exists(), "{failing}");
      // A rerun (the next helper) completes it.
      let mut s = read_state(l).unwrap();
      assert_eq!(s.phase, Phase::RollingBack, "{failing}");
      assert_eq!(rollback(l, &mut s).unwrap().as_deref(), Some("2.0.0"));
      assert_eq!(installed(l), "1.0.0", "{failing}");
      assert!(!l.failed_path().exists());
    }
  }

  #[cfg(unix)]
  #[test]
  fn undeletable_failed_install_is_cleaned_at_next_start() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: getuid has no preconditions.
    if unsafe { libc::getuid() } == 0 {
      return; // root ignores the mode bits this relies on
    }
    let f = fixture(Some("2.0.0"));
    let l = &f.layout;
    let mut s = read_state(l).unwrap();
    apply_swap(l, &mut s).unwrap();
    // The new install holds a read-only directory with a file: removing the
    // failed install fails (as a running exe does on Windows).
    let locked = l.install.join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::write(locked.join("f"), b"x").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555))
      .unwrap();
    let mut s = read_state(l).unwrap();
    assert_eq!(rollback(l, &mut s).unwrap().as_deref(), Some("2.0.0"));
    assert_eq!(installed(l), "1.0.0");
    assert!(l.failed_path().exists());
    assert!(read_state(l).unwrap().cleanup);
    std::fs::set_permissions(
      l.failed_path().join("locked"),
      std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert_eq!(startup_action(l), StartupAction::Continue { trial: false });
    assert!(!l.failed_path().exists());
    assert!(!read_state(l).unwrap().cleanup);
  }

  #[test]
  fn watchdog_gives_up_after_repeated_attempts() {
    let f = fixture(Some("2.0.0"));
    let l = &f.layout;
    let mut s = read_state(l).unwrap();
    apply_swap(l, &mut s).unwrap();
    assert_eq!(startup_action(l), StartupAction::Continue { trial: true });
    mark_trial_exited(l);
    for _ in 0..MAX_HELPER_ATTEMPTS {
      assert_eq!(startup_action(l), StartupAction::RollBack);
    }
    assert_eq!(startup_action(l), StartupAction::Continue { trial: false });
    assert_eq!(read_state(l).unwrap().phase, Phase::Idle);
  }

  #[test]
  fn state_of_another_install_is_ignored() {
    let f = fixture(Some("2.0.0"));
    let l = &f.layout;
    let mut s = read_state(l).unwrap();
    s.install = "/elsewhere/App".into();
    write_state(l, &s).unwrap();
    assert!(read_state(l).is_none());
    // A garbled state file too.
    std::fs::write(l.state_path(), b"{").unwrap();
    assert!(read_state(l).is_none());
    assert_eq!(startup_action(l), StartupAction::Continue { trial: false });
  }

  #[test]
  fn staged_entry_names_are_plain() {
    let f = fixture(None);
    let l = &f.layout;
    let mut s = UpdateState::new(l);
    for bad in ["../x", "a/b", "..", "", "a\\b"] {
      s.entry = Some(bad.into());
      assert!(s.staged_path(l).is_none(), "{bad}");
    }
  }

  #[test]
  fn confirm_crash_before_cleanup_finishes_at_next_start() {
    let f = fixture(Some("2.0.0"));
    let l = &f.layout;
    let mut s = read_state(l).unwrap();
    apply_swap(l, &mut s).unwrap();
    // Simulate: the state reached idle+cleanup but .old was not deleted.
    let mut s = read_state(l).unwrap();
    s.phase = Phase::Idle;
    s.cleanup = true;
    write_state(l, &s).unwrap();
    assert!(l.old_path().exists());
    assert_eq!(startup_action(l), StartupAction::Continue { trial: false });
    assert!(!l.old_path().exists());
    assert!(!read_state(l).unwrap().cleanup);
  }

  #[test]
  fn bundle_runtime_follows_the_host_search_order() {
    let t = tempfile::tempdir().unwrap();
    let macos = t.path().join("A.app/Contents/MacOS");
    let frameworks = t.path().join("A.app/Contents/Frameworks");
    std::fs::create_dir_all(&macos).unwrap();
    std::fs::create_dir_all(&frameworks).unwrap();
    let exe = macos.join("host");
    assert_eq!(bundle_runtime_path(&exe), None);
    std::fs::write(macos.join("libruntime.dylib"), b"").unwrap();
    assert_eq!(
      bundle_runtime_path(&exe),
      Some(macos.join("libruntime.dylib"))
    );
    std::fs::write(frameworks.join("libruntime.dylib"), b"").unwrap();
    assert_eq!(
      bundle_runtime_path(&exe),
      Some(frameworks.join("libruntime.dylib"))
    );
  }

  #[test]
  fn helper_args() {
    let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
      parse_helper_args(&a(&["x", "run", HELPER_ARG, "apply", "42"])),
      Some((HelperMode::Apply, 42))
    );
    assert_eq!(
      parse_helper_args(&a(&["x", "run", HELPER_ARG, "rollback", "7"])),
      Some((HelperMode::Rollback, 7))
    );
    assert_eq!(
      parse_helper_args(&a(&["x", "run", HELPER_ARG, "rm", "7"])),
      None
    );
    assert_eq!(parse_helper_args(&a(&["x", "run", "main.ts"])), None);
    assert_eq!(
      parse_helper_args(&a(&["x", "run", HELPER_ARG, "apply", "-1"])),
      None
    );
  }

  #[test]
  fn waits_for_an_exited_process() {
    let mut child = if cfg!(windows) {
      std::process::Command::new("cmd")
        .args(["/C", "exit 0"])
        .spawn()
    } else {
      std::process::Command::new("true").spawn()
    }
    .unwrap();
    let pid = child.id();
    child.wait().unwrap();
    assert!(wait_for_exit(pid, Duration::from_secs(5)));
    // Our own pid never exits within the timeout.
    let start = Instant::now();
    assert!(!wait_for_exit(
      std::process::id(),
      Duration::from_millis(300)
    ));
    assert!(start.elapsed() >= Duration::from_millis(250));
  }
}
