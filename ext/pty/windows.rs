// Copyright 2018-2026 the Deno authors. MIT license.

//! The Windows pseudo-terminal: a ConPTY (`CreatePseudoConsole`, Windows 10
//! 1809 and later, looked up at run time so an older Windows gets a
//! `NotSupported` error instead of a binary that does not load) over two
//! pipes, read and written through tokio's named-pipe support; the child
//! started by `CreateProcessW` with the pseudoconsole attribute and waited
//! on through `RegisterWaitForSingleObject` (the thread pool's wait, no
//! thread of our own). Closing the pseudoconsole ends the child and, once
//! its output has been drained, the output pipe.

use std::ffi::OsStr;
use std::ffi::OsString;
use std::ffi::c_void;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::os::windows::io::FromRawHandle;
use std::os::windows::io::IntoRawHandle;
use std::os::windows::io::OwnedHandle;
use std::rc::Rc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use deno_process::PreparedRun;
use deno_process::ProcessError;
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::sync::oneshot;
use windows_sys::Win32::Foundation::BOOLEAN;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
use windows_sys::Win32::Foundation::FALSE;
use windows_sys::Win32::Foundation::GENERIC_READ;
use windows_sys::Win32::Foundation::GENERIC_WRITE;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::CreateFileW;
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_FIRST_PIPE_INSTANCE;
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OVERLAPPED;
use windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING;
use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_INBOUND;
use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_OUTBOUND;
use windows_sys::Win32::System::Console::COORD;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::LibraryLoader::GetProcAddress;
use windows_sys::Win32::System::Pipes::CreateNamedPipeW;
use windows_sys::Win32::System::Pipes::PIPE_READMODE_BYTE;
use windows_sys::Win32::System::Pipes::PIPE_REJECT_REMOTE_CLIENTS;
use windows_sys::Win32::System::Pipes::PIPE_TYPE_BYTE;
use windows_sys::Win32::System::Pipes::PIPE_WAIT;
use windows_sys::Win32::System::Threading::CREATE_UNICODE_ENVIRONMENT;
use windows_sys::Win32::System::Threading::CreateProcessW;
use windows_sys::Win32::System::Threading::DeleteProcThreadAttributeList;
use windows_sys::Win32::System::Threading::EXTENDED_STARTUPINFO_PRESENT;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::System::Threading::GetExitCodeProcess;
use windows_sys::Win32::System::Threading::INFINITE;
use windows_sys::Win32::System::Threading::InitializeProcThreadAttributeList;
use windows_sys::Win32::System::Threading::LPPROC_THREAD_ATTRIBUTE_LIST;
use windows_sys::Win32::System::Threading::PROCESS_INFORMATION;
use windows_sys::Win32::System::Threading::RegisterWaitForSingleObject;
use windows_sys::Win32::System::Threading::STARTUPINFOEXW;
use windows_sys::Win32::System::Threading::TerminateProcess;
use windows_sys::Win32::System::Threading::UnregisterWaitEx;
use windows_sys::Win32::System::Threading::UpdateProcThreadAttribute;
use windows_sys::Win32::System::Threading::WT_EXECUTEINWAITTHREAD;
use windows_sys::Win32::System::Threading::WT_EXECUTEONLYONCE;

use crate::Exit;
use crate::PtyError;

/// `ProcThreadAttributeValue(22, FALSE, TRUE, FALSE)`.
const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: usize = 0x0002_0016;
/// `GetExitCodeProcess` for a process that is still running.
const STILL_ACTIVE: u32 = 259;

type HPCON = *mut c_void;
type CreatePseudoConsoleFn = unsafe extern "system" fn(
  size: COORD,
  input: HANDLE,
  output: HANDLE,
  flags: u32,
  pseudoconsole: *mut HPCON,
) -> i32;
type ResizePseudoConsoleFn =
  unsafe extern "system" fn(pseudoconsole: HPCON, size: COORD) -> i32;
type ClosePseudoConsoleFn = unsafe extern "system" fn(pseudoconsole: HPCON);

/// The ConPTY entry points of this machine's kernel32, or none.
#[derive(Clone, Copy)]
struct ConPtyApi {
  create: CreatePseudoConsoleFn,
  resize: ResizePseudoConsoleFn,
  close: ClosePseudoConsoleFn,
}

fn conpty_api() -> Result<ConPtyApi, PtyError> {
  fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
  }
  // SAFETY: kernel32 is always loaded; the symbols are looked up by name and
  // only used with their documented signatures.
  unsafe {
    let kernel32 = GetModuleHandleW(wide("kernel32.dll").as_ptr());
    if kernel32.is_null() {
      return Err(not_supported());
    }
    let create = GetProcAddress(kernel32, c"CreatePseudoConsole".as_ptr() as _);
    let resize = GetProcAddress(kernel32, c"ResizePseudoConsole".as_ptr() as _);
    let close = GetProcAddress(kernel32, c"ClosePseudoConsole".as_ptr() as _);
    match (create, resize, close) {
      (Some(create), Some(resize), Some(close)) => Ok(ConPtyApi {
        create: std::mem::transmute::<
          unsafe extern "system" fn() -> isize,
          CreatePseudoConsoleFn,
        >(create),
        resize: std::mem::transmute::<
          unsafe extern "system" fn() -> isize,
          ResizePseudoConsoleFn,
        >(resize),
        close: std::mem::transmute::<
          unsafe extern "system" fn() -> isize,
          ClosePseudoConsoleFn,
        >(close),
      }),
      _ => Err(not_supported()),
    }
  }
}

fn not_supported() -> PtyError {
  PtyError::NotSupported(
    "Pseudo-terminals need ConPTY, which this Windows does not have (Windows 10 version 1809 or later)"
      .to_string(),
  )
}

/// A console size: `COORD` is `i16`, so a dimension past `i16::MAX` is
/// clamped.
fn coord(cols: u16, rows: u16) -> COORD {
  COORD {
    X: cols.min(i16::MAX as u16) as i16,
    Y: rows.min(i16::MAX as u16) as i16,
  }
}

fn hresult(rc: i32) -> io::Result<()> {
  if rc < 0 {
    // the low word of a Win32 HRESULT is the error code
    Err(io::Error::from_raw_os_error(rc & 0xffff))
  } else {
    Ok(())
  }
}

/// The pseudoconsole handle, closed once: on the child's exit (`lib.rs`,
/// after the wait) or when the last of the master and the child resource
/// drops. The close runs on a thread of its own because
/// `ClosePseudoConsole` waits for conhost, which may still be delivering
/// output to the pipe this thread reads.
struct PseudoConsole {
  api: ConPtyApi,
  hpc: std::cell::Cell<HPCON>,
}

struct SendHpcon(HPCON);
// SAFETY: a ConPTY handle may be closed from any thread.
unsafe impl Send for SendHpcon {}

impl PseudoConsole {
  fn close(&self) {
    let hpc = self.hpc.replace(std::ptr::null_mut());
    if hpc.is_null() {
      return;
    }
    let api = self.api;
    let hpc = SendHpcon(hpc);
    std::thread::spawn(move || {
      let hpc = hpc;
      // SAFETY: the handle was created by CreatePseudoConsole and is closed
      // exactly once.
      unsafe { (api.close)(hpc.0) };
    });
  }
}

impl Drop for PseudoConsole {
  fn drop(&mut self) {
    self.close();
  }
}

pub struct Pty {
  console: Rc<PseudoConsole>,
}

pub struct ReadHalf(NamedPipeServer);
pub struct WriteHalf(NamedPipeServer);

pub struct Waiter {
  process: Rc<OwnedHandle>,
  console: Rc<PseudoConsole>,
  waiting: Option<Waiting>,
}

pub struct Killer {
  process: Rc<OwnedHandle>,
}

pub struct Spawned {
  pub pid: u32,
  pub pty: Pty,
  pub reader: ReadHalf,
  pub writer: WriteHalf,
  pub waiter: Waiter,
  pub killer: Killer,
}

static PIPE_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A pipe whose end for this process is opened overlapped (for tokio); the
/// other end, synchronous, goes to the pseudoconsole. `ours_readable` picks
/// which direction the data flows.
fn overlapped_pipe(
  ours_readable: bool,
) -> io::Result<(OwnedHandle, OwnedHandle)> {
  const PIPE_BUFFER_CAPACITY: u32 = 64 * 1024;
  let mut tries = 0;
  loop {
    tries += 1;
    let name = format!(
      r"\\.\pipe\__deno_pty_{}_{}",
      // SAFETY: no preconditions.
      unsafe { GetCurrentProcessId() },
      PIPE_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let wide_name: Vec<u16> =
      OsStr::new(&name).encode_wide().chain(Some(0)).collect();
    let mut flags = FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED;
    flags |= if ours_readable {
      PIPE_ACCESS_INBOUND
    } else {
      PIPE_ACCESS_OUTBOUND
    };
    // SAFETY: Win32 call with a NUL-terminated name.
    let ours = unsafe {
      CreateNamedPipeW(
        wide_name.as_ptr(),
        flags,
        PIPE_TYPE_BYTE
          | PIPE_READMODE_BYTE
          | PIPE_WAIT
          | PIPE_REJECT_REMOTE_CLIENTS,
        1,
        PIPE_BUFFER_CAPACITY,
        PIPE_BUFFER_CAPACITY,
        0,
        std::ptr::null(),
      )
    };
    if ours == INVALID_HANDLE_VALUE {
      // SAFETY: no preconditions.
      let error = unsafe { GetLastError() };
      if tries < 10 && error == ERROR_ACCESS_DENIED {
        continue;
      }
      return Err(io::Error::from_raw_os_error(error as i32));
    }
    // SAFETY: a handle this function owns.
    let ours = unsafe { OwnedHandle::from_raw_handle(ours as _) };
    let access = if ours_readable {
      GENERIC_WRITE
    } else {
      GENERIC_READ
    };
    // SAFETY: Win32 call with a NUL-terminated name.
    let theirs = unsafe {
      CreateFileW(
        wide_name.as_ptr(),
        access,
        0,
        std::ptr::null(),
        OPEN_EXISTING,
        0,
        std::ptr::null_mut(),
      )
    };
    if theirs == INVALID_HANDLE_VALUE {
      return Err(io::Error::last_os_error());
    }
    // SAFETY: a handle this function owns.
    let theirs = unsafe { OwnedHandle::from_raw_handle(theirs as _) };
    return Ok((ours, theirs));
  }
}

/// `std`'s quoting of one argument for `CreateProcessW` (what the C runtime's
/// and `CommandLineToArgvW`'s parsing undo).
fn append_arg(line: &mut Vec<u16>, arg: &OsStr, force_quotes: bool) {
  let wide: Vec<u16> = arg.encode_wide().collect();
  let quote = force_quotes
    || wide.is_empty()
    || wide.iter().any(|&c| c == b' ' as u16 || c == b'\t' as u16);
  if quote {
    line.push(b'"' as u16);
  }
  let mut backslashes = 0;
  for &c in &wide {
    if c == b'\\' as u16 {
      backslashes += 1;
    } else {
      if c == b'"' as u16 {
        line.extend(std::iter::repeat_n(b'\\' as u16, backslashes + 1));
      }
      backslashes = 0;
    }
    line.push(c);
  }
  if quote {
    line.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
    line.push(b'"' as u16);
  }
}

fn command_line(argv0: &OsStr, args: &[String]) -> Vec<u16> {
  let mut line = Vec::new();
  append_arg(&mut line, argv0, true);
  for arg in args {
    line.push(b' ' as u16);
    append_arg(&mut line, OsStr::new(arg), false);
  }
  line.push(0);
  line
}

/// The environment block: `k=v\0`... `\0`, sorted as Windows keeps it.
fn environment_block(envs: &[(OsString, OsString)]) -> io::Result<Vec<u16>> {
  let mut sorted: Vec<&(OsString, OsString)> = envs.iter().collect();
  sorted.sort_by_cached_key(|(k, _)| k.to_ascii_uppercase());
  let mut block = Vec::new();
  for (k, v) in sorted {
    for part in [k.as_os_str(), v.as_os_str()] {
      if part.encode_wide().any(|c| c == 0) {
        return Err(io::Error::new(
          io::ErrorKind::InvalidInput,
          "nul byte found in provided data",
        ));
      }
    }
    block.extend(k.encode_wide());
    block.push(b'=' as u16);
    block.extend(v.encode_wide());
    block.push(0);
  }
  block.push(0);
  Ok(block)
}

/// A `PROC_THREAD_ATTRIBUTE_LIST` with the pseudoconsole attribute.
struct AttributeList {
  buffer: Vec<u8>,
}

impl AttributeList {
  fn new(hpc: HPCON) -> io::Result<Self> {
    let mut size = 0usize;
    // SAFETY: the sizing call, which fails by design.
    unsafe {
      InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
    }
    let mut buffer = vec![0u8; size];
    let list = buffer.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
    // SAFETY: `buffer` is as large as the sizing call asked for.
    if unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) }
      == FALSE
    {
      return Err(io::Error::last_os_error());
    }
    let mut hpc = hpc;
    // SAFETY: the attribute's value is the handle itself, which lives as long
    // as the console does.
    if unsafe {
      UpdateProcThreadAttribute(
        list,
        0,
        PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
        &mut hpc as *mut HPCON as *mut c_void,
        std::mem::size_of::<HPCON>(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
      )
    } == FALSE
    {
      let err = io::Error::last_os_error();
      // SAFETY: initialized above.
      unsafe { DeleteProcThreadAttributeList(list) };
      return Err(err);
    }
    Ok(Self { buffer })
  }

  fn as_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
    self.buffer.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST
  }
}

impl Drop for AttributeList {
  fn drop(&mut self) {
    // SAFETY: initialized in `new`.
    unsafe { DeleteProcThreadAttributeList(self.as_ptr()) };
  }
}

pub fn spawn(
  run: PreparedRun,
  args: &[String],
  cols: u16,
  rows: u16,
) -> Result<Spawned, PtyError> {
  let api = conpty_api()?;
  // the console reads the child's input from `input_theirs` and writes its
  // output to `output_theirs`; this process keeps the overlapped ends
  let (output_ours, output_theirs) = overlapped_pipe(true)?;
  let (input_ours, input_theirs) = overlapped_pipe(false)?;
  let size = coord(cols, rows);
  let mut hpc: HPCON = std::ptr::null_mut();
  // SAFETY: the pipe ends are open handles.
  hresult(unsafe {
    (api.create)(
      size,
      input_theirs.as_raw_handle() as HANDLE,
      output_theirs.as_raw_handle() as HANDLE,
      0,
      &mut hpc,
    )
  })?;
  let console = Rc::new(PseudoConsole {
    api,
    hpc: std::cell::Cell::new(hpc),
  });
  // conhost holds its own references to the pipe ends
  drop(input_theirs);
  drop(output_theirs);

  let mut attributes = AttributeList::new(hpc)?;
  let program: Vec<u16> = run
    .program
    .as_os_str()
    .encode_wide()
    .chain(Some(0))
    .collect();
  let mut line = command_line(run.argv0.as_os_str(), args);
  let mut environment = environment_block(&run.envs)?;
  let cwd: Vec<u16> =
    run.cwd.as_os_str().encode_wide().chain(Some(0)).collect();
  // SAFETY: a plain C struct, the fields it needs set below.
  let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
  startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
  startup.lpAttributeList = attributes.as_ptr();
  // SAFETY: a plain C struct that CreateProcessW fills.
  let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
  // SAFETY: every pointer names a NUL-terminated buffer that outlives the
  // call; no handle is inherited (the console attribute carries the pty).
  let ok = unsafe {
    CreateProcessW(
      program.as_ptr(),
      line.as_mut_ptr(),
      std::ptr::null(),
      std::ptr::null(),
      FALSE,
      EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
      environment.as_mut_ptr() as *mut c_void,
      cwd.as_ptr(),
      &mut startup.StartupInfo,
      &mut info,
    )
  };
  if ok == FALSE {
    let err = io::Error::last_os_error();
    return Err(
      ProcessError::SpawnFailed {
        command: run.argv0.to_string_lossy().into_owned(),
        error: Box::new(ProcessError::Io(err)),
      }
      .into(),
    );
  }
  // SAFETY: CreateProcessW gave this process both handles.
  unsafe {
    CloseHandle(info.hThread);
  }
  // SAFETY: as above; the handle is owned from here.
  let process =
    Rc::new(unsafe { OwnedHandle::from_raw_handle(info.hProcess as _) });
  let pid = info.dwProcessId;
  run.verify_after_spawn(|| {
    // SAFETY: a process handle this process owns.
    unsafe {
      TerminateProcess(process.as_raw_handle() as HANDLE, 1);
    }
  })?;

  // SAFETY: both are the overlapped server ends created above, handed over
  // whole.
  let reader =
    unsafe { NamedPipeServer::from_raw_handle(output_ours.into_raw_handle())? };
  // SAFETY: as above.
  let writer =
    unsafe { NamedPipeServer::from_raw_handle(input_ours.into_raw_handle())? };
  Ok(Spawned {
    pid,
    pty: Pty {
      console: console.clone(),
    },
    reader: ReadHalf(reader),
    writer: WriteHalf(writer),
    waiter: Waiter {
      process: process.clone(),
      console,
      waiting: None,
    },
    killer: Killer { process },
  })
}

impl Pty {
  pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
    let hpc = self.console.hpc.get();
    if hpc.is_null() {
      return Err(io::Error::new(
        io::ErrorKind::BrokenPipe,
        "the pseudoconsole is closed",
      ));
    }
    // SAFETY: an open pseudoconsole handle.
    hresult(unsafe { (self.console.api.resize)(hpc, coord(cols, rows)) })
  }

  pub fn foreground_process(&self) -> Option<String> {
    None
  }
}

impl tokio::io::AsyncRead for ReadHalf {
  fn poll_read(
    mut self: std::pin::Pin<&mut Self>,
    cx: &mut std::task::Context<'_>,
    buf: &mut tokio::io::ReadBuf<'_>,
  ) -> std::task::Poll<io::Result<()>> {
    match std::pin::Pin::new(&mut self.0).poll_read(cx, buf) {
      // the console went away with the child: the terminal's end-of-file
      std::task::Poll::Ready(Err(err))
        if err.kind() == io::ErrorKind::BrokenPipe =>
      {
        std::task::Poll::Ready(Ok(()))
      }
      other => other,
    }
  }
}

impl tokio::io::AsyncWrite for WriteHalf {
  fn poll_write(
    mut self: std::pin::Pin<&mut Self>,
    cx: &mut std::task::Context<'_>,
    buf: &[u8],
  ) -> std::task::Poll<io::Result<usize>> {
    std::pin::Pin::new(&mut self.0).poll_write(cx, buf)
  }

  fn poll_flush(
    mut self: std::pin::Pin<&mut Self>,
    cx: &mut std::task::Context<'_>,
  ) -> std::task::Poll<io::Result<()>> {
    std::pin::Pin::new(&mut self.0).poll_flush(cx)
  }

  fn poll_shutdown(
    self: std::pin::Pin<&mut Self>,
    _cx: &mut std::task::Context<'_>,
  ) -> std::task::Poll<io::Result<()>> {
    // A terminal has no half-close; the pipe closes with the resource.
    std::task::Poll::Ready(Ok(()))
  }
}

/// A registered wait on the process handle, delivered through a oneshot.
struct Waiting {
  rx: oneshot::Receiver<()>,
  wait_object: HANDLE,
  tx: *mut Option<oneshot::Sender<()>>,
}

impl Drop for Waiting {
  fn drop(&mut self) {
    // SAFETY: unregisters the wait and blocks until any running callback has
    // returned, so the box is ours to free afterwards.
    unsafe {
      UnregisterWaitEx(self.wait_object, INVALID_HANDLE_VALUE);
      drop(Box::from_raw(self.tx));
    }
  }
}

unsafe extern "system" fn wait_callback(
  ptr: *mut c_void,
  _timer_fired: BOOLEAN,
) {
  // SAFETY: the box registered with the wait, alive until `Waiting` drops.
  let complete = unsafe { &mut *(ptr as *mut Option<oneshot::Sender<()>>) };
  if let Some(tx) = complete.take() {
    let _ = tx.send(());
  }
}

fn exit_code(process: &OwnedHandle) -> io::Result<Option<u32>> {
  let mut code = 0u32;
  // SAFETY: an open process handle.
  if unsafe { GetExitCodeProcess(process.as_raw_handle() as HANDLE, &mut code) }
    == FALSE
  {
    return Err(io::Error::last_os_error());
  }
  Ok((code != STILL_ACTIVE).then_some(code))
}

impl Waiter {
  pub async fn wait(&mut self) -> io::Result<Exit> {
    let code = loop {
      if let Some(code) = exit_code(&self.process)? {
        break code;
      }
      if self.waiting.is_none() {
        let (tx, rx) = oneshot::channel();
        let tx = Box::into_raw(Box::new(Some(tx)));
        let mut wait_object: HANDLE = std::ptr::null_mut();
        // SAFETY: the thread pool calls `wait_callback` once with the box,
        // which `Waiting` frees after unregistering.
        let rc = unsafe {
          RegisterWaitForSingleObject(
            &mut wait_object,
            self.process.as_raw_handle() as HANDLE,
            Some(wait_callback),
            tx as *mut c_void,
            INFINITE,
            WT_EXECUTEINWAITTHREAD | WT_EXECUTEONLYONCE,
          )
        };
        if rc == FALSE {
          let err = io::Error::last_os_error();
          // SAFETY: never registered, so still ours.
          drop(unsafe { Box::from_raw(tx) });
          return Err(err);
        }
        self.waiting = Some(Waiting {
          rx,
          wait_object,
          tx,
        });
      }
      let waiting = self.waiting.as_mut().expect("set above");
      let _ = (&mut waiting.rx).await;
      self.waiting = None;
    };
    // the child is gone: let conhost flush and close the output pipe
    self.console.close();
    Ok(Exit {
      code: Some(code as i32),
      signal: None,
    })
  }
}

impl Killer {
  /// Ends the child (any signal but 0, which only checks that it runs), as
  /// `Deno.kill` does on Windows.
  pub fn kill(&self, signal: i32) -> Result<(), PtyError> {
    if exit_code(&self.process)?.is_some() {
      return Err(
        io::Error::new(io::ErrorKind::NotFound, "No such process").into(),
      );
    }
    if signal == 0 {
      return Ok(());
    }
    // SAFETY: a process handle this process owns.
    if unsafe { TerminateProcess(self.process.as_raw_handle() as HANDLE, 1) }
      == FALSE
    {
      return Err(io::Error::last_os_error().into());
    }
    Ok(())
  }

  pub fn kill_on_drop(&self) {
    // SAFETY: best-effort end of a process handle this process owns.
    unsafe {
      TerminateProcess(self.process.as_raw_handle() as HANDLE, 1);
    }
  }
}
