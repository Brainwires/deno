// Copyright 2018-2026 the Deno authors. MIT license.

//! The supervisor of `--kernel-sandbox=broker` (denext): the unconfined
//! process that stays behind as the broker while the program runs confined
//! in a child.
//!
//! The order is exec, then fork, then restrict:
//!
//! 1. The first image (the one that parsed the flags, compiled the policy
//!    and loaded the module graph) re-executes itself with a [`Handoff`] in
//!    a sealed descriptor ([`exec_supervisor`]). Same PID, a fresh image
//!    with one thread: no JavaScript engine, no module, no configuration is
//!    ever loaded in it.
//! 2. That image, first thing in `main` ([`take_handoff`], [`run`]), makes
//!    the first broker channel (a socket pair) and forks. One thread, so
//!    the fork is an ordinary one.
//! 3. The child re-executes the program once more as the confined worker:
//!    the plan in its sealed descriptor as under `enforce`, plus its end of
//!    the channel ([`BROKER_FD_ENV_VAR`]). The worker applies the plan
//!    before it opens anything.
//! 4. The parent is the supervisor: it serves the channel (the caller's
//!    `serve`), passes on the signals it is sent, reaps, and exits as the
//!    worker exited. It never runs program code and never reads anything
//!    the worker names except through the broker's own checks.
//!
//! The supervisor keeps the PID the program was started with, so what
//! watches that PID (a shell, a service manager) sees the program's exit
//! status and can signal it. The worker dies with the supervisor (Linux:
//! `PR_SET_PDEATHSIG`); without it, on macOS, it keeps running confined
//! with every brokered operation refused.
//!
//! Hardening of the supervisor: not dumpable (Linux `PR_SET_DUMPABLE` 0, so
//! the worker, which runs as the same user, can't attach to it or read its
//! memory through `/proc`; macOS `PT_DENY_ATTACH`), and a child subreaper on
//! Linux, so processes the worker orphans end with it instead of outliving
//! the sandbox's supervisor.

#![allow(
  clippy::disallowed_methods,
  reason = "the supervisor acts on the real process and environment"
)]

use std::ffi::CString;
use std::ffi::OsString;
use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::Ordering;

use serde::Deserialize;
use serde::Serialize;

use crate::plan::EnforcePlan;

/// Names the descriptor that holds the [`Handoff`] across the exec into the
/// supervisor image. Like the plan's variable it is never trusted on its
/// own: the worker it leads to still compiles its own plan and refuses to
/// run under any other.
pub const SUPERVISE_FD_ENV_VAR: &str = "DENO_INTERNAL_KERNEL_SUPERVISE_FD";

/// Names the worker's end of the first broker channel.
pub const BROKER_FD_ENV_VAR: &str = "DENO_INTERNAL_KERNEL_BROKER_FD";

/// The largest hand-over (a plan and an environment).
const MAX_HANDOFF_BYTES: usize = 32 * 1024 * 1024;

/// What the first image hands the supervisor image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Handoff {
  /// The worker's plan (it carries the broker's policy too).
  pub plan: EnforcePlan,
  /// The worker's environment, names and values as bytes.
  pub env: Vec<(Vec<u8>, Vec<u8>)>,
  /// This program's file (macOS re-executes by path).
  pub exe: PathBuf,
  /// macOS, where `sandbox_init` is missing: the profile the worker is
  /// started under with `sandbox-exec`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub sandbox_exec_profile: Option<String>,
}

impl Handoff {
  pub fn env_os(&self) -> Vec<(OsString, OsString)> {
    self
      .env
      .iter()
      .map(|(k, v)| {
        (OsString::from_vec(k.clone()), OsString::from_vec(v.clone()))
      })
      .collect()
  }
}

fn open_max() -> i32 {
  // SAFETY: a zeroed rlimit is a valid out parameter
  let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
  // SAFETY: a valid out parameter
  let ok = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0;
  let soft = if ok { limit.rlim_cur } else { 1024 };
  soft.min(65_536) as i32
}

/// Re-executes this program as the supervisor image (the same arguments,
/// the same PID) with `handoff` in a sealed descriptor. Every descriptor
/// but stdio and those `keep` names is made close-on-exec first. Returns
/// only on failure.
pub fn exec_supervisor(
  handoff: &Handoff,
  keep: impl Fn(i32) -> bool,
) -> io::Error {
  let bytes = match serde_json::to_vec(handoff) {
    Ok(bytes) => bytes,
    Err(err) => return io::Error::other(err),
  };
  let sealed = match crate::inherit::seal(&bytes) {
    Ok(fd) => fd,
    Err(err) => return err,
  };
  let sealed_raw = sealed.as_raw_fd();
  for fd in 3..open_max() {
    // SAFETY: flags of a descriptor number; one that isn't open just fails
    unsafe {
      let flags = libc::fcntl(fd, libc::F_GETFD);
      if flags < 0 {
        continue;
      }
      let flags = if fd == sealed_raw || keep(fd) {
        flags & !libc::FD_CLOEXEC
      } else {
        flags | libc::FD_CLOEXEC
      };
      libc::fcntl(fd, libc::F_SETFD, flags);
    }
  }
  let Ok(exe) = CString::new(handoff.exe.as_os_str().as_bytes()) else {
    return io::Error::other("the program's path holds a NUL byte");
  };
  let argv: Option<Vec<CString>> = std::env::args_os()
    .map(|a| CString::new(a.as_bytes()).ok())
    .collect();
  let Some(argv) = argv else {
    return io::Error::other("an argument holds a NUL byte");
  };
  // the supervisor image keeps this image's environment (it reads none of
  // it but the hand-over's variable); the worker gets the hand-over's
  let mut envp: Vec<CString> = std::env::vars_os()
    .filter(|(k, _)| k != SUPERVISE_FD_ENV_VAR && k != BROKER_FD_ENV_VAR)
    .filter_map(|(k, v)| {
      let mut kv = k.as_bytes().to_vec();
      kv.push(b'=');
      kv.extend_from_slice(v.as_bytes());
      CString::new(kv).ok()
    })
    .collect();
  envp.push(
    CString::new(format!("{SUPERVISE_FD_ENV_VAR}={sealed_raw}"))
      .expect("no NUL in a number"),
  );
  let mut argv_p: Vec<*const libc::c_char> =
    argv.iter().map(|a| a.as_ptr()).collect();
  argv_p.push(std::ptr::null());
  let mut envp_p: Vec<*const libc::c_char> =
    envp.iter().map(|e| e.as_ptr()).collect();
  envp_p.push(std::ptr::null());
  // SAFETY: NUL-terminated arrays of valid C strings that outlive the call
  unsafe {
    libc::execve(exe.as_ptr(), argv_p.as_ptr(), envp_p.as_ptr());
  }
  let err = io::Error::last_os_error();
  drop(sealed);
  err
}

/// The hand-over the image before this one left, if its variable is set
/// (removed here; the descriptor is read and closed). Call first thing in
/// `main`, while the process has one thread. `Err`: the caller refuses to
/// start (the variable without a valid hand-over is never ignored).
pub fn take_handoff() -> Result<Option<Handoff>, String> {
  let Some(value) = std::env::var_os(SUPERVISE_FD_ENV_VAR) else {
    return Ok(None);
  };
  // SAFETY: called while the process has one thread
  unsafe { std::env::remove_var(SUPERVISE_FD_ENV_VAR) };
  let fd: i32 = value
    .to_str()
    .and_then(|v| v.parse().ok())
    .filter(|fd| *fd > 2)
    .ok_or_else(|| {
      format!("{SUPERVISE_FD_ENV_VAR}={value:?} is not a descriptor")
    })?;
  let bytes = crate::inherit::read_fd(fd);
  // SAFETY: the hand-over's descriptor, read (or refused) and done with
  unsafe { libc::close(fd) };
  let bytes = bytes?;
  if bytes.len() > MAX_HANDOFF_BYTES {
    return Err("the supervisor's hand-over is too large".to_string());
  }
  serde_json::from_slice(&bytes)
    .map(Some)
    .map_err(|err| format!("the supervisor's hand-over does not parse: {err}"))
}

/// The worker's end of the broker channel, if the supervisor left one
/// (the variable is removed; the descriptor is made close-on-exec). Call
/// first thing in `main`. A variable that doesn't name an open socket is an
/// error.
pub fn take_broker_fd() -> Result<Option<OwnedFd>, String> {
  let Some(value) = std::env::var_os(BROKER_FD_ENV_VAR) else {
    return Ok(None);
  };
  // SAFETY: called while the process has one thread
  unsafe { std::env::remove_var(BROKER_FD_ENV_VAR) };
  let fd: i32 = value
    .to_str()
    .and_then(|v| v.parse().ok())
    .filter(|fd| *fd > 2)
    .ok_or_else(|| {
      format!("{BROKER_FD_ENV_VAR}={value:?} is not a descriptor")
    })?;
  // SAFETY: a zeroed stat is a valid out parameter
  let mut st: libc::stat = unsafe { std::mem::zeroed() };
  // SAFETY: a descriptor number and an out parameter
  if unsafe { libc::fstat(fd, &mut st) } != 0
    || st.st_mode & libc::S_IFMT != libc::S_IFSOCK
  {
    return Err(format!(
      "{BROKER_FD_ENV_VAR}: descriptor {fd} is not a socket"
    ));
  }
  // SAFETY: an open descriptor this process now owns
  let fd = unsafe { OwnedFd::from_raw_fd(fd) };
  // SAFETY: a valid descriptor
  unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
  Ok(Some(fd))
}

/// The worker's PID, for the signal handler.
static WORKER: AtomicI32 = AtomicI32::new(0);

/// The signals the supervisor passes on.
const FORWARDED: &[libc::c_int] = &[
  libc::SIGHUP,
  libc::SIGINT,
  libc::SIGQUIT,
  libc::SIGTERM,
  libc::SIGUSR1,
  libc::SIGUSR2,
  libc::SIGALRM,
  libc::SIGCONT,
  libc::SIGWINCH,
];

/// Passes a signal on to the worker when a process sent it. One the kernel
/// made (the terminal's Ctrl-C, a hangup, a window change) went to the whole
/// foreground process group, the worker included, so passing it on would
/// deliver it twice.
extern "C" fn forward(
  signal: libc::c_int,
  info: *mut libc::siginfo_t,
  _context: *mut libc::c_void,
) {
  let worker = WORKER.load(Ordering::Relaxed);
  if worker <= 0 || info.is_null() {
    return;
  }
  // Linux says who made it in si_code (SI_USER, SI_QUEUE, SI_TKILL are not
  // positive; the kernel's own are)
  #[cfg(target_os = "linux")]
  // SAFETY: the kernel's siginfo for this delivery
  let from_a_process = unsafe { (*info).si_code } <= 0;
  // macOS leaves si_code 0 for kill(2) and names the sender in si_pid;
  // a signal the terminal made has no sender
  #[cfg(not(target_os = "linux"))]
  // SAFETY: the kernel's siginfo for this delivery
  let from_a_process = unsafe { (*info).si_pid } != 0;
  if from_a_process {
    // SAFETY: kill is async-signal-safe
    unsafe { libc::kill(worker, signal) };
  }
}

fn forward_signals() {
  for signal in FORWARDED {
    // SAFETY: a zeroed sigaction with a handler of the SA_SIGINFO shape
    unsafe {
      let mut action: libc::sigaction = std::mem::zeroed();
      action.sa_sigaction = forward as *const () as usize;
      action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
      libc::sigemptyset(&mut action.sa_mask);
      libc::sigaction(*signal, &action, std::ptr::null_mut());
    }
  }
  // a write to a channel the worker closed is an error, never a signal
  // SAFETY: ignoring a signal
  unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
}

fn harden() {
  #[cfg(target_os = "linux")]
  {
    // SAFETY: prctl with integer arguments
    unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
    crate::linux::become_subreaper();
  }
  #[cfg(target_os = "macos")]
  {
    const PT_DENY_ATTACH: libc::c_int = 31;
    // SAFETY: ptrace on this process with no address
    unsafe { libc::ptrace(PT_DENY_ATTACH, 0, std::ptr::null_mut(), 0) };
  }
}

fn channel() -> io::Result<(OwnedFd, OwnedFd)> {
  let mut fds = [0 as libc::c_int; 2];
  // SAFETY: an array of two descriptors for the kernel to fill
  if unsafe {
    libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr())
  } < 0
  {
    return Err(io::Error::last_os_error());
  }
  // SAFETY: just created, owned from here
  Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Ends this process the way the worker ended: its exit code, or the
/// signal that killed it (so a shell reports the same thing).
fn exit_like(status: libc::c_int) -> ! {
  if libc::WIFSIGNALED(status) {
    let signal = libc::WTERMSIG(status);
    // SAFETY: no core file of the supervisor for the worker's crash; the
    // default action for the signal, then the signal itself
    unsafe {
      let none = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
      };
      libc::setrlimit(libc::RLIMIT_CORE, &none);
      libc::signal(signal, libc::SIG_DFL);
      let mut set: libc::sigset_t = std::mem::zeroed();
      libc::sigemptyset(&mut set);
      libc::sigaddset(&mut set, signal);
      libc::sigprocmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
      libc::raise(signal);
    }
    // a signal that doesn't end a process by default
    std::process::exit(128 + signal);
  }
  std::process::exit(libc::WEXITSTATUS(status));
}

/// Ends and reaps every process the worker left behind: as a subreaper
/// the supervisor adopted them, and each one that ends can hand it more.
#[cfg(target_os = "linux")]
fn end_orphans() {
  for _ in 0..64 {
    let orphans = crate::linux::children_of_self();
    if orphans.is_empty() {
      return;
    }
    for pid in orphans {
      let Ok(raw) = libc::pid_t::try_from(pid) else {
        continue;
      };
      // SAFETY: a child of this process, by the /proc scan; after SIGKILL
      // the wait returns at once
      unsafe {
        libc::kill(raw, libc::SIGKILL);
        let mut status = 0;
        libc::waitpid(raw, &mut status, 0);
      }
    }
  }
}

/// Runs the supervisor: forks; the child calls `exec_worker` with the
/// number of its end of the channel (it re-executes as the confined
/// worker and returns only an error); this process serves the other end
/// with `serve` (which starts the broker and returns), then waits and exits
/// as the worker did. Call while the process has one thread.
pub fn run(
  exec_worker: impl FnOnce(i32) -> io::Error,
  serve: impl FnOnce(OwnedFd) -> io::Result<()>,
) -> ! {
  let fail = |what: &str, err: io::Error| -> ! {
    eprintln!("kernel sandbox: the supervisor can't start: {what}: {err}");
    std::process::exit(1);
  };
  let (ours, theirs) = match channel() {
    Ok(pair) => pair,
    Err(err) => fail("the broker channel", err),
  };
  // SAFETY: our end is the supervisor's alone
  unsafe { libc::fcntl(ours.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
  #[cfg(target_os = "linux")]
  let supervisor = std::process::id() as libc::pid_t;
  // SAFETY: the process has one thread, so the child is an ordinary process
  let pid = unsafe { libc::fork() };
  if pid < 0 {
    fail("fork", io::Error::last_os_error());
  }
  if pid == 0 {
    drop(ours);
    #[cfg(target_os = "linux")]
    {
      // the worker never outlives its supervisor (kept across the exec)
      // SAFETY: prctl with integer arguments; getppid has no preconditions
      unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
        if libc::getppid() != supervisor {
          libc::_exit(1);
        }
      }
    }
    let err = exec_worker(theirs.as_raw_fd());
    eprintln!("kernel sandbox: re-executing under the sandbox failed: {err}");
    // SAFETY: ending the forked child without running the parent's exit
    // handlers
    unsafe { libc::_exit(126) };
  }
  drop(theirs);
  WORKER.store(pid, Ordering::SeqCst);
  harden();
  forward_signals();
  if let Err(err) = serve(ours) {
    // SAFETY: the worker can't run without its broker
    unsafe { libc::kill(pid, libc::SIGKILL) };
    fail("the broker", err);
  }
  loop {
    let mut status: libc::c_int = 0;
    // SAFETY: a valid out parameter; any child (the supervisor reaps what
    // it adopts too)
    let reaped = unsafe { libc::waitpid(-1, &mut status, 0) };
    if reaped < 0 {
      let err = io::Error::last_os_error();
      if err.kind() == io::ErrorKind::Interrupted {
        continue;
      }
      // no child left and the worker was never reaped
      std::process::exit(1);
    }
    if reaped == pid {
      WORKER.store(0, Ordering::SeqCst);
      // what the worker orphaned ends with the sandbox's supervisor
      #[cfg(target_os = "linux")]
      end_orphans();
      exit_like(status);
    }
  }
}
