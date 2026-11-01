// Copyright 2018-2026 the Deno authors. MIT license.

use deno_core::ModuleSpecifier;
use deno_core::OpState;
use deno_core::op2;
use deno_core::v8;
use deno_error::JsErrorBox;

deno_core::extension!(
  deno_runtime,
  ops = [
    op_main_module,
    op_ppid,
    op_internal_log,
    op_disarm_resource_limits
  ],
  options = { main_module: ModuleSpecifier },
  state = |state, options| {
    state.put::<ModuleSpecifier>(options.main_module);
  },
);

#[op2]
#[string]
fn op_main_module(state: &mut OpState) -> String {
  let main_url = state.borrow::<ModuleSpecifier>();
  main_url.to_string()
}

/// This is an op instead of being done at initialization time because
/// it's expensive to retrieve the ppid on Windows.
#[op2(fast)]
#[number]
pub fn op_ppid() -> i64 {
  #[cfg(windows)]
  {
    // Adopted from rustup:
    // https://github.com/rust-lang/rustup/blob/1.21.1/src/cli/self_update.rs#L1036
    // Copyright Diggory Blake, the Mozilla Corporation, and rustup contributors.
    // Licensed under either of
    // - Apache License, Version 2.0
    // - MIT license
    use std::mem;

    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::CreateToolhelp32Snapshot;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::PROCESSENTRY32;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::Process32First;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::Process32Next;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::TH32CS_SNAPPROCESS;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    // SAFETY: Win32 calls
    unsafe {
      // Take a snapshot of system processes, one of which is ours
      // and contains our parent's pid
      let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
      if snapshot == INVALID_HANDLE_VALUE {
        return -1;
      }

      let mut entry: PROCESSENTRY32 = mem::zeroed();
      entry.dwSize = mem::size_of::<PROCESSENTRY32>() as u32;

      // Iterate over system processes looking for ours
      let success = Process32First(snapshot, &mut entry);
      if success == 0 {
        CloseHandle(snapshot);
        return -1;
      }

      let this_pid = GetCurrentProcessId();
      while entry.th32ProcessID != this_pid {
        let success = Process32Next(snapshot, &mut entry);
        if success == 0 {
          CloseHandle(snapshot);
          return -1;
        }
      }
      CloseHandle(snapshot);

      // FIXME: Using the process ID exposes a race condition
      // wherein the parent process already exited and the OS
      // reassigned its ID.
      let parent_id = entry.th32ParentProcessID;
      parent_id.into()
    }
  }
  #[cfg(not(windows))]
  {
    use std::os::unix::process::parent_id;
    parent_id().into()
  }
}

#[allow(clippy::match_single_binding, reason = "needed for temporary lifetime")]
#[op2(fast)]
fn op_internal_log(
  #[string] url: &str,
  #[smi] level: u32,
  #[string] message: &str,
) {
  let level = match level {
    1 => log::Level::Error,
    2 => log::Level::Warn,
    3 => log::Level::Info,
    4 => log::Level::Debug,
    5 => log::Level::Trace,
    _ => unreachable!(),
  };
  let target = url.replace('/', "::");
  match format_args!("{message}") {
    args => {
      let record = log::Record::builder()
        .file(Some(url))
        .module_path(Some(url))
        .target(&target)
        .level(level)
        .args(args)
        .build();
      log::logger().log(&record);
    }
  }
}

/// What `Deno.disarmResourceLimits(import.meta)` may do in this worker (denext: a
/// compiled program's own main module lifting the `--max-memory` /
/// `--max-cpu-time` / `--max-time` it was compiled with, for a subcommand such
/// as a job or a migration). The embedder puts it in the main worker's
/// `OpState` only; a worker without it (a web worker) is always refused.
pub struct ResourceLimitsDisarm {
  /// Whether resource limits are in force in this process.
  pub armed: bool,
  /// Whether they came from the program's own `deno compile` metadata. Limits
  /// given on the command line (`deno run --max-time=…`) are the operator's
  /// and are never disarmed by the program they limit.
  pub baked: bool,
  /// Whether the main module is evaluating (top-level `await`s included):
  /// false before and after, so nothing that runs once the program is up (a
  /// request handler, a timer, a module imported later) can disarm the limits,
  /// whatever it holds.
  pub main_module_evaluating: bool,
  /// Lifts the limits. Returns `false` when a limit was already reached (the
  /// program is ending).
  pub disarm: fn() -> bool,
}

/// What [`check_disarm`] allows.
#[derive(Debug, PartialEq, Eq)]
enum DisarmCheck {
  /// No limits are in force: nothing to do.
  NothingArmed,
  /// The caller may disarm the limits.
  Allowed,
}

/// The policy of `Deno.disarmResourceLimits(import.meta)`.
/// `holds_main_import_meta` is whether the argument is the main module's own
/// `import.meta` object (`deno_core::is_main_module_import_meta`).
///
/// The capability, not who is on the stack, is what's checked: a dependency has
/// its own `import.meta`, never the main module's, unless the main module gives
/// it away, which is the main module's own decision.
fn check_disarm(
  state: Option<&ResourceLimitsDisarm>,
  holds_main_import_meta: bool,
) -> Result<DisarmCheck, &'static str> {
  let Some(state) = state else {
    return Err(
      "Deno.disarmResourceLimits() can only be called on the main thread, not in a worker",
    );
  };
  if !state.main_module_evaluating {
    return Err(
      "Deno.disarmResourceLimits() can only be called while the main module evaluates (at its top level), not once the program is running",
    );
  }
  // Checked before the limits' state, so code without the capability is
  // refused the same way whether or not limits are in force.
  if !holds_main_import_meta {
    return Err(
      "Deno.disarmResourceLimits() needs the main module's own import.meta: call Deno.disarmResourceLimits(import.meta) from the program's main module",
    );
  }
  if !state.armed {
    return Ok(DisarmCheck::NothingArmed);
  }
  if !state.baked {
    return Err(
      "resource limits given on the command line (--max-memory, --max-cpu-time, --max-time) can't be disarmed by the program; only those baked in by `deno compile` can",
    );
  }
  Ok(DisarmCheck::Allowed)
}

/// `Deno.disarmResourceLimits(import.meta)`.
#[op2(nofast)]
fn op_disarm_resource_limits(
  state: &mut OpState,
  scope: &mut v8::PinScope<'_, '_>,
  import_meta: v8::Local<v8::Value>,
) -> Result<bool, JsErrorBox> {
  let holds_main_import_meta =
    deno_core::is_main_module_import_meta(scope, import_meta);
  match check_disarm(
    state.try_borrow::<ResourceLimitsDisarm>(),
    holds_main_import_meta,
  ) {
    Err(message) => Err(JsErrorBox::new("NotCapable", message)),
    Ok(DisarmCheck::NothingArmed) => Ok(false),
    Ok(DisarmCheck::Allowed) => {
      let state = state.borrow_mut::<ResourceLimitsDisarm>();
      let disarmed = (state.disarm)();
      if disarmed {
        state.armed = false;
      }
      Ok(disarmed)
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn state(
    armed: bool,
    baked: bool,
    main_module_evaluating: bool,
  ) -> ResourceLimitsDisarm {
    ResourceLimitsDisarm {
      armed,
      baked,
      main_module_evaluating,
      disarm: || true,
    }
  }

  #[test]
  fn disarm_policy() {
    // The main module of a compiled program while it evaluates.
    assert_eq!(
      check_disarm(Some(&state(true, true, true)), true),
      Ok(DisarmCheck::Allowed)
    );
    // Nothing armed: no error, nothing to do.
    assert_eq!(
      check_disarm(Some(&state(false, false, true)), true),
      Ok(DisarmCheck::NothingArmed)
    );
    // A web worker (no state), whatever it holds.
    assert!(check_disarm(None, true).is_err());
    // Without the main module's import.meta, limits in force or not.
    assert!(check_disarm(Some(&state(true, true, true)), false).is_err());
    assert!(check_disarm(Some(&state(false, true, true)), false).is_err());
    // Before or after the main module's evaluation, even with it.
    assert!(check_disarm(Some(&state(true, true, false)), true).is_err());
    // Limits from the command line.
    assert!(check_disarm(Some(&state(true, false, true)), true).is_err());
  }
}
