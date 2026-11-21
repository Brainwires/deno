// Copyright 2018-2026 the Deno authors. MIT license.

//! Event loop phase state.
//!
//! The actual phase-based event loop is driven by `poll_event_loop_inner` in
//! `jsruntime.rs`. This module provides auxiliary state for phases that need
//! Rust-side callback queues (currently only close callbacks).
//!
//! libuv-style phases (timers, idle, prepare, poll, check) are driven
//! directly through `UvLoopInner` when a uv_loop is registered.

use std::cell::Cell;
use std::collections::VecDeque;

thread_local! {
  /// How many times the executor of this thread has polled its I/O driver
  /// (as far as [`reactor_polled`] was told).
  static REACTOR_POLLS: Cell<u64> = const { Cell::new(0) };
}

/// Tells the event loops polled on this thread that their executor has just
/// polled its I/O driver: call it from the executor's unpark hook (tokio's
/// `Builder::on_thread_unpark`).
///
/// An event loop iteration is preceded by an I/O poll, as in libuv. A loop
/// that is woken after such a poll (the usual case: it waited, the executor
/// parked, something arrived) runs at once; one woken with no poll since its
/// last iteration is woken through the executor's next non-blocking poll.
/// Without this call a wake made on the executor's thread takes that extra
/// poll even after the executor parked, which is correct and costs a system
/// call.
pub fn reactor_polled() {
  let _ =
    REACTOR_POLLS.try_with(|polls| polls.set(polls.get().wrapping_add(1)));
}

/// The number of I/O polls [`reactor_polled`] counted on this thread.
pub(crate) fn reactor_polls() -> u64 {
  REACTOR_POLLS.try_with(Cell::get).unwrap_or(0)
}

/// A value that identifies the current thread among the live ones (0 on a
/// thread that is being torn down, which no live thread has).
pub(crate) fn thread_token() -> usize {
  REACTOR_POLLS
    .try_with(|polls| std::ptr::from_ref(polls) as usize)
    .unwrap_or(0)
}

/// Close callback for resource cleanup.
pub struct CloseCallback {
  pub callback: Box<dyn FnOnce()>,
}

/// V8 close callback that needs a scope to run JS.
/// Used by HandleWrap.close() to defer the JS close callback to the
/// close phase of the event loop, matching libuv's uv_close behavior.
pub struct V8CloseCallback {
  pub callback: Box<dyn FnOnce(&mut v8::PinScope<'_, '_>) + 'static>,
}

/// Phase-specific state for the event loop.
///
/// Currently only tracks close callbacks. Other phase hooks (idle, prepare,
/// check) are handled by `UvLoopInner` for the libuv compat path.
#[derive(Default)]
pub struct EventLoopPhases {
  /// Phase 6: Close callbacks.
  pub close_callbacks: VecDeque<CloseCallback>,
  /// Phase 6: V8 close callbacks (need a scope to call JS).
  pub v8_close_callbacks: VecDeque<V8CloseCallback>,
}

impl EventLoopPhases {
  /// Drain and run all close callbacks.
  pub fn run_close_callbacks(&mut self) {
    while let Some(cb) = self.close_callbacks.pop_front() {
      (cb.callback)();
    }
  }

  /// Drain all V8 close callbacks (called with a scope from jsruntime).
  pub(crate) fn drain_v8_close_callbacks(&mut self) -> Vec<V8CloseCallback> {
    self.v8_close_callbacks.drain(..).collect()
  }
}
