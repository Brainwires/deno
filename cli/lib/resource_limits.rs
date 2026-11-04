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
//!   it and terminates execution when it is used up. It samples no more often
//!   than it has to: the budget left can't run out sooner than that budget
//!   spread over every CPU the process may use, so it sleeps for half of that
//!   (at least [`WATCHDOG_TICK`], at most [`CPU_POLL_MAX`]).
//! - `--max-time` is a wall-clock deadline from the same moment, idle time
//!   included, enforced by the same watchdog, which sleeps until it.
//!
//! The watchdog has nothing to poll for `--max-memory` (the near-heap-limit
//! callback wakes it), so an idle program limited by memory alone, or by a
//! deadline, never wakes it before something happens: an idle server with
//! limits stays asleep (it woke every [`WATCHDOG_TICK`] before).
//!
//! Termination unwinds a running program, and the CLI's (or `denort`'s) exit
//! path then reports the limit ([`exit_if_exceeded`]). A program that isn't
//! running JavaScript when a limit is reached (waiting on a timer that never
//! fires, blocked in a native call) can't unwind, so the watchdog ends the
//! process itself [`WATCHDOG_GRACE`] after the limit. Exactly one of the two
//! prints the message ([`REPORTED`]). Once a limit is recorded, it decides the
//! exit code even if the program then ends on its own.
//!
//! A compiled program's own main module can lift the limits it was compiled
//! with, for a subcommand that is not the long-running program they are meant
//! for (a job, a migration): `Deno.disarmResourceLimits(import.meta)`,
//! refused without the main module's own `import.meta`, after the main module's
//! evaluation, and for limits given on the command line (`deno_runtime::ops::runtime::ResourceLimitsDisarm`). See
//! [`disarm`].
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
use parking_lot::Condvar;
use parking_lot::Mutex;

/// Exit code of a program that reached `--max-memory`.
pub const MEMORY_LIMIT_EXIT_CODE: i32 = 137;
/// Exit code of a program that used up `--max-cpu-time`.
pub const CPU_TIME_LIMIT_EXIT_CODE: i32 = 152;
/// Exit code of a program that reached `--max-time`.
pub const TIME_LIMIT_EXIT_CODE: i32 = 124;

/// The watchdog's shortest sleep: how often it samples a CPU budget that is
/// nearly used up, and how often it looks during [`WATCHDOG_GRACE`].
const WATCHDOG_TICK: Duration = Duration::from_millis(20);

/// The watchdog's longest sleep while a CPU budget is set, however much of it
/// is left.
const CPU_POLL_MAX: Duration = Duration::from_secs(10);

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

/// The first limit reached (a [`LimitKind`] as `u8`, 0 for none), or
/// [`DISARMED`]. Set once and never cleared: one run per process. A limit
/// reached and a disarm race for it, so exactly one of them wins.
static EXCEEDED: AtomicU8 = AtomicU8::new(0);

/// [`EXCEEDED`]'s value once the limits are disarmed: no limit is recorded
/// after it.
const DISARMED: u8 = u8::MAX;

/// The heap limit (old generation, in bytes) the main isolate would have had
/// without `--max-memory`: how far a disarmed program's heap may grow.
static DEFAULT_HEAP_LIMIT: OnceLock<Option<usize>> = OnceLock::new();

/// Set by whichever of the exit path and the watchdog prints the message.
static REPORTED: AtomicBool = AtomicBool::new(false);

/// The main isolate the watchdog terminates.
static ACTIVE_ISOLATE: parking_lot::Mutex<Option<v8::IsolateHandle>> =
  parking_lot::Mutex::new(None);

/// The limits the watchdog was started with (it starts once per process).
static WATCHDOG_LIMITS: OnceLock<ResourceLimits> = OnceLock::new();

/// Whether the watchdog should look now rather than at the end of its sleep
/// (a limit recorded outside it, or a disarm); cleared when it wakes.
static WATCHDOG_WAKE: Mutex<bool> = Mutex::new(false);
static WATCHDOG_WAKE_SIGNAL: Condvar = Condvar::new();

/// Wakes the watchdog. Safe from V8's near-heap-limit callback: the lock is
/// only ever held for a flag update.
fn wake_watchdog() {
  *WATCHDOG_WAKE.lock() = true;
  WATCHDOG_WAKE_SIGNAL.notify_all();
}

/// Sleeps for `timeout` (`None`: until woken) or until [`wake_watchdog`],
/// whichever comes first. A wake that came before the sleep is not lost.
fn wait_for_watchdog(timeout: Option<Duration>) {
  let mut woken = WATCHDOG_WAKE.lock();
  if !*woken {
    match timeout {
      Some(timeout) => {
        WATCHDOG_WAKE_SIGNAL.wait_for(&mut woken, timeout);
      }
      None => WATCHDOG_WAKE_SIGNAL.wait(&mut woken),
    }
  }
  *woken = false;
}

/// How long the watchdog may sleep before it must look again; `None` when
/// nothing but a wake can need it (`--max-memory` alone).
///
/// - after a limit is reached, [`WATCHDOG_TICK`] (the grace period);
/// - with `--max-time`, until the deadline;
/// - with `--max-cpu-time`, half of what the `cpu_remaining` budget lasts at
///   full use of `parallelism` CPUs, within [`WATCHDOG_TICK`] and
///   [`CPU_POLL_MAX`].
fn next_wait(
  now: Instant,
  deadline: Option<Instant>,
  cpu_remaining: Option<Duration>,
  parallelism: u32,
  reached: bool,
) -> Option<Duration> {
  if reached {
    return Some(WATCHDOG_TICK);
  }
  let until_deadline =
    deadline.map(|deadline| deadline.saturating_duration_since(now));
  let cpu_wait = cpu_remaining.map(|remaining| {
    (remaining / parallelism.max(1).saturating_mul(2))
      .clamp(WATCHDOG_TICK, CPU_POLL_MAX)
  });
  match (until_deadline, cpu_wait) {
    (Some(a), Some(b)) => Some(a.min(b)),
    (a, b) => a.or(b),
  }
}

/// The limit the program reached, if any.
pub fn exceeded() -> Option<LimitKind> {
  LimitKind::from_u8(EXCEEDED.load(Ordering::SeqCst))
}

/// Records `kind` in `cell` unless a limit is already recorded or the limits
/// are disarmed. Returns whether this call recorded it.
fn record(cell: &AtomicU8, kind: LimitKind) -> bool {
  cell
    .compare_exchange(0, kind as u8, Ordering::SeqCst, Ordering::SeqCst)
    .is_ok()
}

/// Marks the limits in `cell` disarmed unless a limit was already reached.
/// Returns whether they are disarmed (also when they already were).
fn record_disarm(cell: &AtomicU8) -> bool {
  match cell.compare_exchange(0, DISARMED, Ordering::SeqCst, Ordering::SeqCst) {
    Ok(_) => true,
    Err(current) => current == DISARMED,
  }
}

fn disarmed() -> bool {
  EXCEEDED.load(Ordering::SeqCst) == DISARMED
}

/// Records `kind` unless a limit is already recorded (or the limits are
/// disarmed), and terminates the main isolate. Returns whether this call
/// recorded it.
fn trip(kind: LimitKind) -> bool {
  let recorded = record(&EXCEEDED, kind);
  if recorded {
    if let Some(handle) = ACTIVE_ISOLATE.lock().as_ref() {
      handle.terminate_execution();
    }
    // The watchdog sleeps until woken when only `--max-memory` is set; it
    // ends the process if the program doesn't unwind.
    wake_watchdog();
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

/// Lifts the limits armed by [`arm`]: the watchdog thread stops without
/// ending anything, and the main isolate's heap may grow to
/// `default_heap_limit` (what it would have had without `--max-memory`, the
/// cap set at creation can only be raised from the near-heap-limit callback).
/// Returns `false`, and changes nothing, when a limit was already reached.
///
/// This is what `Deno.disarmResourceLimits(import.meta)` calls, after
/// `deno_runtime::ops::runtime` has checked that the caller is the main
/// module of a compiled program, evaluating; nothing else calls it.
pub fn disarm() -> bool {
  let disarmed = record_disarm(&EXCEEDED);
  if disarmed {
    // Let the watchdog thread see it and end.
    wake_watchdog();
  }
  disarmed
}

/// The near-heap-limit callback's new limit. Armed, reaching the limit
/// records it (and the program unwinds); disarmed, the heap grows as it would
/// have without `--max-memory`, up to `default_heap_limit`, past which V8
/// ends the process as it ends any program out of heap.
fn next_heap_limit(
  current_limit: usize,
  default_heap_limit: Option<usize>,
  disarmed: bool,
) -> usize {
  if !disarmed {
    // Room to unwind: without it V8 aborts the process once the callback
    // returns the same limit.
    return current_limit.saturating_mul(2);
  }
  match default_heap_limit {
    Some(ceiling) if current_limit < ceiling => {
      current_limit.saturating_mul(2).min(ceiling)
    }
    _ => current_limit,
  }
}

/// Arms the limits for the main worker `runtime`: the near-heap-limit
/// callback for `--max-memory` (the heap cap itself is set at isolate creation
/// by [`ResourceLimits::apply_heap_limit`]) and the watchdog thread. A no-op
/// when no limit is set. `default_heap_limit` is the heap limit (old
/// generation, bytes) the isolate would have had without `--max-memory`, for
/// [`disarm`].
pub fn arm(
  limits: ResourceLimits,
  runtime: &mut JsRuntime,
  default_heap_limit: Option<usize>,
) {
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
    let _ = DEFAULT_HEAP_LIMIT.set(default_heap_limit);
    runtime.add_near_heap_limit_callback(|current_limit, _initial_limit| {
      trip(LimitKind::Memory);
      next_heap_limit(
        current_limit,
        DEFAULT_HEAP_LIMIT.get().copied().flatten(),
        disarmed(),
      )
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
  // CPU time grows at most this fast: one second a second per CPU the
  // process may run on.
  let parallelism = std::thread::available_parallelism()
    .map(|n| u32::try_from(n.get()).unwrap_or(u32::MAX))
    .unwrap_or(1);
  let mut reached_at: Option<Instant> = None;
  loop {
    let cpu_remaining = cpu_budget.map(|(baseline, budget)| {
      budget.saturating_sub(cpu_time_now().saturating_sub(baseline))
    });
    wait_for_watchdog(next_wait(
      Instant::now(),
      deadline,
      cpu_remaining,
      parallelism,
      reached_at.is_some(),
    ));
    if disarmed() {
      // `Deno.disarmResourceLimits()`: nothing is ended from here on.
      return;
    }
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
  fn a_limit_and_a_disarm_race_once() {
    // Disarmed first: no limit is recorded afterwards.
    let cell = AtomicU8::new(0);
    assert!(record_disarm(&cell));
    assert!(!record(&cell, LimitKind::Time));
    assert!(record_disarm(&cell));
    assert_eq!(LimitKind::from_u8(cell.load(Ordering::SeqCst)), None);
    // A limit reached first: too late to disarm.
    let cell = AtomicU8::new(0);
    assert!(record(&cell, LimitKind::CpuTime));
    assert!(!record_disarm(&cell));
    assert!(!record(&cell, LimitKind::Memory));
    assert_eq!(
      LimitKind::from_u8(cell.load(Ordering::SeqCst)),
      Some(LimitKind::CpuTime)
    );
  }

  #[test]
  fn disarmed_heap_grows_to_the_default_limit() {
    const MB: usize = 1024 * 1024;
    // Armed: room to unwind.
    assert_eq!(next_heap_limit(64 * MB, Some(4096 * MB), false), 128 * MB);
    // Disarmed: doubling up to the limit the isolate would have had.
    assert_eq!(next_heap_limit(64 * MB, Some(4096 * MB), true), 128 * MB);
    assert_eq!(next_heap_limit(3000 * MB, Some(4096 * MB), true), 4096 * MB);
    // At it (or with none known), no more: V8 ends the program as it ends
    // any program out of heap.
    assert_eq!(next_heap_limit(4096 * MB, Some(4096 * MB), true), 4096 * MB);
    assert_eq!(next_heap_limit(64 * MB, None, true), 64 * MB);
  }

  #[test]
  fn the_watchdog_sleeps_until_it_has_something_to_check() {
    let now = Instant::now();
    // `--max-memory` alone: nothing to poll, the callback wakes it.
    assert_eq!(next_wait(now, None, None, 8, false), None);
    // A limit reached: the grace period's ticks.
    assert_eq!(next_wait(now, None, None, 8, true), Some(WATCHDOG_TICK));
    // `--max-time`: until the deadline, and not past it.
    let deadline = now + Duration::from_secs(3600);
    assert_eq!(
      next_wait(now, Some(deadline), None, 8, false),
      Some(Duration::from_secs(3600))
    );
    assert_eq!(
      next_wait(
        deadline + Duration::from_secs(1),
        Some(deadline),
        None,
        8,
        false
      ),
      Some(Duration::ZERO)
    );
  }

  #[test]
  fn a_cpu_budget_is_sampled_before_it_can_run_out() {
    let now = Instant::now();
    // 8 CPUs can use 8 s of budget in 1 s: half of that.
    assert_eq!(
      next_wait(now, None, Some(Duration::from_secs(8)), 8, false),
      Some(Duration::from_millis(500))
    );
    // A large budget: at most `CPU_POLL_MAX` between samples.
    assert_eq!(
      next_wait(now, None, Some(Duration::from_secs(3600)), 8, false),
      Some(CPU_POLL_MAX)
    );
    // Nearly used up (or used up): every tick, as before.
    assert_eq!(
      next_wait(now, None, Some(Duration::from_millis(10)), 8, false),
      Some(WATCHDOG_TICK)
    );
    assert_eq!(
      next_wait(now, None, Some(Duration::ZERO), 0, false),
      Some(WATCHDOG_TICK)
    );
    // With a deadline too, whichever comes first.
    assert_eq!(
      next_wait(
        now,
        Some(now + Duration::from_millis(200)),
        Some(Duration::from_secs(8)),
        8,
        false
      ),
      Some(Duration::from_millis(200))
    );
  }

  #[test]
  fn a_wake_is_not_lost() {
    // A wake before the sleep: the sleep returns at once (an hour would
    // time the test out).
    wake_watchdog();
    let started = Instant::now();
    wait_for_watchdog(Some(Duration::from_secs(3600)));
    assert!(started.elapsed() < Duration::from_secs(60));
    // A wake during an unbounded sleep ends it.
    let waiter = std::thread::spawn(|| wait_for_watchdog(None));
    while !waiter.is_finished() {
      wake_watchdog();
      std::thread::sleep(Duration::from_millis(5));
    }
    waiter.join().unwrap();
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
