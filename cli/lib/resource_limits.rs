// Copyright 2018-2026 the Deno authors. MIT license.

//! `--max-memory`, `--max-cpu-time` and `--max-time`: process resource limits
//! for `deno run` and for a program compiled with them (`deno compile` stores
//! them in the binary's metadata and `denort` applies them).
//!
//! Each limit ends the process with its own exit code and a one-line message
//! on stderr:
//!
//! | Flag             | Exit code | Message                                     |
//! | ---------------- | --------- | ------------------------------------------- |
//! | `--max-memory`   | 137       | `Memory limit exceeded (--max-memory)`      |
//! | `--max-cpu-time` | 152       | `CPU time limit exceeded (--max-cpu-time)`  |
//! | `--max-time`     | 124       | `Time limit exceeded (--max-time)`          |
//!
//! (124 is what GNU `timeout` exits with, 137 is how a shell reports a process
//! the kernel's OOM killer ended, 128 + SIGKILL, and 152 is 128 + SIGXCPU,
//! the signal `RLIMIT_CPU` sends, so supervisors that already understand
//! those codes read these the same way.)
//!
//! How each is enforced:
//!
//! - `--max-memory` caps the main isolate's V8 heap (young and old generation
//!   together, `ResourceConstraints::ConfigureDefaultsFromHeapSize`). When the
//!   heap reaches it, V8's near-heap-limit callback records the limit and
//!   terminates execution, and raises the limit so the termination can unwind
//!   instead of V8 aborting the process. Worker isolates have their own heaps
//!   (`new Worker(url, { deno: { memoryMb } })`), and memory outside the heap
//!   (`ArrayBuffer` backing stores, native code) is not counted.
//! - `--max-cpu-time` is a budget of process CPU time (user + system, every
//!   thread: `getrusage(RUSAGE_SELF)` on Unix, `GetProcessTimes` on Windows),
//!   counted from when the main worker is created; a watchdog thread samples
//!   it every [`WATCHDOG_TICK`] and terminates execution when it is used up.
//! - `--max-time` is a wall-clock deadline from the same moment, idle time
//!   included, enforced by the same watchdog.
//!
//! Termination unwinds a running program, and the CLI's (or `denort`'s) exit
//! path then reports the limit ([`exit_if_exceeded`]). A program that isn't
//! running JavaScript when a limit is reached (waiting on a timer that never
//! fires, blocked in a native call) can't unwind, so the watchdog ends the
//! process itself [`WATCHDOG_GRACE`] after the limit. Exactly one of the two
//! prints the message ([`REPORTED`]). Once a limit is recorded, it decides the
//! exit code even if the program then ends on its own.
//!
//! These are guardrails against runaway programs, not a sandbox: subprocesses
//! are neither counted nor stopped, the sampling can overshoot by up to a
//! tick, and native code can allocate outside the heap. The limits are
//! process-wide and meant for one program, so only `deno run` and
//! `deno compile` take them, and not together with `--watch`.

use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use deno_runtime::colors;
use deno_runtime::deno_core::JsRuntime;
use deno_runtime::deno_core::v8;

/// Exit code of a program that reached `--max-memory`.
pub const MEMORY_LIMIT_EXIT_CODE: i32 = 137;
/// Exit code of a program that used up `--max-cpu-time`.
pub const CPU_TIME_LIMIT_EXIT_CODE: i32 = 152;
/// Exit code of a program that reached `--max-time`.
pub const TIME_LIMIT_EXIT_CODE: i32 = 124;

/// How often the watchdog samples the CPU time and the clock.
const WATCHDOG_TICK: Duration = Duration::from_millis(20);

/// How long the watchdog lets a terminated program unwind and report the
/// limit itself before ending the process.
const WATCHDOG_GRACE: Duration = Duration::from_secs(2);

/// The limits given to `deno run` / `deno compile`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResourceLimits {
  /// `--max-memory`: the main isolate's heap limit, in bytes.
  pub max_memory_bytes: Option<u64>,
  /// `--max-cpu-time`, in seconds.
  pub max_cpu_time_secs: Option<u64>,
  /// `--max-time`, in seconds.
  pub max_time_secs: Option<u64>,
}

impl ResourceLimits {
  pub fn any(&self) -> bool {
    self.max_memory_bytes.is_some()
      || self.max_cpu_time_secs.is_some()
      || self.max_time_secs.is_some()
  }

  /// Applies `--max-memory` to the main isolate's create params.
  pub fn apply_heap_limit(
    &self,
    params: Option<v8::CreateParams>,
  ) -> Option<v8::CreateParams> {
    match self.max_memory_bytes {
      Some(bytes) => {
        let bytes = usize::try_from(bytes).unwrap_or(usize::MAX);
        Some(params.unwrap_or_default().heap_limits(0, bytes))
      }
      None => params,
    }
  }
}

/// Which limit ended the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LimitKind {
  Memory = 1,
  CpuTime = 2,
  Time = 3,
}

impl LimitKind {
  fn from_u8(value: u8) -> Option<Self> {
    match value {
      1 => Some(Self::Memory),
      2 => Some(Self::CpuTime),
      3 => Some(Self::Time),
      _ => None,
    }
  }

  pub fn exit_code(self) -> i32 {
    match self {
      Self::Memory => MEMORY_LIMIT_EXIT_CODE,
      Self::CpuTime => CPU_TIME_LIMIT_EXIT_CODE,
      Self::Time => TIME_LIMIT_EXIT_CODE,
    }
  }

  pub fn message(self) -> &'static str {
    match self {
      Self::Memory => "Memory limit exceeded (--max-memory)",
      Self::CpuTime => "CPU time limit exceeded (--max-cpu-time)",
      Self::Time => "Time limit exceeded (--max-time)",
    }
  }
}

/// The first limit reached (a [`LimitKind`] as `u8`, 0 for none). Set once
/// and never cleared: one run per process.
static EXCEEDED: AtomicU8 = AtomicU8::new(0);

/// Set by whichever of the exit path and the watchdog prints the message.
static REPORTED: AtomicBool = AtomicBool::new(false);

/// The main isolate the watchdog terminates.
static ACTIVE_ISOLATE: parking_lot::Mutex<Option<v8::IsolateHandle>> =
  parking_lot::Mutex::new(None);

/// The limits the watchdog was started with (it starts once per process).
static WATCHDOG_LIMITS: OnceLock<ResourceLimits> = OnceLock::new();

/// The limit the program reached, if any.
pub fn exceeded() -> Option<LimitKind> {
  LimitKind::from_u8(EXCEEDED.load(Ordering::SeqCst))
}

/// Records `kind` unless a limit is already recorded, and terminates the main
/// isolate. Returns whether this call recorded it.
fn trip(kind: LimitKind) -> bool {
  let recorded = EXCEEDED
    .compare_exchange(0, kind as u8, Ordering::SeqCst, Ordering::SeqCst)
    .is_ok();
  if recorded && let Some(handle) = ACTIVE_ISOLATE.lock().as_ref() {
    handle.terminate_execution();
  }
  recorded
}

fn claim_report() -> bool {
  REPORTED
    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
    .is_ok()
}

fn print_message(kind: LimitKind) {
  log::error!("{}: {}", colors::red_bold("error"), kind.message());
}

/// Ends the process with the limit's exit code if a limit was reached,
/// printing its message unless the watchdog already is. Call it on every exit
/// path of the main thread, before the program's own exit code is used: the
/// error a terminated isolate surfaces is only "execution terminated".
pub fn exit_if_exceeded() {
  let Some(kind) = exceeded() else {
    return;
  };
  if claim_report() {
    print_message(kind);
    deno_runtime::exit(kind.exit_code());
  }
  // The watchdog is reporting and ending the process; leave it to it rather
  // than run the exit handlers on two threads at once.
  loop {
    std::thread::park();
  }
}

/// Arms the limits for the main worker `runtime`: the near-heap-limit
/// callback for `--max-memory` (the heap cap itself is set at isolate creation
/// by [`ResourceLimits::apply_heap_limit`]) and the watchdog thread. A no-op
/// when no limit is set.
pub fn arm(limits: ResourceLimits, runtime: &mut JsRuntime) {
  if !limits.any() {
    return;
  }
  if limits.max_cpu_time_secs.is_some()
    && !deno_runtime::deno_os::PROCESS_CPU_TIME_SUPPORTED
  {
    // Refuse rather than accept a budget that would never run out.
    log::error!(
      "{}: --max-cpu-time is not supported on this platform",
      colors::red_bold("error")
    );
    deno_runtime::exit(1);
  }
  let handle = runtime.v8_isolate().thread_safe_handle();
  *ACTIVE_ISOLATE.lock() = Some(handle);

  if limits.max_memory_bytes.is_some() {
    runtime.add_near_heap_limit_callback(|current_limit, _initial_limit| {
      trip(LimitKind::Memory);
      // Room to unwind: without it V8 aborts the process once the callback
      // returns the same limit.
      current_limit.saturating_mul(2)
    });
  }

  let first = WATCHDOG_LIMITS.set(limits).is_ok();
  // `deno run` and a compiled program create one main worker; a second one
  // would only move the watchdog to its isolate (above).
  debug_assert!(first || WATCHDOG_LIMITS.get() == Some(&limits));
  if first {
    start_watchdog(limits);
  }
}

fn start_watchdog(limits: ResourceLimits) {
  let started = Instant::now();
  let deadline = limits
    .max_time_secs
    .map(|secs| started + Duration::from_secs(secs));
  let cpu_budget = limits
    .max_cpu_time_secs
    .map(|secs| (cpu_time_now(), Duration::from_secs(secs)));
  std::thread::Builder::new()
    .name("deno-resource-limits".to_string())
    .spawn(move || watchdog(deadline, cpu_budget))
    .expect("failed to start the resource limit watchdog thread");
}

fn cpu_time_now() -> Duration {
  deno_runtime::deno_os::process_cpu_time().unwrap_or_default()
}

fn watchdog(
  deadline: Option<Instant>,
  cpu_budget: Option<(Duration, Duration)>,
) {
  let mut reached_at: Option<Instant> = None;
  loop {
    std::thread::sleep(WATCHDOG_TICK);
    let now = Instant::now();
    if let Some(reached_at) = reached_at {
      if now.duration_since(reached_at) >= WATCHDOG_GRACE {
        // The program didn't unwind and exit by itself.
        if claim_report()
          && let Some(kind) = exceeded()
        {
          print_message(kind);
          deno_runtime::exit(kind.exit_code());
        }
        // The exit path is reporting; it ends the process.
        return;
      }
      continue;
    }
    if exceeded().is_some() {
      // `--max-memory`, recorded by the near-heap-limit callback.
      reached_at = Some(now);
    } else if deadline.is_some_and(|deadline| now >= deadline) {
      trip(LimitKind::Time);
      reached_at = Some(now);
    } else if let Some((baseline, budget)) = cpu_budget
      && let Some(used) = deno_runtime::deno_os::process_cpu_time()
      && used.saturating_sub(baseline) >= budget
    {
      trip(LimitKind::CpuTime);
      reached_at = Some(now);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn exit_codes_and_messages() {
    assert_eq!(LimitKind::Memory.exit_code(), 137);
    assert_eq!(LimitKind::CpuTime.exit_code(), 152);
    assert_eq!(LimitKind::Time.exit_code(), 124);
    for kind in [LimitKind::Memory, LimitKind::CpuTime, LimitKind::Time] {
      assert_eq!(LimitKind::from_u8(kind as u8), Some(kind));
    }
    assert_eq!(LimitKind::from_u8(0), None);
    assert_eq!(
      LimitKind::CpuTime.message(),
      "CPU time limit exceeded (--max-cpu-time)"
    );
  }

  #[test]
  fn heap_limit_only_with_max_memory() {
    let none = ResourceLimits {
      max_time_secs: Some(1),
      ..Default::default()
    };
    assert!(none.any());
    assert!(none.apply_heap_limit(None).is_none());
    let limits = ResourceLimits {
      max_memory_bytes: Some(64 * 1024 * 1024),
      ..Default::default()
    };
    let params = limits.apply_heap_limit(None).unwrap();
    // Young and old generation together stay within the limit.
    assert!(params.max_old_generation_size_in_bytes() <= 64 * 1024 * 1024);
    assert!(params.max_old_generation_size_in_bytes() > 0);
    assert!(!ResourceLimits::default().any());
  }
}
