// Copyright 2018-2026 the Deno authors. MIT license.

//! denext (DENEXT.md item 104): `--unstable-idle-trim`.
//!
//! A long-running server's resident memory after a load burst is dominated by
//! memory glibc's allocator keeps in its arenas after the JavaScript heap has
//! let go of it. With this feature on, once the main event loop has not been
//! woken for [`IdleTrimConfig::delay`] after some activity, the runtime runs a
//! full V8 GC (`low_memory_notification`) and then `malloc_trim(0)`, which
//! hands the freed pages back to the OS.
//!
//! - It runs only between event loop polls, after the loop has gone a whole
//!   `delay` without being woken (no timer fired, no I/O, no op completed), so
//!   it never runs while JavaScript, a request handler included, is running.
//!   Work that arrives during a trim waits for it to finish.
//! - At most once per idle period (it re-arms only after the loop is woken
//!   again) and never twice within [`IdleTrimConfig::min_interval`]. A check
//!   the growth gate skips runs no GC and does not count toward the interval.
//! - glibc Linux only. On every other target (macOS, Windows, musl) enabling
//!   it does nothing.
//! - The full GC's cost grows with the live heap. Once a trim's GC has left
//!   the heap at [`IdleTrimConfig::max_gc_heap`] or more, later trims skip the
//!   GC (and only `malloc_trim`) while the heap stays that large, so a process
//!   that holds a large heap on purpose (an in-memory cache) doesn't pay a
//!   long GC in front of the next request every `min_interval`.

use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;
use std::task::Wake;
use std::task::Waker;
use std::time::Duration;
use std::time::Instant;

/// How long the event loop must go without a wake-up before a trim.
pub const DEFAULT_IDLE_DELAY: Duration = Duration::from_millis(1000);
/// The minimum spacing between two trims.
pub const DEFAULT_MIN_INTERVAL: Duration = Duration::from_secs(10);
/// How much the RSS must have grown since the last trim for another one.
pub const DEFAULT_MIN_GROWTH_BYTES: u64 = 16 * 1024 * 1024;
/// The V8 heap size (after a trim's GC) from which later trims skip the GC.
pub const DEFAULT_MAX_GC_HEAP_BYTES: u64 = 1024 * 1024 * 1024;

/// Whether a trim can do anything on this target (glibc Linux).
pub const SUPPORTED: bool = cfg!(all(target_os = "linux", target_env = "gnu"));

/// Tunables for the idle trim, read from the environment by
/// [`IdleTrimConfig::from_env`]:
///
/// - `DENO_IDLE_TRIM_DELAY_MS`: [`IdleTrimConfig::delay`] (default 1000).
/// - `DENO_IDLE_TRIM_MIN_INTERVAL_MS`: [`IdleTrimConfig::min_interval`]
///   (default 10000).
/// - `DENO_IDLE_TRIM_MIN_GROWTH_MB`: [`IdleTrimConfig::min_growth`] in MiB
///   (default 16).
/// - `DENO_IDLE_TRIM_MAX_GC_HEAP_MB`: [`IdleTrimConfig::max_gc_heap`] in MiB
///   (default 1024; `0` never skips the GC).
/// - `DENO_IDLE_TRIM_TRACE=1`: print one line per trim to stderr (GC and
///   `malloc_trim` durations, RSS before and after).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdleTrimConfig {
  pub delay: Duration,
  pub min_interval: Duration,
  /// After the first trim, a due trim is skipped (and the idle period counted
  /// as handled) unless the RSS has grown by this much since the last trim
  /// left it: a server woken by its own periodic timers would otherwise run a
  /// full GC every `min_interval` for nothing.
  pub min_growth: u64,
  /// A trim skips the full GC (and only runs `malloc_trim`) while the V8 heap
  /// is at least this large, both as the last trim's GC left it and now: the
  /// GC's cost grows with the live heap, and a heap that survived a full GC
  /// at this size is mostly live data the GC can't free. `None`: never skip.
  pub max_gc_heap: Option<u64>,
  pub trace: bool,
}

impl Default for IdleTrimConfig {
  fn default() -> Self {
    Self {
      delay: DEFAULT_IDLE_DELAY,
      min_interval: DEFAULT_MIN_INTERVAL,
      min_growth: DEFAULT_MIN_GROWTH_BYTES,
      max_gc_heap: Some(DEFAULT_MAX_GC_HEAP_BYTES),
      trace: false,
    }
  }
}

impl IdleTrimConfig {
  pub fn from_env() -> Self {
    Self::from_lookup(|name| std::env::var(name).ok())
  }

  fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
    let number =
      |name: &str| get(name).and_then(|value| value.trim().parse::<u64>().ok());
    let millis = |name: &str, default: Duration| {
      number(name).map(Duration::from_millis).unwrap_or(default)
    };
    Self {
      delay: millis("DENO_IDLE_TRIM_DELAY_MS", DEFAULT_IDLE_DELAY),
      min_interval: millis(
        "DENO_IDLE_TRIM_MIN_INTERVAL_MS",
        DEFAULT_MIN_INTERVAL,
      ),
      min_growth: number("DENO_IDLE_TRIM_MIN_GROWTH_MB")
        .map(|mb| mb.saturating_mul(1024 * 1024))
        .unwrap_or(DEFAULT_MIN_GROWTH_BYTES),
      max_gc_heap: match number("DENO_IDLE_TRIM_MAX_GC_HEAP_MB") {
        Some(0) => None,
        Some(mb) => Some(mb.saturating_mul(1024 * 1024)),
        None => Some(DEFAULT_MAX_GC_HEAP_BYTES),
      },
      trace: matches!(get("DENO_IDLE_TRIM_TRACE").as_deref(), Some("1")),
    }
  }
}

/// The clock-free scheduling rule: when the next trim is due, given the last
/// wake-up of the event loop and the last trim.
#[derive(Debug)]
struct IdleTrimScheduler {
  delay: Duration,
  min_interval: Duration,
  last_activity: Instant,
  last_trim: Option<Instant>,
  /// Set by activity, cleared by a trim: one trim per idle period.
  armed: bool,
}

impl IdleTrimScheduler {
  fn new(config: &IdleTrimConfig, now: Instant) -> Self {
    Self {
      delay: config.delay,
      min_interval: config.min_interval,
      last_activity: now,
      last_trim: None,
      armed: true,
    }
  }

  fn on_activity(&mut self, now: Instant) {
    self.last_activity = now;
    self.armed = true;
  }

  /// When the next trim is due, or `None` when this idle period was already
  /// trimmed.
  fn deadline(&self) -> Option<Instant> {
    if !self.armed {
      return None;
    }
    let idle_at = self.last_activity + self.delay;
    Some(match self.last_trim {
      Some(last) => idle_at.max(last + self.min_interval),
      None => idle_at,
    })
  }

  /// Claims this idle period's check if it is due at `now` (disarming until
  /// the next wake-up). Only [`Self::record_trim`] counts toward
  /// `min_interval`: a check that finds nothing to do runs no GC, so it must
  /// not push back the trim that follows the next burst of load.
  fn take_due(&mut self, now: Instant) -> bool {
    match self.deadline() {
      Some(deadline) if now >= deadline => {
        self.armed = false;
        true
      }
      _ => false,
    }
  }

  /// Records a trim that ran at `now`.
  fn record_trim(&mut self, now: Instant) {
    self.last_trim = Some(now);
  }
}

/// Whether a trim runs the full GC ([`IdleTrimConfig::max_gc_heap`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GcGate {
  max_heap: Option<u64>,
  /// The V8 heap's used size right after the last trim's full GC: mostly
  /// live data.
  heap_after_last_gc: Option<u64>,
}

impl GcGate {
  fn new(config: &IdleTrimConfig) -> Self {
    Self {
      max_heap: config.max_gc_heap,
      heap_after_last_gc: None,
    }
  }

  /// Whether this trim runs the GC, with the heap's used size now. It is
  /// skipped only while the heap is over the limit both as the last GC left
  /// it (live data, not a burst's garbage) and now (it hasn't shrunk below
  /// the limit since); the first trim always runs it.
  pub(crate) fn should_gc(&self, heap_used_now: u64) -> bool {
    match (self.max_heap, self.heap_after_last_gc) {
      (Some(max), Some(after_gc)) => after_gc < max || heap_used_now < max,
      _ => true,
    }
  }

  /// Records the heap's used size right after a trim's GC.
  pub(crate) fn record_gc(&mut self, heap_used_after: u64) {
    self.heap_after_last_gc = Some(heap_used_after);
  }
}

thread_local! {
  /// Set on the event loop's thread while it runs a trim.
  static TRIMMING: Cell<bool> = const { Cell::new(false) };
}

/// Wraps the task's waker so a wake-up of the event loop (a timer, I/O, an op
/// completing, a cross-thread task) is recorded as activity.
struct ActivityWake {
  woken: Arc<AtomicBool>,
  inner: Waker,
}

impl Wake for ActivityWake {
  fn wake(self: Arc<Self>) {
    self.wake_by_ref();
  }

  fn wake_by_ref(self: &Arc<Self>) {
    // A wake-up on the trimming thread during a trim comes from the trim
    // itself (V8 posting the GC's follow-up tasks, finalizers): not new
    // activity. One from any other thread (a blocking op completing, a
    // worker's message) is, and is kept.
    if !TRIMMING.with(Cell::get) {
      self.woken.store(true, Ordering::Release);
    }
    self.inner.wake_by_ref();
  }
}

/// Per-worker idle-trim state, driven from the worker's event loop poll.
pub(crate) struct IdleTrim {
  config: IdleTrimConfig,
  scheduler: IdleTrimScheduler,
  woken: Arc<AtomicBool>,
  /// The wrapping waker and the task waker it wraps.
  waker: Option<(Waker, Waker)>,
  sleep: Pin<Box<tokio::time::Sleep>>,
  sleep_deadline: Option<Instant>,
  /// Reads the process RSS in bytes (`None` where it can't).
  read_rss: fn() -> Option<u64>,
  /// The RSS the last trim left.
  rss_after_trim: Option<u64>,
  gc_gate: GcGate,
}

impl IdleTrim {
  pub(crate) fn new(config: IdleTrimConfig) -> Self {
    Self::with_rss_reader(config, rss_bytes)
  }

  fn with_rss_reader(
    config: IdleTrimConfig,
    read_rss: fn() -> Option<u64>,
  ) -> Self {
    let now = Instant::now();
    Self {
      config,
      scheduler: IdleTrimScheduler::new(&config, now),
      // The first poll counts as activity.
      woken: Arc::new(AtomicBool::new(true)),
      waker: None,
      sleep: Box::pin(tokio::time::sleep_until(
        tokio::time::Instant::from_std(now + config.delay),
      )),
      sleep_deadline: None,
      read_rss,
      rss_after_trim: None,
      gc_gate: GcGate::new(&config),
    }
  }

  fn arm(&mut self, deadline: Instant) {
    self
      .sleep
      .as_mut()
      .reset(tokio::time::Instant::from_std(deadline));
    self.sleep_deadline = Some(deadline);
  }

  /// Whether the memory grew enough since the last trim to trim again.
  fn grown_since_last_trim(&self) -> bool {
    match (self.rss_after_trim, (self.read_rss)()) {
      (Some(after), Some(now)) => {
        now >= after.saturating_add(self.config.min_growth)
      }
      _ => true,
    }
  }

  pub(crate) fn config(&self) -> &IdleTrimConfig {
    &self.config
  }

  fn activity_waker(&mut self, task: &Waker) -> Waker {
    if let Some((waker, inner)) = &self.waker
      && inner.will_wake(task)
    {
      return waker.clone();
    }
    let waker = Waker::from(Arc::new(ActivityWake {
      woken: self.woken.clone(),
      inner: task.clone(),
    }));
    self.waker = Some((waker.clone(), task.clone()));
    waker
  }

  /// Polls the event loop (`poll_loop`) with a waker that records its
  /// wake-ups, then, while it stays pending, runs `trim` once it has gone
  /// `delay` without one (and `min_interval` has passed since the last trim).
  pub(crate) fn poll<S, T>(
    &mut self,
    cx: &mut Context<'_>,
    state: &mut S,
    poll_loop: impl FnOnce(&mut S, &mut Context<'_>) -> Poll<T>,
    trim: impl FnOnce(&mut S, &mut GcGate),
  ) -> Poll<T> {
    if self.woken.swap(false, Ordering::AcqRel) {
      self.scheduler.on_activity(Instant::now());
    }
    let waker = self.activity_waker(cx.waker());
    let result = poll_loop(state, &mut Context::from_waker(&waker));
    if result.is_ready() {
      return result;
    }
    // Woken during its own poll: busy, it will be polled again at once.
    if self.woken.load(Ordering::Acquire) {
      return Poll::Pending;
    }
    while let Some(deadline) = self.scheduler.deadline() {
      // Activity only ever moves the deadline later, so the sleep is not
      // re-armed on every busy poll (a timer-wheel update each time): one
      // armed for an earlier instant fires, and is re-armed then. Under
      // steady load that is one reset per `delay`.
      match self.sleep_deadline {
        Some(armed) if armed <= deadline => {}
        _ => self.arm(deadline),
      }
      if self.sleep.as_mut().poll(cx).is_pending() {
        return Poll::Pending;
      }
      if self.sleep_deadline.is_some_and(|fired| fired < deadline) {
        self.arm(deadline);
        continue;
      }
      // The sleep fired for `deadline`, so the trim is due (the timer's
      // millisecond rounding never lets this spin).
      let now = Instant::now().max(deadline);
      if self.scheduler.take_due(now) {
        if self.grown_since_last_trim() {
          self.scheduler.record_trim(now);
          // The GC's own follow-up wake-ups (V8's posted tasks, finalizers)
          // come on this thread and are not new activity (`ActivityWake`);
          // a wake-up from another thread during the trim is, and re-arms.
          TRIMMING.with(|t| t.set(true));
          let guard = TrimmingGuard;
          trim(state, &mut self.gc_gate);
          drop(guard);
          self.rss_after_trim = (self.read_rss)();
        }
        // Disarmed until the next wake-up.
        return Poll::Pending;
      }
    }
    Poll::Pending
  }
}

/// Clears [`TRIMMING`] when the trim ends, unwinding included.
struct TrimmingGuard;

impl Drop for TrimmingGuard {
  fn drop(&mut self) {
    TRIMMING.with(|t| t.set(false));
  }
}

/// A full GC (unless `gate` skips it), then `malloc_trim(0)`. Returns whether
/// the GC ran.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub(crate) fn trim_now(
  isolate: &mut deno_core::v8::Isolate,
  trace: bool,
  gate: &mut GcGate,
) -> bool {
  let rss_before = if trace { rss_bytes() } else { None };
  let heap_used = isolate.get_heap_statistics().used_heap_size() as u64;
  let gc = if gate.should_gc(heap_used) {
    let start = Instant::now();
    isolate.low_memory_notification();
    let elapsed = start.elapsed();
    gate.record_gc(isolate.get_heap_statistics().used_heap_size() as u64);
    Some(elapsed)
  } else {
    None
  };
  let start = Instant::now();
  // SAFETY: calling into libc; `malloc_trim` is thread-safe and touches no
  // Rust-side state.
  let released = unsafe { libc::malloc_trim(0) } != 0;
  let trim = start.elapsed();
  if trace {
    print_trace(gc, heap_used, trim, released, rss_before, rss_bytes());
  }
  gc.is_some()
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[allow(clippy::print_stderr, reason = "opt-in trace output")]
fn print_trace(
  gc: Option<Duration>,
  heap_used: u64,
  trim: Duration,
  released: bool,
  rss_before: Option<u64>,
  rss_after: Option<u64>,
) {
  let mb = |b: Option<u64>| {
    b.map(|b| format!("{:.1}", b as f64 / 1048576.0))
      .unwrap_or_else(|| "?".into())
  };
  let gc = match gc {
    Some(gc) => format!("gc {:.2} ms", gc.as_secs_f64() * 1000.0),
    None => format!("gc skipped (heap {} MB)", mb(Some(heap_used))),
  };
  eprintln!(
    "idle-trim: {gc}, malloc_trim {:.2} ms (released: {}), rss {} -> {} MB",
    trim.as_secs_f64() * 1000.0,
    released,
    mb(rss_before),
    mb(rss_after),
  );
}

/// No-op off glibc Linux.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub(crate) fn trim_now(
  _isolate: &mut deno_core::v8::Isolate,
  _trace: bool,
  _gate: &mut GcGate,
) -> bool {
  false
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn rss_bytes() -> Option<u64> {
  None
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn rss_bytes() -> Option<u64> {
  #[allow(clippy::disallowed_methods, reason = "procfs read")]
  let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
  let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
  // SAFETY: sysconf has no preconditions.
  let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
  Some(pages * page.max(4096) as u64)
}

#[cfg(test)]
mod tests {
  use std::cell::Cell;
  use std::rc::Rc;

  use super::*;

  fn config(delay_ms: u64, min_interval_ms: u64) -> IdleTrimConfig {
    IdleTrimConfig {
      delay: Duration::from_millis(delay_ms),
      min_interval: Duration::from_millis(min_interval_ms),
      min_growth: DEFAULT_MIN_GROWTH_BYTES,
      max_gc_heap: Some(DEFAULT_MAX_GC_HEAP_BYTES),
      trace: false,
    }
  }

  #[test]
  fn config_reads_the_environment() {
    let defaults = IdleTrimConfig::from_lookup(|_| None);
    assert_eq!(defaults, IdleTrimConfig::default());
    let set = IdleTrimConfig::from_lookup(|name| match name {
      "DENO_IDLE_TRIM_DELAY_MS" => Some("250".into()),
      "DENO_IDLE_TRIM_MIN_INTERVAL_MS" => Some(" 3000 ".into()),
      "DENO_IDLE_TRIM_MIN_GROWTH_MB" => Some("64".into()),
      "DENO_IDLE_TRIM_MAX_GC_HEAP_MB" => Some("4096".into()),
      "DENO_IDLE_TRIM_TRACE" => Some("1".into()),
      _ => None,
    });
    assert_eq!(
      set,
      IdleTrimConfig {
        delay: Duration::from_millis(250),
        min_interval: Duration::from_millis(3000),
        min_growth: 64 * 1024 * 1024,
        max_gc_heap: Some(4096 * 1024 * 1024),
        trace: true,
      }
    );
    // `0` turns the GC limit off.
    let unlimited = IdleTrimConfig::from_lookup(|name| {
      (name == "DENO_IDLE_TRIM_MAX_GC_HEAP_MB").then(|| "0".into())
    });
    assert_eq!(unlimited.max_gc_heap, None);
    // Garbage falls back to the default.
    let bad = IdleTrimConfig::from_lookup(|name| match name {
      "DENO_IDLE_TRIM_DELAY_MS" => Some("soon".into()),
      "DENO_IDLE_TRIM_TRACE" => Some("yes".into()),
      _ => None,
    });
    assert_eq!(bad, IdleTrimConfig::default());
  }

  #[test]
  fn trims_once_the_loop_is_idle_for_the_delay() {
    let t0 = Instant::now();
    let ms = Duration::from_millis;
    let mut s = IdleTrimScheduler::new(&config(1000, 10_000), t0);
    assert_eq!(s.deadline(), Some(t0 + ms(1000)));
    // Activity pushes the deadline back.
    s.on_activity(t0 + ms(600));
    assert!(!s.take_due(t0 + ms(1000)));
    assert_eq!(s.deadline(), Some(t0 + ms(1600)));
    assert!(s.take_due(t0 + ms(1600)));
  }

  #[test]
  fn trims_at_most_once_per_idle_period() {
    let t0 = Instant::now();
    let ms = Duration::from_millis;
    let mut s = IdleTrimScheduler::new(&config(1000, 0), t0);
    assert!(trim_if_due(&mut s, t0 + ms(1000)));
    // Still idle: nothing more to do until the loop is woken again.
    assert_eq!(s.deadline(), None);
    assert!(!trim_if_due(&mut s, t0 + ms(60_000)));
    s.on_activity(t0 + ms(60_000));
    assert_eq!(s.deadline(), Some(t0 + ms(61_000)));
    assert!(trim_if_due(&mut s, t0 + ms(61_000)));
  }

  #[test]
  fn keeps_the_minimum_spacing_between_trims() {
    let t0 = Instant::now();
    let ms = Duration::from_millis;
    let mut s = IdleTrimScheduler::new(&config(1000, 10_000), t0);
    assert!(trim_if_due(&mut s, t0 + ms(1000)));
    // A short burst right after the trim, then idle again: the next trim
    // waits for the spacing, not just the idle delay.
    s.on_activity(t0 + ms(2000));
    assert_eq!(s.deadline(), Some(t0 + ms(11_000)));
    assert!(!trim_if_due(&mut s, t0 + ms(3000)));
    assert!(trim_if_due(&mut s, t0 + ms(11_000)));
    // Long after the spacing, the idle delay alone decides.
    s.on_activity(t0 + ms(50_000));
    assert_eq!(s.deadline(), Some(t0 + ms(51_000)));
  }

  /// The scheduler as `IdleTrim::poll` drives it when the trim runs.
  fn trim_if_due(s: &mut IdleTrimScheduler, now: Instant) -> bool {
    let due = s.take_due(now);
    if due {
      s.record_trim(now);
    }
    due
  }

  #[test]
  fn a_skipped_check_does_not_delay_the_next_trim() {
    let t0 = Instant::now();
    let ms = Duration::from_millis;
    let mut s = IdleTrimScheduler::new(&config(1000, 10_000), t0);
    assert!(trim_if_due(&mut s, t0 + ms(1000)));
    // A wake-up that brings no load (a keep-alive connection closing) 4 s
    // after the trim, then idle: the check comes at the spacing and the
    // growth gate skips it (the caller records no trim).
    s.on_activity(t0 + ms(5000));
    assert_eq!(s.deadline(), Some(t0 + ms(11_000)));
    assert!(s.take_due(t0 + ms(11_000)));
    // A burst of load 3 s later, then idle: its trim is due after the idle
    // delay, the spacing counted from the last trim that ran. (Counting the
    // skipped check put it at 21 s: 5 s late on a 15 s burst cycle.)
    s.on_activity(t0 + ms(14_000));
    assert_eq!(s.deadline(), Some(t0 + ms(15_000)));
    assert!(trim_if_due(&mut s, t0 + ms(15_000)));
    // And the spacing still holds after that trim.
    s.on_activity(t0 + ms(16_000));
    assert_eq!(s.deadline(), Some(t0 + ms(25_000)));
  }

  /// A stand-in event loop: pending until `done`, woken by whoever holds the
  /// waker it registered.
  #[derive(Default)]
  struct FakeLoop {
    waker: Option<Waker>,
    polls: usize,
  }

  fn drive(
    trim: &mut IdleTrim,
    fake: &Rc<std::cell::RefCell<FakeLoop>>,
    trims: &Rc<Cell<usize>>,
    done: &Rc<Cell<bool>>,
  ) -> impl Future<Output = ()> {
    std::future::poll_fn(move |cx| {
      let fake = fake.clone();
      let trims = trims.clone();
      let done = done.clone();
      trim.poll(
        cx,
        &mut (),
        move |_, cx| {
          let mut f = fake.borrow_mut();
          f.polls += 1;
          f.waker = Some(cx.waker().clone());
          if done.get() {
            Poll::Ready(())
          } else {
            Poll::Pending
          }
        },
        move |_, _| trims.set(trims.get() + 1),
      )
    })
  }

  #[tokio::test]
  async fn trims_an_idle_loop_once_and_not_a_busy_one() {
    let fake = Rc::new(std::cell::RefCell::new(FakeLoop::default()));
    let trims = Rc::new(Cell::new(0));
    let done = Rc::new(Cell::new(false));
    let mut idle = IdleTrim::new(config(150, 0));
    let local = tokio::task::LocalSet::new();
    local
      .run_until(async {
        // A "busy" loop: woken every 10 ms (far shorter than the delay) for
        // 300 ms. No trim may run.
        let busy = {
          let fake = fake.clone();
          tokio::task::spawn_local(async move {
            for _ in 0..30 {
              tokio::time::sleep(Duration::from_millis(10)).await;
              if let Some(w) = fake.borrow().waker.clone() {
                w.wake();
              }
            }
          })
        };
        let drive_fut = drive(&mut idle, &fake, &trims, &done);
        tokio::pin!(drive_fut);
        tokio::select! {
          _ = &mut drive_fut => unreachable!(),
          _ = busy => {}
        }
        assert_eq!(trims.get(), 0, "trimmed while the loop was busy");
        // Then idle for 800 ms: exactly one trim.
        tokio::select! {
          _ = &mut drive_fut => unreachable!(),
          _ = tokio::time::sleep(Duration::from_millis(800)) => {}
        }
        assert_eq!(trims.get(), 1);
        // The loop finishing still resolves the wrapped poll.
        done.set(true);
        if let Some(w) = fake.borrow().waker.clone() {
          w.wake();
        }
        drive_fut.await;
      })
      .await;
    assert_eq!(trims.get(), 1);
    assert!(fake.borrow().polls > 30);
  }

  static FAKE_RSS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

  fn fake_rss() -> Option<u64> {
    Some(FAKE_RSS.load(Ordering::SeqCst))
  }

  /// Polls an always-pending loop through `idle`, letting one idle period
  /// pass; returns how many trims ran.
  async fn one_idle_period(idle: &mut IdleTrim) -> usize {
    idle_period_within(idle, Duration::from_millis(400)).await
  }

  /// [`one_idle_period`], waiting `window` for the trim.
  async fn idle_period_within(idle: &mut IdleTrim, window: Duration) -> usize {
    let trims = Cell::new(0);
    // Activity first, then idle past the delay.
    idle.woken.store(true, Ordering::SeqCst);
    let fut = std::future::poll_fn(|cx| {
      idle.poll(
        cx,
        &mut (),
        |_, _| Poll::<()>::Pending,
        |_, _| trims.set(trims.get() + 1),
      )
    });
    let _ = tokio::time::timeout(window, fut).await;
    trims.get()
  }

  #[tokio::test]
  async fn skips_a_trim_unless_memory_grew_since_the_last() {
    const MB: u64 = 1024 * 1024;
    let mut idle = IdleTrim::with_rss_reader(config(50, 0), fake_rss);
    FAKE_RSS.store(500 * MB, Ordering::SeqCst);
    // The first trim always runs; the fake RSS stays at 500 MB after it.
    assert_eq!(one_idle_period(&mut idle).await, 1);
    // Woken by a timer, idle again, no growth: skipped.
    FAKE_RSS.store(510 * MB, Ordering::SeqCst);
    assert_eq!(one_idle_period(&mut idle).await, 0);
    // Grown by the threshold (16 MB): trims again.
    FAKE_RSS.store(516 * MB, Ordering::SeqCst);
    assert_eq!(one_idle_period(&mut idle).await, 1);
  }

  static FAKE_RSS_SPACED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

  fn fake_rss_spaced() -> Option<u64> {
    Some(FAKE_RSS_SPACED.load(Ordering::SeqCst))
  }

  #[tokio::test]
  async fn a_skipped_check_leaves_the_spacing_to_the_last_trim() {
    const MB: u64 = 1024 * 1024;
    let ms = Duration::from_millis;
    let mut idle = IdleTrim::with_rss_reader(config(50, 1000), fake_rss_spaced);
    FAKE_RSS_SPACED.store(500 * MB, Ordering::SeqCst);
    // The first trim, 50 ms into the period.
    assert_eq!(idle_period_within(&mut idle, ms(200)).await, 1);
    // Woken 200 ms later by something that brings no load: its check waits
    // for the spacing (1 s after the trim) and is skipped (no growth).
    assert_eq!(idle_period_within(&mut idle, ms(1200)).await, 0);
    // Then load that grew the heap: trimmed after the idle delay, as the
    // last trim was over a second ago. Counting the skipped check as a trim
    // pushed this one past the window.
    FAKE_RSS_SPACED.store(600 * MB, Ordering::SeqCst);
    assert_eq!(idle_period_within(&mut idle, ms(400)).await, 1);
  }
  fn no_rss() -> Option<u64> {
    None
  }

  /// Lets idle periods pass on an always-pending loop whose trim calls
  /// `during_trim` with the task's waker the first time; returns how many
  /// trims ran within `window`.
  async fn trims_with_a_wake_during_the_first(
    during_trim: fn(&Waker),
    window: Duration,
  ) -> usize {
    // No RSS reading: the growth gate never skips.
    let mut idle = IdleTrim::with_rss_reader(config(50, 0), no_rss);
    let waker = std::cell::RefCell::new(None::<Waker>);
    let trims = Cell::new(0);
    let fut = std::future::poll_fn(|cx| {
      idle.poll(
        cx,
        &mut (),
        |_, cx| {
          *waker.borrow_mut() = Some(cx.waker().clone());
          Poll::<()>::Pending
        },
        |_, _| {
          trims.set(trims.get() + 1);
          if trims.get() == 1
            && let Some(waker) = waker.borrow().as_ref()
          {
            during_trim(waker);
          }
        },
      )
    });
    let _ = tokio::time::timeout(window, fut).await;
    trims.get()
  }

  #[tokio::test]
  async fn a_wake_from_another_thread_during_a_trim_is_activity() {
    // Work that arrives during the trim (a blocking op completing, a
    // worker's message) wakes the loop from another thread: it is activity,
    // so once that is idle again the loop is trimmed again. Clearing the flag
    // after the trim dropped it, and the loop stayed disarmed (one trim).
    let trims = trims_with_a_wake_during_the_first(
      |waker| {
        let waker = waker.clone();
        std::thread::spawn(move || waker.wake()).join().unwrap();
      },
      Duration::from_millis(400),
    )
    .await;
    assert_eq!(trims, 2);
  }

  #[tokio::test]
  async fn a_wake_from_the_trim_itself_is_not_activity() {
    // The GC's follow-up wake-ups (V8's posted tasks, finalizers) come on the
    // trimming thread during the trim: not activity, so one trim only.
    let trims = trims_with_a_wake_during_the_first(
      Waker::wake_by_ref,
      Duration::from_millis(400),
    )
    .await;
    assert_eq!(trims, 1);
    assert!(!TRIMMING.with(Cell::get), "the trim flag is left set");
  }

  #[test]
  fn a_large_heap_skips_the_gc_after_the_first() {
    const MB: u64 = 1024 * 1024;
    let mut gate = GcGate::new(&IdleTrimConfig {
      max_gc_heap: Some(1024 * MB),
      ..IdleTrimConfig::default()
    });
    // The first trim always runs the GC, however large the heap.
    assert!(gate.should_gc(3000 * MB));
    // It left 2 GB live: later trims skip the GC while the heap is over the
    // limit.
    gate.record_gc(2000 * MB);
    assert!(!gate.should_gc(2100 * MB));
    assert!(!gate.should_gc(1024 * MB));
    // The heap has shrunk below the limit (V8's own GCs freed the cache):
    // cheap again, so the GC runs and its result is the new reference.
    assert!(gate.should_gc(900 * MB));
    gate.record_gc(300 * MB);
    // A burst's garbage over a small live heap is still collected.
    assert!(gate.should_gc(3000 * MB));
  }

  #[test]
  fn the_gc_is_never_skipped_without_a_limit() {
    let mut gate = GcGate::new(&IdleTrimConfig {
      max_gc_heap: None,
      ..IdleTrimConfig::default()
    });
    gate.record_gc(u64::MAX / 2);
    assert!(gate.should_gc(u64::MAX / 2));
    // The default limit is 1 GiB.
    assert_eq!(
      IdleTrimConfig::default().max_gc_heap,
      Some(DEFAULT_MAX_GC_HEAP_BYTES)
    );
  }

  /// On glibc Linux, against a real isolate: a trim whose GC leaves the heap
  /// over the limit makes the next trim skip the GC.
  #[cfg(all(target_os = "linux", target_env = "gnu"))]
  #[tokio::test]
  async fn trim_now_skips_the_gc_while_the_live_heap_is_over_the_limit() {
    // In a tokio runtime: V8 posts delayed tasks (its memory reducer) through
    // deno_core's platform, which needs one.
    const MB: u64 = 1024 * 1024;
    let mut runtime = deno_core::JsRuntime::new(Default::default());
    // About 40 MB kept alive by a global.
    runtime
      .execute_script(
        "keep.js",
        "globalThis.keep = Array.from({ length: 400_000 }, (_, i) => \
         ({ i, s: `item-${i}-${'x'.repeat(48)}` }));",
      )
      .unwrap();
    let mut gate = GcGate::new(&IdleTrimConfig {
      max_gc_heap: Some(16 * MB),
      ..IdleTrimConfig::default()
    });
    assert!(trim_now(runtime.v8_isolate(), false, &mut gate));
    assert!(!trim_now(runtime.v8_isolate(), false, &mut gate));
    // Dropped: the heap falls below the limit and the GC runs again.
    runtime
      .execute_script("drop.js", "globalThis.keep = null;")
      .unwrap();
    runtime.v8_isolate().low_memory_notification();
    assert!(trim_now(runtime.v8_isolate(), false, &mut gate));
  }
}
