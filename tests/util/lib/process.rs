// Copyright 2018-2026 the Deno authors. MIT license.
use std::convert::Infallible;

use anyhow::Error;

/// For unix targets, we just replace our current process with the desired cargo process.
#[cfg(unix)]
pub fn exec_replace_inner(
  cmd: &str,
  args: &[&str],
) -> Result<Infallible, Error> {
  use std::ffi::CStr;
  use std::ffi::CString;

  let args = args
    .iter()
    .map(|arg| CString::new(*arg).unwrap())
    .collect::<Vec<_>>();
  let args: Vec<&CStr> =
    args.iter().map(|arg| arg.as_ref()).collect::<Vec<_>>();

  let err = nix::unistd::execvp(&CString::new(cmd).unwrap(), &args)
    .expect_err("Impossible");
  Err(err.into())
}

#[cfg(windows)]
pub fn exec_replace_inner(
  cmd: &str,
  args: &[&str],
) -> Result<Infallible, Error> {
  use std::os::windows::io::AsRawHandle;
  use std::process::Command;

  use win32job::ExtendedLimitInfo;
  use win32job::Job;

  // Use a job to ensure the child process's lifetime does not exceed the current process's lifetime.
  // This ensures that if the current process is terminated (e.g., via ctrl+c or task manager),
  // the child process is automatically reaped.

  // For more information about this technique, see Raymond Chen's blog post:
  // https://devblogs.microsoft.com/oldnewthing/20131209-00/?p=2433
  // Note: While our implementation is not perfect, it serves its purpose for test code.

  // In the future, we may directly obtain the main thread's handle from Rust code and use it
  // to create a suspended process that we can then resume:
  // https://github.com/rust-lang/rust/issues/96723

  // Creates a child process and assigns it to our current job.
  // A more reliable approach would be to create the child suspended and then assign it to the job.
  // For now, we create the child, create the job, and then assign both us and the child to the job.
  let mut child = Command::new(cmd).args(&args[1..]).spawn()?;

  let mut info = ExtendedLimitInfo::default();
  info.limit_kill_on_job_close();
  let job = Job::create_with_limit_info(&info)?;
  job.assign_current_process()?;
  let handle = child.as_raw_handle();
  job.assign_process(handle as _)?;

  let exit = child.wait()?;
  std::process::exit(exit.code().unwrap_or(1));
}

/// Runs a command, replacing the current process on Unix. On Windows, this function blocks and
/// exits.
///
/// In either case, the only way this function returns is if it fails to launch the child
/// process.
pub fn exec_replace(command: &str, args: &[&str]) -> Result<Infallible, Error> {
  exec_replace_inner(command, args)
}

#[cfg(unix)]
pub fn kill(pid: u32) -> std::io::Result<()> {
  // SAFETY: libc call
  let result = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
  if result == 0 {
    Ok(())
  } else {
    Err(std::io::Error::last_os_error())
  }
}

#[cfg(windows)]
pub fn kill(pid: u32) -> std::io::Result<()> {
  use std::process::Command;
  Command::new("taskkill")
    .args(["/PID", &pid.to_string(), "/F"])
    .status()?;
  Ok(())
}

/// denext: a spawned test process and everything it starts: on Unix the
/// process group it leads (spawn it with
/// [`crate::builders::TestCommandBuilder::own_process_group`]), on Windows a
/// job object it is put in right after it starts (kill-on-close, so the job
/// also dies with this process). Killing a test used to kill only its direct
/// child, and a grandchild (a Node test's `fixtures/loop.js` child) outlived
/// the test and the harness by hours.
///
/// [`ProcessTree::wait_in_thread`] reaps the leader and, once it exits,
/// kills the rest of the tree before the reap (while the zombie leader keeps
/// its pid, so the group id can't have been reused), then the tree is never
/// signalled again.
pub struct ProcessTree {
  inner: std::sync::Arc<ProcessTreeInner>,
}

struct ProcessTreeInner {
  pid: u32,
  /// set, under the lock, once the leader has been reaped
  reaped: std::sync::Mutex<bool>,
  /// this tree's slot in [`LIVE_GROUPS`]
  #[cfg(unix)]
  slot: Option<usize>,
  #[cfg(windows)]
  job: Option<win32job::Job>,
}

/// The process groups of the live trees, for a signal handler to kill (only
/// atomics: a handler may not take a lock). A tree finding no free slot is
/// still killed when it ends, just not on a signal.
#[cfg(unix)]
static LIVE_GROUPS: [std::sync::atomic::AtomicI32; 512] =
  [const { std::sync::atomic::AtomicI32::new(0) }; 512];

impl ProcessTree {
  /// Takes over `child`, which leads its own process group (Unix).
  pub fn new(child: &std::process::Child) -> Self {
    let pid = child.id();
    #[cfg(unix)]
    let slot = {
      use std::sync::atomic::Ordering;
      LIVE_GROUPS.iter().position(|slot| {
        slot
          .compare_exchange(0, pid as i32, Ordering::SeqCst, Ordering::SeqCst)
          .is_ok()
      })
    };
    #[cfg(windows)]
    let job = {
      use std::os::windows::io::AsRawHandle;
      let mut info = win32job::ExtendedLimitInfo::default();
      info.limit_kill_on_job_close();
      win32job::Job::create_with_limit_info(&info)
        .ok()
        .filter(|job| job.assign_process(child.as_raw_handle() as _).is_ok())
    };
    Self {
      inner: std::sync::Arc::new(ProcessTreeInner {
        pid,
        reaped: std::sync::Mutex::new(false),
        #[cfg(unix)]
        slot,
        #[cfg(windows)]
        job,
      }),
    }
  }

  pub fn pid(&self) -> u32 {
    self.inner.pid
  }

  /// Waits for the leader on a thread of its own and sends its exit status;
  /// before the leader is reaped, whatever is left of the tree is killed.
  pub fn wait_in_thread<C>(
    &self,
    mut child: C,
  ) -> std::sync::mpsc::Receiver<std::io::Result<std::process::ExitStatus>>
  where
    C: std::ops::DerefMut<Target = std::process::Child> + Send + 'static,
  {
    let (tx, rx) = std::sync::mpsc::channel();
    let inner = self.inner.clone();
    crate::print::spawn_thread(move || {
      // as `Child::wait` does: a child reading stdin sees its end
      drop(child.stdin.take());
      wait_without_reaping(inner.pid);
      inner.kill_tree();
      let status = {
        let mut reaped = inner.reaped.lock().unwrap();
        #[cfg(unix)]
        inner.release_slot();
        let status = child.wait();
        *reaped = true;
        status
      };
      let _ = tx.send(status);
    });
    rx
  }

  /// Kills the whole tree (a timeout); a no-op once the leader was reaped.
  pub fn kill(&self) {
    let reaped = self.inner.reaped.lock().unwrap();
    if !*reaped {
      self.inner.kill_tree();
    }
  }
}

impl ProcessTreeInner {
  fn kill_tree(&self) {
    #[cfg(unix)]
    {
      // SAFETY: SIGKILL to the group this tree's (unreaped) leader leads;
      // ESRCH (nothing left) is ignored
      unsafe {
        libc::kill(-(self.pid as i32), libc::SIGKILL);
      }
    }
    #[cfg(windows)]
    {
      match &self.job {
        Some(job) => {
          // SAFETY: the job handle is open for the call
          unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(
              job.handle() as _,
              1,
            );
          }
        }
        None => {
          let _ = kill(self.pid);
        }
      }
    }
  }

  #[cfg(unix)]
  fn release_slot(&self) {
    if let Some(slot) = self.slot {
      LIVE_GROUPS[slot].store(0, std::sync::atomic::Ordering::SeqCst);
    }
  }
}

impl Drop for ProcessTreeInner {
  fn drop(&mut self) {
    // a tree dropped before its leader was reaped (no waiter, or one that
    // never ran) still goes; on Windows closing the job does the same
    if !*self.reaped.get_mut().unwrap_or_else(|e| e.into_inner()) {
      self.kill_tree();
      #[cfg(unix)]
      self.release_slot();
    }
  }
}

/// Blocks until `pid` (a child of this process) has exited, leaving it a
/// zombie for `Child::wait` to reap.
#[cfg(unix)]
fn wait_without_reaping(pid: u32) {
  loop {
    // SAFETY: an all-zero siginfo_t is a valid out buffer
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: plain libc call on our own child
    let rc = unsafe {
      libc::waitid(
        libc::P_PID,
        pid as libc::id_t,
        &mut info,
        libc::WEXITED | libc::WNOWAIT,
      )
    };
    if rc == 0
      || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR)
    {
      return;
    }
  }
}

/// Windows has no zombie to hold the pid; the job, not the pid, names the
/// tree, so waiting for the leader in `Child::wait` is enough.
#[cfg(windows)]
fn wait_without_reaping(pid: u32) {
  use windows_sys::Win32::Foundation::CloseHandle;
  use windows_sys::Win32::System::Threading::INFINITE;
  use windows_sys::Win32::System::Threading::OpenProcess;
  use windows_sys::Win32::System::Threading::PROCESS_SYNCHRONIZE;
  use windows_sys::Win32::System::Threading::WaitForSingleObject;
  // SAFETY: plain Win32 calls on a handle opened and closed here
  unsafe {
    let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
    if !handle.is_null() {
      WaitForSingleObject(handle, INFINITE);
      CloseHandle(handle);
    }
  }
}

/// denext: kills every live [`ProcessTree`] (a harness that ends early,
/// a panic unwinding past the runner).
pub fn kill_all_process_trees() {
  #[cfg(unix)]
  for slot in &LIVE_GROUPS {
    let pgid = slot.swap(0, std::sync::atomic::Ordering::SeqCst);
    if pgid > 0 {
      // SAFETY: SIGKILL to a registered group whose leader isn't reaped yet
      unsafe {
        libc::kill(-pgid, libc::SIGKILL);
      }
    }
  }
  // Windows: each job dies with the last handle to it, at the latest when
  // this process exits
}

/// denext: on SIGINT / SIGTERM / SIGHUP, kills every live [`ProcessTree`]
/// and then dies of the signal as before. Trees lead process groups of their
/// own, so the terminal's Ctrl+C no longer reaches them by itself. Windows
/// needs nothing: the jobs are killed with this process.
pub fn kill_process_trees_on_signal() {
  #[cfg(unix)]
  {
    extern "C" fn on_signal(signal: libc::c_int) {
      for slot in &LIVE_GROUPS {
        let pgid = slot.load(std::sync::atomic::Ordering::SeqCst);
        if pgid > 0 {
          // SAFETY: kill(2) is async-signal-safe
          unsafe {
            libc::kill(-pgid, libc::SIGKILL);
          }
        }
      }
      // SAFETY: signal(2) and raise(3) are async-signal-safe
      unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
      }
    }
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
      // SAFETY: installs a handler that only makes async-signal-safe calls
      unsafe {
        libc::signal(
          signal,
          on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
      }
    }
  }
}

#[cfg(all(test, unix))]
mod process_tree_tests {
  use std::io::BufRead;
  use std::process::Command;
  use std::process::Stdio;
  use std::time::Duration;
  use std::time::Instant;

  use super::*;

  /// `kill_all_process_trees` reaches every tree of this process
  static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

  fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists
    unsafe { libc::kill(pid, 0) == 0 }
  }

  fn gone_within(pid: i32, limit: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
      if !alive(pid) {
        return true;
      }
      std::thread::sleep(Duration::from_millis(20));
    }
    false
  }

  /// A fixture that starts a sleeping grandchild, prints its pid and then
  /// either sleeps itself (`hang`) or exits; returns the tree and the pid.
  fn spawn_fixture(hang: bool) -> (ProcessTree, std::process::Child, i32) {
    use std::os::unix::process::CommandExt;
    let script = if hang {
      "sleep 600 & echo $!; sleep 600"
    } else {
      "sleep 600 & echo $!"
    };
    let mut child = Command::new("sh")
      .args(["-c", script])
      .stdout(Stdio::piped())
      .process_group(0)
      .spawn()
      .unwrap();
    let tree = ProcessTree::new(&child);
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
      .read_line(&mut line)
      .unwrap();
    let grandchild = line.trim().parse::<i32>().unwrap();
    assert!(alive(grandchild));
    (tree, child, grandchild)
  }

  /// A test that times out: killing the tree takes its grandchild too.
  #[test]
  fn timeout_kills_the_grandchild() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (tree, child, grandchild) = spawn_fixture(true);
    let rx = tree.wait_in_thread(Box::new(child));
    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    tree.kill(); // the harness's deadline
    let status = rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
    assert!(!status.success());
    assert!(gone_within(grandchild, Duration::from_secs(10)));
    // signalling after the reap is a no-op
    tree.kill();
  }

  /// A test that exits on its own leaves no grandchild behind either.
  #[test]
  fn exit_kills_a_leftover_grandchild() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (tree, child, grandchild) = spawn_fixture(false);
    let rx = tree.wait_in_thread(Box::new(child));
    let status = rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
    assert!(status.success());
    assert!(gone_within(grandchild, Duration::from_secs(10)));
  }

  /// A harness that ends (or panics) kills the trees still running.
  #[test]
  fn kill_all_kills_live_trees() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (tree, child, grandchild) = spawn_fixture(true);
    let rx = tree.wait_in_thread(Box::new(child));
    kill_all_process_trees();
    rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
    assert!(gone_within(grandchild, Duration::from_secs(10)));
  }
}
