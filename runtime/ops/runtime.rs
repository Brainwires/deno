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

/// What `Deno.disarmResourceLimits()` may do in this worker (denext: a
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
  /// The V8 script id of the main module's source while the main module
  /// evaluates (top-level `await`s included); `None` before and after, so
  /// nothing that runs once the program is up (a request handler, a timer, a
  /// module imported later) can disarm the limits.
  pub main_module_script_id: Option<i32>,
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

/// The policy of `Deno.disarmResourceLimits()`. `caller_script_id` is the
/// script id of the innermost JavaScript frame, the code that called the op
/// (the op is installed on `Deno` as is, so no wrapper frame sits between).
fn check_disarm(
  state: Option<&ResourceLimitsDisarm>,
  caller_script_id: Option<usize>,
) -> Result<DisarmCheck, &'static str> {
  let Some(state) = state else {
    return Err(
      "Deno.disarmResourceLimits() can only be called on the main thread, not in a worker",
    );
  };
  // The caller is checked before anything else, so code that isn't the main
  // module is refused the same way whether or not limits are in force.
  if let Some(main) = state.main_module_script_id {
    let is_main = caller_script_id
      .is_some_and(|id| i64::try_from(id).ok() == Some(i64::from(main)));
    if !is_main {
      return Err(
        "Deno.disarmResourceLimits() can only be called by the program's main module itself, not by a module it imports or by evaluated code",
      );
    }
  } else {
    return Err(
      "Deno.disarmResourceLimits() can only be called while the main module evaluates (at its top level), not once the program is running",
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

/// `Deno.disarmResourceLimits()`. Installed on `Deno` directly, not behind a
/// JavaScript wrapper, so the innermost frame of the current stack is the
/// caller's. Never a fast call: it walks the stack, which allocates handles.
#[op2(nofast)]
fn op_disarm_resource_limits(
  state: &mut OpState,
  scope: &mut v8::PinScope<'_, '_>,
) -> Result<bool, JsErrorBox> {
  let caller_script_id = v8::StackTrace::current_stack_trace(scope, 1)
    .and_then(|trace| trace.get_frame(scope, 0))
    .map(|frame| frame.get_script_id());
  match check_disarm(
    state.try_borrow::<ResourceLimitsDisarm>(),
    caller_script_id,
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
    main_module_script_id: Option<i32>,
  ) -> ResourceLimitsDisarm {
    ResourceLimitsDisarm {
      armed,
      baked,
      main_module_script_id,
      disarm: || true,
    }
  }

  #[test]
  fn disarm_policy() {
    // The main module of a compiled program while it evaluates.
    assert_eq!(
      check_disarm(Some(&state(true, true, Some(7))), Some(7)),
      Ok(DisarmCheck::Allowed)
    );
    // Nothing armed: no error, nothing to do.
    assert_eq!(
      check_disarm(Some(&state(false, false, Some(7))), Some(7)),
      Ok(DisarmCheck::NothingArmed)
    );
    // A web worker (no state), whatever the caller.
    assert!(check_disarm(None, Some(7)).is_err());
    // Another script: a dependency, eval, node:vm, a later import.
    assert!(check_disarm(Some(&state(true, true, Some(7))), Some(8)).is_err());
    assert!(check_disarm(Some(&state(false, true, Some(7))), Some(8)).is_err());
    // No JavaScript frame at all.
    assert!(check_disarm(Some(&state(true, true, Some(7))), None).is_err());
    // Before or after the main module's evaluation.
    assert!(check_disarm(Some(&state(true, true, None)), Some(7)).is_err());
    // Limits from the command line.
    assert!(check_disarm(Some(&state(true, false, Some(7))), Some(7)).is_err());
  }
}
