// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: writing coverage (and CPU profiles) when the process is ended by
//! SIGINT or SIGTERM (denoland/deno#32362).
//!
//! The data is flushed through each isolate's inspector, which only its own
//! thread can drive, so a signal cannot flush it from the signal thread. A
//! thread holding such data joins as a [`Participant`] and races its work
//! against [`Participant::signalled`]; once signalled it flushes and drops the
//! participant. The signal (a hook that runs only when no JS listener took
//! the signal over, before its default action ends the process, see
//! [`deno_signals::register_before_default_action`]) waits for every
//! participant to finish, for at most [`FLUSH_TIMEOUT`].
//!
//! A thread stuck in synchronous JavaScript cannot be reached: its data is
//! lost, and the process ends once the timeout passes.
//!
//! The wait blocks the thread that handles signals, so the same signal
//! arriving again (a second Ctrl+C, a second SIGTERM) can't be handled until
//! it ends. The wait watches [`deno_signals::arrival_count`] instead and
//! gives up as soon as the signal arrives again, so the default action ends
//! the process at once.

use std::sync::Condvar;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::Once;
use std::time::Duration;
use std::time::Instant;

use tokio::sync::watch;

/// How long a signal waits for the participants to flush before the process
/// ends anyway.
pub const FLUSH_TIMEOUT: Duration = Duration::from_secs(10);

struct State {
  signalled: watch::Sender<Option<i32>>,
  active: Mutex<usize>,
  idle: Condvar,
}

static STATE: LazyLock<State> = LazyLock::new(|| State {
  signalled: watch::Sender::new(None),
  active: Mutex::new(0),
  idle: Condvar::new(),
});

/// Makes SIGINT and SIGTERM (SIGINT only on Windows) wait for the
/// participants to flush before they end the process, when nothing else
/// handles them. Idempotent.
pub fn install() {
  static INSTALL: Once = Once::new();
  INSTALL.call_once(|| {
    for signal in termination_signals() {
      let result = deno_signals::register_before_default_action(
        signal,
        Box::new(move || {
          let arrived = deno_signals::arrival_count(signal);
          trigger(signal);
          wait_for_flush_unless(FLUSH_TIMEOUT, || {
            deno_signals::arrival_count(signal) != arrived
          });
        }),
      );
      if let Err(err) = result {
        log::debug!("Could not listen for signal {signal}: {err}");
      }
    }
  });
}

fn termination_signals() -> Vec<i32> {
  #[cfg(unix)]
  {
    vec![libc::SIGINT, libc::SIGTERM]
  }
  #[cfg(not(unix))]
  {
    vec![deno_signals::SIGINT]
  }
}

/// Asks every participant to flush, because the process is ending by
/// `signal`. Only the first call counts.
pub fn trigger(signal: i32) {
  STATE.signalled.send_if_modified(|value| {
    if value.is_none() {
      *value = Some(signal);
      true
    } else {
      false
    }
  });
}

/// How often a wait checks whether it should give up early (see
/// [`wait_for_flush_unless`]). A signal handler can't wake a condition
/// variable (that isn't async-signal-safe), so the wait polls.
const GIVE_UP_POLL: Duration = Duration::from_millis(50);

/// Waits until no participant is left, or for `timeout`. Returns whether
/// every participant finished.
pub fn wait_for_flush(timeout: Duration) -> bool {
  wait_for_flush_unless(timeout, || false)
}

/// Like [`wait_for_flush`], but also stops waiting (returning `false`) once
/// `give_up` returns `true`, checked every [`GIVE_UP_POLL`]: the signal
/// that ends the process arrived again, so whoever sent it wants the process
/// gone now.
pub fn wait_for_flush_unless(
  timeout: Duration,
  give_up: impl Fn() -> bool,
) -> bool {
  let deadline = Instant::now() + timeout;
  let mut active = STATE.active.lock().unwrap();
  while *active > 0 {
    let now = Instant::now();
    if now >= deadline || give_up() {
      return false;
    }
    let slice = (deadline - now).min(GIVE_UP_POLL);
    active = STATE.idle.wait_timeout(active, slice).unwrap().0;
  }
  true
}

/// A thread with data to flush before the process ends by a signal. Dropping
/// it tells the signal this thread is done.
pub struct Participant {
  signalled: watch::Receiver<Option<i32>>,
}

impl Participant {
  pub fn join() -> Self {
    *STATE.active.lock().unwrap() += 1;
    Self {
      signalled: STATE.signalled.subscribe(),
    }
  }

  /// Whether the process is ending by a signal.
  pub fn is_signalled(&self) -> bool {
    self.signalled.borrow().is_some()
  }

  /// Resolves with the signal once the process is ending by one.
  pub async fn signalled(&mut self) -> i32 {
    match self.signalled.wait_for(Option::is_some).await {
      Ok(signal) => signal.unwrap(),
      // The sender is static and never dropped.
      Err(_) => std::future::pending().await,
    }
  }
}

impl Drop for Participant {
  fn drop(&mut self) {
    let mut active = STATE.active.lock().unwrap();
    *active -= 1;
    if *active == 0 {
      STATE.idle.notify_all();
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  // The state is process-wide: one test exercises it end to end.
  #[tokio::test]
  async fn participants_flush_before_the_signal_returns() {
    // Nobody to wait for.
    assert!(wait_for_flush(Duration::from_millis(10)));

    let mut first = Participant::join();
    let second = Participant::join();
    // Not signalled yet.
    assert!(
      tokio::time::timeout(Duration::from_millis(20), first.signalled())
        .await
        .is_err()
    );
    // Both still flushing.
    assert!(!wait_for_flush(Duration::from_millis(20)));

    let waiter = std::thread::spawn(|| wait_for_flush(Duration::from_secs(10)));
    trigger(15);
    // A later signal does not replace the first.
    trigger(2);
    assert_eq!(first.signalled().await, 15);
    drop(first);
    assert!(!waiter.is_finished());
    drop(second);
    assert!(waiter.join().unwrap());

    // A participant joining after the signal is signalled at once.
    let mut late = Participant::join();
    assert_eq!(late.signalled().await, 15);

    // The signal arriving again ends the wait early, without the participant.
    let again = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let waiter = {
      let again = again.clone();
      std::thread::spawn(move || {
        let start = Instant::now();
        let flushed = wait_for_flush_unless(Duration::from_secs(10), || {
          again.load(std::sync::atomic::Ordering::Relaxed)
        });
        (flushed, start.elapsed())
      })
    };
    std::thread::sleep(Duration::from_millis(100));
    assert!(!waiter.is_finished());
    again.store(true, std::sync::atomic::Ordering::Relaxed);
    let (flushed, elapsed) = waiter.join().unwrap();
    assert!(!flushed);
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");

    // Nothing to give up on when every participant is done.
    drop(late);
    assert!(wait_for_flush_unless(Duration::from_secs(10), || true));
  }
}
