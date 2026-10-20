// Copyright 2018-2026 the Deno authors. MIT license.

use std::collections::HashMap;
#[cfg(unix)]
use std::io::ErrorKind;
#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::sync::OnceLock;
#[cfg(unix)]
use std::sync::atomic::AtomicI32;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;

use signal_hook::consts::*;
use tokio::sync::watch;

mod dict;
pub use dict::*;

#[cfg(windows)]
static SIGHUP: i32 = 1;
#[cfg(windows)]
static SIGWINCH: i32 = 28;

static COUNTER: AtomicU32 = AtomicU32::new(0);

// The single terminating signal number the OS signal thread should consume once
// (instead of running the exit callbacks and the OS default action), or 0 when
// nothing is suppressed. The file watcher arms this for the duration of a
// graceful restart, where a JS signal handler may unregister itself and re-raise
// a real OS signal (the `signal-exit` "unload + re-raise" pattern used by Vite
// 8 / rolldown) that would otherwise kill the watcher instead of restarting it.
// See https://github.com/denoland/deno/issues/35942.
#[cfg(unix)]
static SUPPRESSED_SIGNAL: AtomicI32 = AtomicI32::new(0);

/// Arm one-shot suppression of the OS default action for `signal` and return a
/// guard that disarms on drop.
///
/// A graceful restart re-raises exactly one signal after unregistering its JS
/// handler; only that single matching signal, sent by this process to itself,
/// is consumed, so a genuine external signal (e.g. `kill <pid>` from a
/// supervisor) in the same window still terminates, whether it arrives before
/// or after the re-raise. Signals other than `signal` — notably a real Ctrl+C —
/// are never suppressed.
///
/// This only works because the re-raise happens synchronously inside the signal
/// handler thread. A signal that arrives after the guard is dropped, or a
/// library that re-raises after an `await`, lands with the flag already cleared
/// and still terminates — this is a best-effort window, not a hard guarantee.
///
/// The mechanism is a no-op on non-unix targets: the bug only reproduces with a
/// real OS re-raise, which does not exist on Windows (`raise()` is synthetic
/// there).
pub fn suppress_default_exit_for(signal: i32) -> SuppressDefaultExitGuard {
  #[cfg(unix)]
  SUPPRESSED_SIGNAL.store(signal, Ordering::Relaxed);
  #[cfg(not(unix))]
  let _ = signal;
  SuppressDefaultExitGuard(())
}

/// Disarms the suppression armed by [`suppress_default_exit_for`] on drop.
pub struct SuppressDefaultExitGuard(());

impl Drop for SuppressDefaultExitGuard {
  fn drop(&mut self) {
    #[cfg(unix)]
    SUPPRESSED_SIGNAL.store(0, Ordering::Relaxed);
  }
}

// How many times each signal arrived from another process or the kernel (the
// OS signal thread) or, on Windows, from the console control handler, indexed
// by signal number. `raise()` is synthetic and is not counted, and neither is a
// signal the process sent itself. The file watcher uses this to tell a
// `kill <pid>` from its own restart SIGTERM and from a program's
// `process.kill(process.pid)`.
const DELIVERY_COUNTS_LEN: usize = 65;
static DELIVERY_COUNTS: [AtomicU32; DELIVERY_COUNTS_LEN] =
  [const { AtomicU32::new(0) }; DELIVERY_COUNTS_LEN];

fn count_delivery(signal: i32) {
  if let Some(count) = usize::try_from(signal)
    .ok()
    .and_then(|index| DELIVERY_COUNTS.get(index))
  {
    count.fetch_add(1, Ordering::Relaxed);
  }
}

/// The number of times `signal` has been delivered to the process from
/// outside since startup: by another process or the kernel, or as a console
/// control event on Windows. Synthetic [`raise`] calls and signals the process
/// sent itself are not counted. Only signals Deno listens for (a registered
/// handler, or SIGHUP / SIGTERM / SIGINT once any handler is registered) are
/// observed, and the count is updated before the handlers run.
pub fn delivery_count(signal: i32) -> u32 {
  usize::try_from(signal)
    .ok()
    .and_then(|index| DELIVERY_COUNTS.get(index))
    .map(|count| count.load(Ordering::Relaxed))
    .unwrap_or(0)
}

#[cfg(unix)]
fn is_terminating_signal(signal: i32) -> bool {
  signal == SIGHUP || signal == SIGTERM || signal == SIGINT
}

type Handler = Box<dyn Fn() + Send>;
type Handlers = HashMap<i32, Vec<(u32, bool, Handler)>>;
static HANDLERS: OnceLock<(Handle, Mutex<Handlers>)> = OnceLock::new();

#[cfg(unix)]
struct Handle(signal_hook::iterator::Handle);

#[cfg(windows)]
struct Handle;

fn handle_signal(signal: i32) -> bool {
  let Some((_, handlers)) = HANDLERS.get() else {
    return false;
  };
  let handlers = handlers.lock().unwrap();
  let Some(handlers) = handlers.get(&signal) else {
    return false;
  };

  let mut handled = false;

  for (_, prevent_default, f) in handlers {
    if *prevent_default {
      handled = true;
    }

    f();
  }

  handled
}

#[cfg(unix)]
fn init() -> Handle {
  use signal_hook::iterator::backend::SignalDelivery;
  use signal_hook::iterator::exfiltrator::WithRawSiginfo;

  fn has_signals(read: &mut UnixStream) -> Result<bool, std::io::Error> {
    loop {
      match read.read(&mut [0_u8]) {
        Ok(num_read) => break Ok(num_read > 0),
        Err(error) => {
          if error.kind() != ErrorKind::Interrupted {
            break Err(error);
          }
        }
      }
    }
  }

  // Construct the self-pipe ourselves (rather than letting `Signals::new`
  // create it internally) so the raw fds can be registered as Deno-owned.
  // This prevents user fd APIs such as `net.Socket({ fd })` from adopting
  // them and flipping them to non-blocking mode, which would make
  // signal-hook's reads fail with EAGAIN and panic. See #34054.
  let (read, write) = UnixStream::pair().unwrap();
  deno_io::register_internal_fd(read.as_raw_fd());
  deno_io::register_internal_fd(write.as_raw_fd());

  let mut signals = SignalDelivery::with_pipe(
    read,
    write,
    WithRawSiginfo,
    [SIGHUP, SIGTERM, SIGINT],
  )
  .unwrap();
  let handle = signals.handle();

  std::thread::spawn(move || {
    loop {
      match signals.poll_pending(&mut has_signals) {
        Ok(Some(pending)) => {
          for info in pending {
            let signal = info.si_signo;
            // Whether this process sent the signal to itself (`kill(getpid())`,
            // e.g. `process.kill(process.pid)`), as opposed to another process
            // or the kernel.
            // SAFETY: `info` is the siginfo the kernel filled in for this
            // delivery; reading its sender pid has no other precondition.
            let self_sent = u32::try_from(unsafe { info.si_pid() }).ok()
              == Some(std::process::id());
            if !self_sent {
              count_delivery(signal);
            }
            let handled = handle_signal(signal);
            // A graceful restart (file watcher) may have unregistered the JS
            // handler and re-raised this signal; consume that single re-raise
            // instead of killing the process. Only a signal this process sent
            // itself can be that re-raise: one from another process (a
            // `kill <pid>`, a supervisor) still terminates, even inside the
            // window. `compare_exchange` disarms as it matches, so at most one
            // signal is swallowed. See
            // https://github.com/denoland/deno/issues/35942.
            if !handled
              && self_sent
              && is_terminating_signal(signal)
              && SUPPRESSED_SIGNAL
                .compare_exchange(
                  signal,
                  0,
                  Ordering::Relaxed,
                  Ordering::Relaxed,
                )
                .is_ok()
            {
              continue;
            }
            if !handled {
              if is_terminating_signal(signal) {
                run_exit();
              }
              signal_hook::low_level::emulate_default_handler(signal).unwrap();
            }
          }
        }
        Ok(None) => break,
        Err(error) => {
          panic!("Unexpected error: {error}");
        }
      }
    }
  });

  Handle(handle)
}

#[cfg(windows)]
fn init() -> Handle {
  unsafe extern "system" fn handle(ctrl_type: u32) -> i32 {
    let signal = match ctrl_type {
      0 => SIGINT,
      1 => SIGBREAK,
      2 => SIGHUP,
      5 => SIGTERM,
      6 => SIGTERM,
      _ => return 0,
    };
    count_delivery(signal);
    let handled = handle_signal(signal);
    handled as _
  }

  // SAFETY: Registering handler
  unsafe {
    windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(handle), 1);
  }

  Handle
}

#[cfg(windows)]
fn start_sigwinch_polling() {
  static STARTED: OnceLock<()> = OnceLock::new();
  STARTED.get_or_init(|| {
    std::thread::spawn(|| {
      // SAFETY: Win32 calls to open CONOUT$ and poll console size
      unsafe {
        let conout_name: Vec<u16> =
          "CONOUT$".encode_utf16().chain(Some(0)).collect();
        let handle = windows_sys::Win32::Storage::FileSystem::CreateFileW(
          conout_name.as_ptr(),
          windows_sys::Win32::Foundation::GENERIC_READ,
          windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ
            | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE,
          std::ptr::null(),
          windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING,
          0,
          std::ptr::null_mut(),
        );
        if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
          return;
        }

        let mut prev_cols: i32 = 0;
        let mut prev_rows: i32 = 0;

        // Read initial size
        let mut bufinfo: windows_sys::Win32::System::Console::CONSOLE_SCREEN_BUFFER_INFO =
          std::mem::zeroed();
        if windows_sys::Win32::System::Console::GetConsoleScreenBufferInfo(handle, &mut bufinfo)
          != 0
        {
          prev_cols =
            bufinfo.srWindow.Right as i32 - bufinfo.srWindow.Left as i32 + 1;
          prev_rows =
            bufinfo.srWindow.Bottom as i32 - bufinfo.srWindow.Top as i32 + 1;
        }

        loop {
          windows_sys::Win32::System::Threading::Sleep(250);

          let mut bufinfo: windows_sys::Win32::System::Console::CONSOLE_SCREEN_BUFFER_INFO =
            std::mem::zeroed();
          if windows_sys::Win32::System::Console::GetConsoleScreenBufferInfo(
            handle,
            &mut bufinfo,
          ) == 0
          {
            continue;
          }

          let cols =
            bufinfo.srWindow.Right as i32 - bufinfo.srWindow.Left as i32 + 1;
          let rows =
            bufinfo.srWindow.Bottom as i32 - bufinfo.srWindow.Top as i32 + 1;

          if cols != prev_cols || rows != prev_rows {
            prev_cols = cols;
            prev_rows = rows;
            handle_signal(SIGWINCH);
          }
        }
      }
    });
  });
}

pub fn register(
  signal: i32,
  prevent_default: bool,
  f: Box<dyn Fn() + Send>,
) -> Result<u32, std::io::Error> {
  if is_forbidden(signal) {
    return Err(std::io::Error::other(format!(
      "Refusing to register signal {signal}"
    )));
  }

  let (handle, handlers) = HANDLERS.get_or_init(|| {
    let handle = init();

    let handlers = Mutex::new(HashMap::new());

    (handle, handlers)
  });

  let id = COUNTER.fetch_add(1, Ordering::Relaxed);
  let mut handlers = handlers.lock().unwrap();
  match handlers.entry(signal) {
    std::collections::hash_map::Entry::Occupied(mut v) => {
      v.get_mut().push((id, prevent_default, f))
    }
    std::collections::hash_map::Entry::Vacant(v) => {
      v.insert(vec![(id, prevent_default, f)]);

      #[cfg(unix)]
      {
        handle.0.add_signal(signal).map_err(|e| {
          std::io::Error::other(format!(
            "Failed to register signal {signal}: {e}"
          ))
        })?;
      }
      #[cfg(windows)]
      {
        let _ = handle;
        if signal == SIGWINCH {
          start_sigwinch_polling();
        }
      }
    }
  }

  Ok(id)
}

pub fn unregister(signal: i32, id: u32) {
  let Some((_, handlers)) = HANDLERS.get() else {
    return;
  };
  let mut handlers = handlers.lock().unwrap();
  let Some(handlers) = handlers.get_mut(&signal) else {
    return;
  };
  let Some(index) = handlers.iter().position(|v| v.0 == id) else {
    return;
  };
  let _ = handlers.swap_remove(index);
}

static BEFORE_EXIT: OnceLock<Mutex<Vec<Handler>>> = OnceLock::new();

pub fn before_exit(f: fn()) {
  BEFORE_EXIT
    .get_or_init(|| Mutex::new(vec![]))
    .lock()
    .unwrap()
    .push(Box::new(f));
}

pub fn run_exit() {
  if let Some(fns) = BEFORE_EXIT.get() {
    let fns = fns.lock().unwrap();
    for f in fns.iter() {
      f();
    }
  }
}

pub const SIGINT: i32 = 2;
pub const SIGTERM: i32 = 15;

/// Synthetically raise a signal, triggering all registered JS handlers.
///
/// This does NOT use OS-level signal delivery — it directly invokes the
/// handler functions under a mutex, making it safe to call from any async
/// or sync context on all platforms (including Windows).
///
/// Returns true if any handler prevented the default behavior.
pub fn raise(signal: i32) -> bool {
  handle_signal(signal)
}

pub fn is_forbidden(signo: i32) -> bool {
  if FORBIDDEN.contains(&signo) {
    return true;
  }
  // On Windows, signal_hook's FORBIDDEN list doesn't include SIGKILL/SIGABRT
  // (they're Unix-specific in the crate). Add them here since listening for
  // uncatchable/fatal signals doesn't make sense on any platform.
  #[cfg(windows)]
  if signo == 9 || signo == 22 {
    // SIGKILL (9) and SIGABRT (22, Windows CRT value)
    return true;
  }
  false
}

pub struct SignalStream {
  signo: i32,
  id: u32,
  rx: watch::Receiver<()>,
}

impl SignalStream {
  pub async fn recv(&mut self) -> Option<()> {
    self.rx.changed().await.ok()
  }
}

impl Drop for SignalStream {
  fn drop(&mut self) {
    unregister(self.signo, self.id);
  }
}

pub fn signal_stream(signo: i32) -> Result<SignalStream, std::io::Error> {
  signal_stream_inner(signo, true)
}

/// Like [`signal_stream`], but does not prevent the default signal behavior.
///
/// The stream only observes the signal: if no other registered handler
/// (e.g. a JS signal listener) prevents the default, the default action
/// still runs, terminating the process for signals like SIGINT. This is
/// important for consumers that poll the stream from the main event loop,
/// which may be blocked in synchronous JS execution and unable to react
/// to the notification.
pub fn signal_stream_allow_default(
  signo: i32,
) -> Result<SignalStream, std::io::Error> {
  signal_stream_inner(signo, false)
}

fn signal_stream_inner(
  signo: i32,
  prevent_default: bool,
) -> Result<SignalStream, std::io::Error> {
  let (tx, rx) = watch::channel(());
  let cb = Box::new(move || {
    tx.send_replace(());
  });
  let id = register(signo, prevent_default, cb)?;
  Ok(SignalStream { signo, id, rx })
}

pub async fn ctrl_c() -> std::io::Result<()> {
  ctrl_c_inner(signal_stream(libc::SIGINT)?).await
}

/// Like [`ctrl_c`], but does not prevent the default SIGINT behavior.
/// See [`signal_stream_allow_default`].
pub async fn ctrl_c_allow_default() -> std::io::Result<()> {
  ctrl_c_inner(signal_stream_allow_default(libc::SIGINT)?).await
}

async fn ctrl_c_inner(mut stream: SignalStream) -> std::io::Result<()> {
  match stream.recv().await {
    Some(_) => Ok(()),
    None => Err(std::io::Error::other("failed to receive SIGINT signal")),
  }
}

#[cfg(all(test, unix))]
mod tests {
  use std::time::Duration;
  use std::time::Instant;

  use super::*;

  // Sends `signal` to this process from another process, as `kill <pid>`.
  fn kill_from_another_process(signal: i32) {
    let status = std::process::Command::new("kill")
      .arg(format!("-{signal}"))
      .arg(std::process::id().to_string())
      .status()
      .unwrap();
    assert!(status.success());
  }

  fn kill_self(signal: i32) {
    // SAFETY: signals this process; every caller has made the signal harmless.
    assert_eq!(unsafe { libc::kill(libc::getpid(), signal) }, 0);
  }

  fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
      assert!(Instant::now() < deadline, "timed out waiting for {what}");
      std::thread::sleep(Duration::from_millis(5));
    }
  }

  // The file watcher relies on this to tell an external `kill` from its own
  // synthetic restart signal and from a program signalling itself: only a
  // delivery from another process counts.
  #[test]
  fn delivery_count_counts_only_signals_from_other_processes() {
    let signal = SIGUSR2;
    let (tx, rx) = std::sync::mpsc::channel();
    let id = register(
      signal,
      true,
      Box::new(move || {
        let _ = tx.send(());
      }),
    )
    .unwrap();
    let before = delivery_count(signal);

    assert!(raise(signal));
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(delivery_count(signal), before);

    kill_self(signal);
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(delivery_count(signal), before);

    kill_from_another_process(signal);
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // Counted before the handlers run.
    assert_eq!(delivery_count(signal), before + 1);

    unregister(signal, id);
  }

  // The suppression of a graceful restart consumes the re-raise the process
  // sends itself, and only that: a SIGHUP from another process inside the
  // window is a real request (here a handled one, so the test survives), and
  // does not use up the suppression.
  #[test]
  fn suppression_consumes_only_a_self_sent_reraise() {
    let signal = SIGHUP;
    let (tx, rx) = std::sync::mpsc::channel();
    let id = register(
      signal,
      true,
      Box::new(move || {
        let _ = tx.send(());
      }),
    )
    .unwrap();
    let guard = suppress_default_exit_for(signal);

    // Handled by the listener: the suppression stays armed.
    kill_from_another_process(signal);
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(SUPPRESSED_SIGNAL.load(Ordering::Relaxed), signal);

    // The restart's re-raise comes after the listener unregistered itself.
    unregister(signal, id);
    let before = delivery_count(signal);
    kill_self(signal);
    wait_until("the re-raise to be consumed", || {
      SUPPRESSED_SIGNAL.load(Ordering::Relaxed) == 0
    });
    drop(guard);
    assert_eq!(delivery_count(signal), before);
  }
}
