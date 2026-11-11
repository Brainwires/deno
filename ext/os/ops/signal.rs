// Copyright 2018-2026 the Deno authors. MIT license.
use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use deno_core::AsyncRefCell;
use deno_core::OpState;
use deno_core::RcRef;
use deno_core::Resource;
use deno_core::ResourceId;
use deno_core::error::ResourceError;
use deno_core::op2;

#[derive(Debug, thiserror::Error, deno_error::JsError)]
pub enum SignalError {
  #[class(type)]
  #[error(transparent)]
  InvalidSignalStr(#[from] deno_signals::InvalidSignalStrError),
  #[class(type)]
  #[error(transparent)]
  InvalidSignalInt(#[from] deno_signals::InvalidSignalIntError),
  #[class(type)]
  #[error("Binding to signal '{0}' is not allowed")]
  SignalNotAllowed(String),
  #[class(inherit)]
  #[error("{0}")]
  Io(#[from] std::io::Error),
}

struct SignalStreamResource {
  signo: i32,
  id: u32,
  rx: AsyncRefCell<tokio::sync::watch::Receiver<()>>,
  /// denext: how many deliveries this resource was handed, in all
  /// (`deno_signals::dispatch_started` counts each).
  delivered: Arc<AtomicU64>,
  /// denext: how many of them the listeners finished, in all; the rest are
  /// finished when the resource goes.
  finished: AtomicU64,
}

impl SignalStreamResource {
  /// The listeners finished every delivery up to the `seen`th.
  fn finish_up_to(&self, seen: u64) {
    let seen = seen.min(self.delivered.load(Ordering::Acquire));
    let before = self.finished.fetch_max(seen, Ordering::AcqRel);
    deno_signals::dispatch_finished(self.signo, seen.saturating_sub(before));
  }
}

impl Resource for SignalStreamResource {
  fn name(&self) -> Cow<'_, str> {
    "signal".into()
  }

  fn close(self: Rc<Self>) {
    deno_signals::unregister(self.signo, self.id);
    self.finish_up_to(u64::MAX);
  }
}

impl Drop for SignalStreamResource {
  fn drop(&mut self) {
    deno_signals::unregister(self.signo, self.id);
    self.finish_up_to(u64::MAX);
  }
}

#[op2(fast)]
#[smi]
pub fn op_signal_bind(
  state: &mut OpState,
  #[string] sig: &str,
) -> Result<ResourceId, SignalError> {
  let signo = deno_signals::signal_str_to_int(sig)?;
  if deno_signals::is_forbidden(signo) {
    return Err(SignalError::SignalNotAllowed(sig.to_string()));
  }

  let (tx, rx) = tokio::sync::watch::channel(());
  let delivered = Arc::new(AtomicU64::new(0));
  let id = deno_signals::register(signo, true, {
    let delivered = delivered.clone();
    Box::new(move || {
      // Counted before the listener can see it (see
      // `deno_signals::wait_for_dispatches`).
      deno_signals::dispatch_started(signo);
      delivered.fetch_add(1, Ordering::AcqRel);
      let _ = tx.send(());
    })
  })?;

  let rid = state.resource_table.add(SignalStreamResource {
    signo,
    id,
    rx: AsyncRefCell::new(rx),
    delivered,
    finished: AtomicU64::new(0),
  });

  Ok(rid)
}

/// Waits for the next delivery. Resolves to how many deliveries the resource
/// has been handed in all (at least 1; deliveries close together are
/// coalesced), or 0 once the resource is closed. The listeners report them
/// finished with `op_signal_dispatched`.
#[op2]
#[number]
pub async fn op_signal_poll(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
) -> Result<u64, ResourceError> {
  let resource = state
    .borrow_mut()
    .resource_table
    .get::<SignalStreamResource>(rid)?;

  let mut rx = RcRef::map(&resource, |r| &r.rx).borrow_mut().await;

  if rx.changed().await.is_err() {
    return Ok(0);
  }
  Ok(resource.delivered.load(Ordering::Acquire).max(1))
}

/// denext: the listeners finished every delivery up to the `seen`th (what
/// `op_signal_poll` resolved to): they returned, and the promises they
/// returned settled. The
/// Windows console control handler waits for this (at most the time Windows
/// gives) before it lets Windows end the process.
#[op2(fast)]
pub fn op_signal_dispatched(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[number] seen: u64,
) {
  if let Ok(resource) = state.resource_table.get::<SignalStreamResource>(rid) {
    resource.finish_up_to(seen);
  }
}

#[op2(fast)]
pub fn op_signal_unbind(
  state: &mut OpState,
  #[smi] rid: ResourceId,
) -> Result<(), ResourceError> {
  let resource = state.resource_table.take::<SignalStreamResource>(rid)?;
  resource.close();
  Ok(())
}
