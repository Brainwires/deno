// Copyright 2018-2026 the Deno authors. MIT license.

//! Web MIDI (`navigator.requestMIDIAccess()`), MIDI 1.0 (denext, DENEXT.md).
//!
//! `01_midi.js` is the Web MIDI API: `MIDIAccess`, the port maps, `MIDIPort`
//! / `MIDIInput` / `MIDIOutput`, the events, message validation, hot-plug
//! (it polls [`op_midi_ports`]). The ops here reach the OS through
//! `backend.rs`:
//!
//! - [`op_midi_request_access`] checks `--allow-midi` (`sysex` for system
//!   exclusive access) and starts the backend;
//! - an input is a resource whose bytes are framed into single messages
//!   (`parser.rs`) on the backend's thread and read in batches by
//!   [`op_midi_input_recv`], each with its arrival time on
//!   `performance.now()`'s clock;
//! - an output is a resource with a scheduler thread: `send(data,
//!   timestamp)` queues the bytes for that `performance.now()` time (now, if
//!   it is past), in timestamp order and, for equal timestamps, in the order
//!   sent; `clear()` drops what hasn't gone out.

use std::borrow::Cow;
use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::Instant;

use deno_core::AsyncRefCell;
use deno_core::CancelFuture;
use deno_core::CancelHandle;
use deno_core::OpState;
use deno_core::RcRef;
use deno_core::Resource;
use deno_core::ResourceId;
use deno_core::ToJsBuffer;
use deno_core::op2;
use deno_permissions::PermissionCheckError;
use deno_permissions::PermissionsContainer;
use serde::Serialize;
use tokio::sync::mpsc;

#[cfg(target_os = "linux")]
mod alsa;
mod backend;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod midir_backend;
pub mod parser;

deno_core::extension!(
  deno_midi,
  deps = [deno_web],
  ops = [
    op_midi_request_access,
    op_midi_ports,
    op_midi_open_input,
    op_midi_input_recv,
    op_midi_open_output,
    op_midi_output_send,
    op_midi_output_clear,
  ],
  lazy_loaded_js = ["01_midi.js"],
);

#[derive(Debug, thiserror::Error, deno_error::JsError)]
pub enum MidiError {
  #[class(inherit)]
  #[error(transparent)]
  Permission(#[from] PermissionCheckError),
  #[class(inherit)]
  #[error(transparent)]
  Resource(#[from] deno_core::error::ResourceError),
  #[class("DOMExceptionNotSupportedError")]
  #[error("{0}")]
  NotSupported(String),
  #[class("DOMExceptionInvalidAccessError")]
  #[error("{0}")]
  InvalidAccess(String),
}

fn backend() -> Result<&'static Mutex<Box<dyn backend::Backend>>, MidiError> {
  backend::backend().map_err(MidiError::NotSupported)
}

fn lock(
  backend: &'static Mutex<Box<dyn backend::Backend>>,
) -> std::sync::MutexGuard<'static, Box<dyn backend::Backend>> {
  // a panic on a backend thread leaves the backend usable
  backend.lock().unwrap_or_else(|e| e.into_inner())
}

/// The `performance.now()` clock's origin (`deno_web`'s start time).
fn time_origin(state: &OpState) -> Instant {
  **state.borrow::<deno_web::StartTime>()
}

#[op2(fast, stack_trace)]
pub fn op_midi_request_access(
  state: &mut OpState,
  sysex: bool,
) -> Result<(), MidiError> {
  state
    .borrow_mut::<PermissionsContainer>()
    .check_midi(sysex, "navigator.requestMIDIAccess()")?;
  backend()?;
  Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Port {
  id: String,
  name: Option<String>,
  manufacturer: Option<String>,
  version: Option<String>,
  #[serde(rename = "type")]
  kind: &'static str,
  software: bool,
}

/// The ports available now. Called only after `op_midi_request_access`
/// succeeded in this isolate.
#[op2]
#[serde]
pub fn op_midi_ports() -> Result<Vec<Port>, MidiError> {
  let backend = backend()?;
  let ports = lock(backend).ports();
  Ok(
    ports
      .into_iter()
      .map(|p| Port {
        id: p.id,
        name: p.name,
        manufacturer: p.manufacturer,
        version: p.version,
        kind: if p.input { "input" } else { "output" },
        software: p.software,
      })
      .collect(),
  )
}

#[derive(Serialize)]
pub struct Message {
  data: ToJsBuffer,
  /// Arrival time, in `performance.now()` milliseconds.
  timestamp: f64,
}

type Received = (Vec<u8>, Instant);

struct MidiInputResource {
  rx: AsyncRefCell<mpsc::UnboundedReceiver<Received>>,
  cancel: CancelHandle,
  connection: RefCell<Option<Box<dyn backend::InputConnection>>>,
  origin: Instant,
}

impl Resource for MidiInputResource {
  fn name(&self) -> Cow<'_, str> {
    "midiInput".into()
  }

  fn close(self: Rc<Self>) {
    self.cancel.cancel();
    // closes the port: no callback runs after this returns
    self.connection.borrow_mut().take();
  }
}

/// Opens an input. With `sysex` false, system exclusive messages are
/// dropped (`MIDIAccess.sysexEnabled`).
#[op2(fast)]
#[smi]
pub fn op_midi_open_input(
  state: &mut OpState,
  #[string] id: String,
  sysex: bool,
) -> Result<ResourceId, MidiError> {
  let backend = backend()?;
  let (tx, rx) = mpsc::unbounded_channel::<Received>();
  let mut parser = parser::MessageParser::new();
  let callback = Box::new(move |bytes: &[u8]| {
    let now = Instant::now();
    parser.feed(bytes, |message| {
      if !sysex && message[0] == 0xF0 {
        return;
      }
      let _ = tx.send((message.to_vec(), now));
    });
  });
  let connection = lock(backend)
    .open_input(&id, callback)
    .map_err(MidiError::InvalidAccess)?;
  let origin = time_origin(state);
  Ok(state.resource_table.add(MidiInputResource {
    rx: AsyncRefCell::new(rx),
    cancel: CancelHandle::default(),
    connection: RefCell::new(Some(connection)),
    origin,
  }))
}

/// The messages received since the last call (waiting for at least one), or
/// `null` once the input is closed.
#[op2]
#[serde]
pub async fn op_midi_input_recv(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
) -> Result<Option<Vec<Message>>, MidiError> {
  let resource = state
    .borrow()
    .resource_table
    .get::<MidiInputResource>(rid)?;
  let mut rx = RcRef::map(&resource, |r| &r.rx).borrow_mut().await;
  let cancel = RcRef::map(&resource, |r| &r.cancel);
  let Ok(Some(first)) = rx.recv().or_cancel(cancel).await else {
    return Ok(None);
  };
  let origin = resource.origin;
  let message = |(data, at): Received| Message {
    timestamp: at.saturating_duration_since(origin).as_secs_f64() * 1000.0,
    data: data.into(),
  };
  let mut batch = vec![message(first)];
  while batch.len() < 1024 {
    match rx.try_recv() {
      Ok(next) => batch.push(message(next)),
      Err(_) => break,
    }
  }
  Ok(Some(batch))
}

#[derive(Default)]
struct Queue {
  /// (due, sequence number, bytes): earliest first, then in send order.
  pending: BinaryHeap<Reverse<(Instant, u64, Vec<u8>)>>,
  next_seq: u64,
  closed: bool,
}

#[derive(Default)]
struct Scheduler {
  queue: Mutex<Queue>,
  wake: Condvar,
}

impl Scheduler {
  fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
    self.queue.lock().unwrap_or_else(|e| e.into_inner())
  }

  /// The output thread: sends each queued payload when it is due, one
  /// message per backend call.
  fn run(&self, mut connection: Box<dyn backend::OutputConnection>) {
    let mut queue = self.lock();
    loop {
      if queue.closed {
        return;
      }
      let Some(Reverse((due, _, _))) = queue.pending.peek() else {
        queue = self.wake.wait(queue).unwrap_or_else(|e| e.into_inner());
        continue;
      };
      let now = Instant::now();
      if *due > now {
        let wait = *due - now;
        queue = self
          .wake
          .wait_timeout(queue, wait)
          .unwrap_or_else(|e| e.into_inner())
          .0;
        continue;
      }
      let Reverse((_, _, bytes)) = queue.pending.pop().unwrap();
      drop(queue);
      for message in parser::split_messages(&bytes) {
        // a send to a port that went away fails quietly, as in browsers;
        // the JavaScript side notices the disconnection
        let _ = connection.send(&message);
      }
      queue = self.lock();
    }
  }
}

struct MidiOutputResource {
  scheduler: Arc<Scheduler>,
  thread: RefCell<Option<JoinHandle<()>>>,
  origin: Instant,
}

impl MidiOutputResource {
  fn stop_scheduler(&self) {
    {
      let mut queue = self.scheduler.lock();
      queue.closed = true;
      queue.pending.clear();
    }
    self.scheduler.wake.notify_all();
    if let Some(thread) = self.thread.borrow_mut().take() {
      // closes the port (the thread owns the connection) before a reopen
      let _ = thread.join();
    }
  }
}

impl Resource for MidiOutputResource {
  fn name(&self) -> Cow<'_, str> {
    "midiOutput".into()
  }

  fn close(self: Rc<Self>) {
    self.stop_scheduler();
  }
}

impl Drop for MidiOutputResource {
  fn drop(&mut self) {
    self.stop_scheduler();
  }
}

#[op2(fast)]
#[smi]
pub fn op_midi_open_output(
  state: &mut OpState,
  #[string] id: String,
) -> Result<ResourceId, MidiError> {
  let backend = backend()?;
  let connection = lock(backend)
    .open_output(&id)
    .map_err(MidiError::InvalidAccess)?;
  let scheduler = Arc::new(Scheduler::default());
  let thread_scheduler = scheduler.clone();
  let thread = std::thread::Builder::new()
    .name("deno-midi-output".to_string())
    .spawn(move || thread_scheduler.run(connection))
    .map_err(|e| MidiError::InvalidAccess(e.to_string()))?;
  let origin = time_origin(state);
  Ok(state.resource_table.add(MidiOutputResource {
    scheduler,
    thread: RefCell::new(Some(thread)),
    origin,
  }))
}

/// Queues `data` (validated by the caller) for `timestamp`, a
/// `performance.now()` time in milliseconds; 0 or a past time sends at once.
#[op2(fast)]
pub fn op_midi_output_send(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[buffer] data: &[u8],
  timestamp: f64,
) -> Result<(), MidiError> {
  let resource = state.resource_table.get::<MidiOutputResource>(rid)?;
  let now = Instant::now();
  let due = if timestamp.is_finite() && timestamp > 0.0 {
    resource
      .origin
      .checked_add(Duration::from_secs_f64((timestamp / 1000.0).min(1e9)))
      .map_or(now, |due| due.max(now))
  } else {
    now
  };
  {
    let mut queue = resource.scheduler.lock();
    let seq = queue.next_seq;
    queue.next_seq += 1;
    queue.pending.push(Reverse((due, seq, data.to_vec())));
  }
  resource.scheduler.wake.notify_all();
  Ok(())
}

/// `MIDIOutput.clear()`: drops what is queued and not yet sent.
#[op2(fast)]
pub fn op_midi_output_clear(
  state: &mut OpState,
  #[smi] rid: ResourceId,
) -> Result<(), MidiError> {
  let resource = state.resource_table.get::<MidiOutputResource>(rid)?;
  resource.scheduler.lock().pending.clear();
  Ok(())
}
