// Copyright 2018-2026 the Deno authors. MIT license.

use std::future::Future;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;

use tokio::time::Instant;
use tokio::time::Sleep;

use crate::reactor::Reactor;
use crate::reactor::ReactorInstant;
use crate::reactor::ReactorTimer;

/// Default reactor implementation backed by tokio.
#[derive(Default)]
pub struct TokioReactor;

impl Reactor for TokioReactor {
  type Timer = TokioTimer;
  type Instant = Instant;

  fn timer(&self, deadline: Self::Instant) -> Self::Timer {
    TokioTimer {
      sleep: Box::pin(tokio::time::sleep_until(wheel_deadline(deadline))),
      deadline,
    }
  }

  fn now(&self) -> Self::Instant {
    Instant::now()
  }

  fn poll(&self, cx: &mut Context, _timeout: Option<Duration>) -> Poll<()> {
    // Tokio's reactor is driven implicitly by the runtime,
    // so we just yield back.
    cx.waker().wake_by_ref();
    Poll::Pending
  }

  fn spawn(
    &self,
    fut: Pin<Box<dyn Future<Output = ()> + 'static>>,
  ) -> Pin<Box<dyn Future<Output = ()>>> {
    let handle = deno_unsync::spawn(fut);
    Box::pin(async move {
      let _ = handle.await;
    })
  }
}

/// The whole millisecond less a nanosecond: tokio's timer wheel rounds a
/// deadline up to the end of its millisecond (`deadline + 999_999 ns`,
/// truncated to whole milliseconds since the runtime started).
const WHEEL_ROUNDING: Duration = Duration::from_nanos(999_999);

/// The deadline to hand tokio's timer wheel for a timer due at `deadline`.
///
/// The wheel counts whole milliseconds since the runtime started and rounds a
/// deadline up to the end of its millisecond; the driver then parks for the
/// whole milliseconds between that tick and the tick it is in now, from now.
/// A 1 ms timer set at `n + f` ms is due at `n + 1 + f`, lands on tick
/// `n + 2`, and the driver parks 2 ms: it fires about 2 ms after it was set,
/// not 1 (Node.js, whose libuv counts the same whole milliseconds from the
/// loop's own clock, fires it after about 1). Handing the wheel the deadline
/// less its rounding puts the timer on the tick the deadline is in (`n + 1`),
/// so the driver parks 1 ms from `n + f`: it wakes at the deadline, plus the
/// time from setting the timer to parking. Where it wakes before the
/// deadline (the driver parked after its millisecond had turned, or woke for
/// something else at the tick), [`TokioTimer`] waits for the deadline itself.
pub(crate) fn wheel_deadline(deadline: Instant) -> Instant {
  deadline.checked_sub(WHEEL_ROUNDING).unwrap_or(deadline)
}

/// A timer backed by tokio's [`Sleep`]. It completes at its deadline or
/// later, never before (HTML and Node.js: a timer waits at least its delay),
/// and as close after it as the timer wheel allows ([`wheel_deadline`]).
pub struct TokioTimer {
  sleep: Pin<Box<Sleep>>,
  /// When the timer is due; `sleep` is set at its [`wheel_deadline`] until
  /// it wakes early, then at this.
  deadline: Instant,
}

impl Future for TokioTimer {
  type Output = ();

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
    loop {
      if self.sleep.as_mut().poll(cx).is_pending() {
        return Poll::Pending;
      }
      if Instant::now() >= self.deadline {
        return Poll::Ready(());
      }
      // Woken within the deadline's millisecond, before it: wait for the
      // deadline itself (the wheel rounds it up, so this one is never
      // early), and register for it.
      let deadline = self.deadline;
      if self.sleep.deadline() == deadline {
        // Already there; can't be early once tokio says it's due, but don't
        // spin if a clock disagrees.
        return Poll::Ready(());
      }
      self.sleep.as_mut().reset(deadline);
    }
  }
}

impl ReactorTimer for TokioTimer {
  type Instant = Instant;

  fn reset(&mut self, deadline: impl Into<Instant>) {
    let deadline = deadline.into();
    self.deadline = deadline;
    self.sleep.as_mut().reset(wheel_deadline(deadline));
  }

  fn deadline(&self) -> Instant {
    self.deadline
  }
}

impl ReactorInstant for Instant {
  fn now() -> Self {
    Instant::now()
  }

  fn elapsed(&self) -> Duration {
    Instant::elapsed(self)
  }

  fn checked_add(&self, duration: Duration) -> Option<Self> {
    Instant::checked_add(self, duration)
  }
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use tokio::time::Instant;

  use super::TokioReactor;
  use crate::reactor::Reactor;
  use crate::reactor::ReactorTimer;

  const MS: Duration = Duration::from_millis(1);

  /// Waits for a timer `delay` from now and returns when it was due and when
  /// it completed (the paused clock advances to the timer wheel's wake-ups).
  async fn fire(delay: Duration) -> (Instant, Instant) {
    let due = Instant::now() + delay;
    TokioReactor.timer(due).await;
    (due, Instant::now())
  }

  // A 1 ms timer set partway through a millisecond fires 1 ms later, not on
  // the tick after (tokio's wheel rounded its deadline up to the end of its
  // millisecond and the driver parked 2 ms: chained `setTimeout(f, 1)` took
  // about 2 ms a hop, where Node.js takes about 1).
  #[tokio::test(start_paused = true)]
  async fn a_one_millisecond_timer_fires_after_one_millisecond() {
    for offset in [0, 1, 300, 500, 999] {
      tokio::time::advance(Duration::from_micros(offset)).await;
      let (due, fired) = fire(MS).await;
      assert_eq!(fired, due, "set {offset} us into a millisecond");
    }
  }

  // Chained, as `setTimeout(f, 1)` from its own callback: each 1 ms.
  #[tokio::test(start_paused = true)]
  async fn chained_one_millisecond_timers_take_one_millisecond_each() {
    tokio::time::advance(Duration::from_micros(400)).await;
    let start = Instant::now();
    for _ in 0..100 {
      fire(MS).await;
    }
    assert_eq!(start.elapsed(), MS * 100);
  }

  // Never early: where the wheel wakes it within the deadline's millisecond
  // but before the deadline (here at 1.3 ms for a deadline at 1.8 ms), it
  // waits for the deadline.
  #[tokio::test(start_paused = true)]
  async fn a_timer_never_fires_before_its_deadline() {
    for (offset, delay) in [(300, 1500), (100, 950), (900, 50), (0, 2500)] {
      tokio::time::advance(Duration::from_micros(offset)).await;
      let (due, fired) = fire(Duration::from_micros(delay)).await;
      assert!(
        fired >= due,
        "{offset} + {delay} us: {:?} early",
        due - fired
      );
      assert!(fired - due < MS * 2, "{offset} + {delay} us: late");
    }
  }

  // `reset` moves the deadline the timer completes at.
  #[tokio::test(start_paused = true)]
  async fn reset_moves_the_deadline() {
    let start = Instant::now();
    let mut timer = TokioReactor.timer(start + MS * 50);
    timer.reset(start + MS * 3);
    assert_eq!(timer.deadline(), start + MS * 3);
    (&mut timer).await;
    assert_eq!(start.elapsed(), MS * 3);
  }
}
