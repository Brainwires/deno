// Copyright 2018-2026 the Deno authors. MIT license.

//! `Deno.openPty`: a child process on a pseudo-terminal (DENEXT.md item 290).
//!
//! The program is resolved and its run permission checked exactly as
//! `Deno.Command` does it (`deno_process::prepare_run`: `--allow-run` and
//! its allow-list, the `LD_*` / `DYLD_*` rule, an embedded program's hash).
//! Unix opens a pty pair and gives the child the slave as its controlling
//! terminal (`unix.rs`); Windows a ConPTY (`windows.rs`). The master is one
//! resource, read by `readable` and written by `writable`; the child is
//! another, waited on by `status` and the only thing `kill` can reach. The
//! JavaScript side is `01_pty.js`.

use std::borrow::Cow;
use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;

use deno_core::AsyncRefCell;
use deno_core::AsyncResult;
use deno_core::CancelHandle;
use deno_core::CancelTryFuture;
use deno_core::OpState;
use deno_core::RcRef;
use deno_core::Resource;
use deno_core::ResourceId;
use deno_core::op2;
use deno_error::JsErrorBox;
use deno_process::ProcessError;
use serde::Deserialize;
use serde::Serialize;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;

deno_core::extension!(
  deno_pty,
  deps = [deno_web, deno_process],
  ops = [
    op_pty_spawn,
    op_pty_wait,
    op_pty_kill,
    op_pty_resize,
    op_pty_foreground_process,
  ],
  lazy_loaded_js = ["01_pty.js"],
);

#[derive(Debug, thiserror::Error, deno_error::JsError)]
pub enum PtyError {
  #[class(inherit)]
  #[error(transparent)]
  Process(#[from] ProcessError),
  #[class(inherit)]
  #[error(transparent)]
  Io(#[from] std::io::Error),
  #[class(inherit)]
  #[error(transparent)]
  Resource(#[from] deno_core::error::ResourceError),
  #[class(inherit)]
  #[error(transparent)]
  Signal(#[from] deno_os::SignalError),
  #[class(type)]
  #[error("Child process has already terminated.")]
  AlreadyTerminated,
  #[class("NotSupported")]
  #[error("{0}")]
  NotSupported(String),
  #[class(inherit)]
  #[error(transparent)]
  Other(#[from] JsErrorBox),
}

/// How the child ended: an exit code, or on Unix the signal that ended it.
pub struct Exit {
  pub code: Option<i32>,
  pub signal: Option<i32>,
}

/// A signal as `Deno.kill` takes it: a name or a number.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum SignalArg {
  Int(i32),
  String(String),
}

impl SignalArg {
  fn number(&self) -> Result<i32, PtyError> {
    match self {
      SignalArg::Int(n) => Ok(*n),
      SignalArg::String(s) => Ok(
        deno_signals::signal_str_to_int(s)
          .map_err(deno_os::SignalError::InvalidSignalStr)?,
      ),
    }
  }
}

/// The terminal's master side: the stream the child's output arrives on and
/// its input goes out on, plus the window size. Closed by JavaScript when
/// `readable` ends or the handle is disposed; a pending read or write is
/// cancelled then.
struct PtyMasterResource {
  rd: AsyncRefCell<platform::ReadHalf>,
  wr: AsyncRefCell<platform::WriteHalf>,
  pty: platform::Pty,
  read_cancel: CancelHandle,
  write_cancel: CancelHandle,
}

impl PtyMasterResource {
  async fn read(
    self: Rc<Self>,
    data: &mut [u8],
  ) -> Result<usize, std::io::Error> {
    let mut rd = RcRef::map(&self, |r| &r.rd).borrow_mut().await;
    rd.read(data)
      .try_or_cancel(RcRef::map(&self, |r| &r.read_cancel))
      .await
  }

  async fn write(self: Rc<Self>, data: &[u8]) -> Result<usize, std::io::Error> {
    let mut wr = RcRef::map(&self, |r| &r.wr).borrow_mut().await;
    wr.write(data)
      .try_or_cancel(RcRef::map(&self, |r| &r.write_cancel))
      .await
  }
}

impl Resource for PtyMasterResource {
  deno_core::impl_readable_byob!();
  deno_core::impl_writable!();

  fn name(&self) -> Cow<'_, str> {
    "pty".into()
  }

  fn close(self: Rc<Self>) {
    self.read_cancel.cancel();
    self.write_cancel.cancel();
  }

  fn cancel_read_ops(self: Rc<Self>) {
    self.read_cancel.cancel();
  }
}

/// The child: waited on once (`op_pty_wait`, which removes the resource),
/// signalled by `op_pty_kill` while it has not been reaped (a reaped pid may
/// already name another process). Dropped unreaped (the runtime ends with the
/// child running) it is killed, as a `Deno.Command` child is.
struct PtyChildResource {
  waiter: AsyncRefCell<platform::Waiter>,
  killer: platform::Killer,
  reaped: Cell<bool>,
}

impl Resource for PtyChildResource {
  fn name(&self) -> Cow<'_, str> {
    "ptyChild".into()
  }
}

impl Drop for PtyChildResource {
  fn drop(&mut self) {
    if !self.reaped.get() {
      self.killer.kill_on_drop();
    }
  }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnArgs {
  cmd: String,
  args: Vec<String>,
  cwd: Option<String>,
  clear_env: bool,
  env: Vec<(String, String)>,
  cols: u16,
  rows: u16,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Spawned {
  pid: u32,
  master_rid: ResourceId,
  child_rid: ResourceId,
}

/// The status `Deno.ChildProcess` reports: on a signal, `code` is 128 + the
/// signal's number and `signal` its name.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildStatus {
  success: bool,
  code: i32,
  signal: Option<String>,
}

impl ChildStatus {
  fn from_exit(exit: Exit) -> Result<Self, PtyError> {
    Ok(match exit {
      Exit {
        signal: Some(signal),
        ..
      } => ChildStatus {
        success: false,
        code: 128 + signal,
        signal: Some(
          deno_signals::signal_int_to_str(signal)
            .map_err(deno_os::SignalError::InvalidSignalInt)?
            .to_string(),
        ),
      },
      Exit { code, .. } => {
        let code = code.unwrap_or(-1);
        ChildStatus {
          success: code == 0,
          code,
          signal: None,
        }
      }
    })
  }
}

#[op2(stack_trace)]
#[serde]
fn op_pty_spawn(
  state: &mut OpState,
  #[serde] args: SpawnArgs,
  #[string] api_name: String,
) -> Result<Spawned, PtyError> {
  // The child is handed fds 0-2 (the slave), nothing above: an embedded
  // program's verified handle is numbered from 3 (DENEXT.md item 232).
  let run = deno_process::prepare_run(
    state,
    &args.cmd,
    args.cwd.as_deref(),
    &args.env,
    args.clear_env,
    &api_name,
    3,
  )?;
  let spawned = platform::spawn(run, &args.args, args.cols, args.rows)?;
  let pid = spawned.pid;
  let master_rid = state.resource_table.add(PtyMasterResource {
    rd: spawned.reader.into(),
    wr: spawned.writer.into(),
    pty: spawned.pty,
    read_cancel: Default::default(),
    write_cancel: Default::default(),
  });
  let child_rid = state.resource_table.add(PtyChildResource {
    waiter: spawned.waiter.into(),
    killer: spawned.killer,
    reaped: Cell::new(false),
  });
  Ok(Spawned {
    pid,
    master_rid,
    child_rid,
  })
}

#[op2]
#[serde]
#[allow(
  clippy::await_holding_refcell_ref,
  reason = "ref is dropped before await points"
)]
async fn op_pty_wait(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
) -> Result<ChildStatus, PtyError> {
  let resource = state
    .borrow_mut()
    .resource_table
    .get::<PtyChildResource>(rid)?;
  let exit = {
    let mut waiter = RcRef::map(&resource, |r| &r.waiter).borrow_mut().await;
    waiter.wait().await?
  };
  resource.reaped.set(true);
  let status = ChildStatus::from_exit(exit)?;
  if let Ok(resource) = state.borrow_mut().resource_table.take_any(rid) {
    resource.close();
  }
  Ok(status)
}

#[op2]
fn op_pty_kill(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[serde] signal: SignalArg,
) -> Result<(), PtyError> {
  let Ok(resource) = state.resource_table.get::<PtyChildResource>(rid) else {
    return Err(PtyError::AlreadyTerminated);
  };
  if resource.reaped.get() {
    return Err(PtyError::AlreadyTerminated);
  }
  resource.killer.kill(signal.number()?)
}

#[op2(fast)]
fn op_pty_resize(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  cols: u16,
  rows: u16,
) -> Result<(), PtyError> {
  let resource = state.resource_table.get::<PtyMasterResource>(rid)?;
  resource.pty.resize(cols, rows)?;
  Ok(())
}

/// The name of the terminal's foreground process (what `node-pty`'s
/// `process` reports), when the platform can tell.
#[op2]
#[string]
fn op_pty_foreground_process(
  state: &mut OpState,
  #[smi] rid: ResourceId,
) -> Result<Option<String>, PtyError> {
  let resource = state.resource_table.get::<PtyMasterResource>(rid)?;
  Ok(resource.pty.foreground_process())
}
