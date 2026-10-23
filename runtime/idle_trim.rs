// Copyright 2018-2026 the Deno authors. MIT license.

//! denext (DENEXT.md item 101): `--unstable-idle-trim`.
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
//!   again) and never twice within [`IdleTrimConfig::min_interval`].
//! - glibc Linux only. On every other target (macOS, Windows, musl) enabling
//!   it does nothing.

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

/// Whether a trim can do anything on this target (glibc Linux).
pub const SUPPORTED: bool = cfg!(all(target_os = "linux", target_env = "gnu"));

/// Tunables for the idle trim, read from the environment by
/// [`IdleTrimConfig::from_env`]:
///
/// - `DENO_IDLE_TRIM_DELAY_MS`: [`IdleTrimConfig::delay`] (default 1000).
/// - `DENO_IDLE_TRIM_MIN_INTERVAL_MS`: [`IdleTrimConfig::min_interval`]
///   (default 10000).
/// - `DENO_IDLE_TRIM_TRACE=1`: print one line per trim to stderr (GC and
///   `malloc_trim` durations, RSS before and after).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdleTrimConfig {
  pub delay: Duration,
  pub min_interval: Duration,
  pub trace: bool,
}

impl Default for IdleTrimConfig {
  fn default() -> Self {
    Self {
      delay: DEFAULT_IDLE_DELAY,
      min_interval: DEFAULT_MIN_INTERVAL,
      trace: false,
    }
  }
}

impl IdleTrimConfig {
  pub fn from_env() -> Self {
    Self::from_lookup(|name| std::env::var(name).ok())
  }

  fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
    let millis = |name: &str, default: Duration| {
      get(name)
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(default)
    };
    Self {
      delay: millis("DENO_IDLE_TRIM_DELAY_MS", DEFAULT_IDLE_DELAY),
      min_interval: millis(
        "DENO_IDLE_TRIM_MIN_INTERVAL_MS",
        DEFAULT_MIN_INTERVAL,
      ),
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

  /// Claims the trim if it is due at `now`.
  fn take_due(&mut self, now: Instant) -> bool {
    match self.deadline() {
      Some(deadline) if now >= deadline => {
        self.armed = false;
        self.last_trim = Some(now);
        true
      }
      _ => false,
    }
  }
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
    self.woken.store(true, Ordering::Release);
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
}

impl IdleTrim {
  pub(crate) fn new(config: IdleTrimConfig) -> Self {
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
    trim: impl FnOnce(&mut S),
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
      if self.sleep_deadline != Some(deadline) {
        self
          .sleep
          .as_mut()
          .reset(tokio::time::Instant::from_std(deadline));
        self.sleep_deadline = Some(deadline);
      }
      if self.sleep.as_mut().poll(cx).is_pending() {
        return Poll::Pending;
      }
      // The sleep fired for `deadline`, so the trim is due (the timer's
      // millisecond rounding never lets this spin).
      if self.scheduler.take_due(Instant::now().max(deadline)) {
        trim(state);
        // The GC's own follow-up work (finalizers) is not new activity.
        self.woken.store(false, Ordering::Release);
      }
    }
    Poll::Pending
  }
}

/// A full GC, then `malloc_trim(0)`.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub(crate) fn trim_now(isolate: &mut deno_core::v8::Isolate, trace: bool) {
  let rss_before = if trace { rss_bytes() } else { None };
  let start = Instant::now();
  isolate.low_memory_notification();
  let gc = start.elapsed();
  let start = Instant::now();
  // SAFETY: calling into libc; `malloc_trim` is thread-safe and touches no
  // Rust-side state.
  let released = unsafe { libc::malloc_trim(0) } != 0;
  let trim = start.elapsed();
  if trace {
    print_trace(gc, trim, released, rss_before, rss_bytes());
  }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[allow(clippy::print_stderr, reason = "opt-in trace output")]
fn print_trace(
  gc: Duration,
  trim: Duration,
  released: bool,
  rss_before: Option<u64>,
  rss_after: Option<u64>,
) {
  let mb = |b: Option<u64>| {
    b.map(|b| format!("{:.1}", b as f64 / 1048576.0))
      .unwrap_or_else(|| "?".into())
  };
  eprintln!(
    "idle-trim: gc {:.2} ms, malloc_trim {:.2} ms (released: {}), rss {} -> {} MB",
    gc.as_secs_f64() * 1000.0,
    trim.as_secs_f64() * 1000.0,
    released,
    mb(rss_before),
    mb(rss_after),
  );
}

/// No-op off glibc Linux.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub(crate) fn trim_now(_isolate: &mut deno_core::v8::Isolate, _trace: bool) {}

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
      "DENO_IDLE_TRIM_TRACE" => Some("1".into()),
      _ => None,
    });
    assert_eq!(
      set,
      IdleTrimConfig {
        delay: Duration::from_millis(250),
        min_interval: Duration::from_millis(3000),
        trace: true,
      }
    );
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
    assert!(s.take_due(t0 + ms(1000)));
    // Still idle: nothing more to do until the loop is woken again.
    assert_eq!(s.deadline(), None);
    assert!(!s.take_due(t0 + ms(60_000)));
    s.on_activity(t0 + ms(60_000));
    assert_eq!(s.deadline(), Some(t0 + ms(61_000)));
    assert!(s.take_due(t0 + ms(61_000)));
  }

  #[test]
  fn keeps_the_minimum_spacing_between_trims() {
    let t0 = Instant::now();
    let ms = Duration::from_millis;
    let mut s = IdleTrimScheduler::new(&config(1000, 10_000), t0);
    assert!(s.take_due(t0 + ms(1000)));
    // A short burst right after the trim, then idle again: the next trim
    // waits for the spacing, not just the idle delay.
    s.on_activity(t0 + ms(2000));
    assert_eq!(s.deadline(), Some(t0 + ms(11_000)));
    assert!(!s.take_due(t0 + ms(3000)));
    assert!(s.take_due(t0 + ms(11_000)));
    // Long after the spacing, the idle delay alone decides.
    s.on_activity(t0 + ms(50_000));
    assert_eq!(s.deadline(), Some(t0 + ms(51_000)));
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
        move |_| trims.set(trims.get() + 1),
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
}
