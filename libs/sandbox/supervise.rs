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
//! A compiled program (`denort`) has no first image to leave: its grant is
//! baked and its modules are embedded, so the process that starts is
//! already the one image there is, with one thread and nothing loaded. It
//! forks in place ([`fork_worker`]): the child goes on as the confined
//! worker, the parent is the supervisor. No exec at all.
//!
//! The same supervisor, with or without a broker, answers the exec system
//! calls of a strict exec scope on Linux ([`run_with_gate`],
//! [`fork_worker_with_gate`], [`crate::exec_gate`]): the worker hands the
//! listener of its filter over a channel of its own as it applies its plan.
//! A run that needs no broker and has no such scope has no supervisor.
//!
//! The same shape, with a tracer where the broker is, is the Linux observer
//! of an explicit `--kernel-sandbox=warn` ([`run_traced`],
//! [`crate::trace`]): the supervisor sees the tree's system calls, reports
//! what `enforce` would refuse and lets them continue.
//!
//! As PID 1 of a container the supervisor does what an init must: it has a
//! handler for every signal it passes on (the kernel delivers PID 1 only
//! signals it handles), it reaps every child it is handed, and when the
//! worker was killed by a signal it exits with `128 + signal` (PID 1 can't
//! be ended by a signal of its own).
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
#![allow(
  clippy::print_stderr,
  reason = "the supervisor image sets no logger up: its few messages go straight to stderr"
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
  /// The supervisor observes instead of brokering (Linux, an explicit
  /// `--kernel-sandbox=warn`): the worker runs unconfined under a tracing
  /// filter, and `plan` is only what the reports are decided against.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub trace: Option<TraceHandoff>,
}

/// What a tracing supervisor is handed ([`Handoff::trace`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TraceHandoff {
  pub policy: crate::would_deny::TracePolicy,
  /// The warn log (`DENO_KERNEL_SANDBOX_WARN_LOG`) the reports are
  /// appended to.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub log: Option<PathBuf>,
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

/// Re-executes this program (`exe`, the same arguments) with `env` and
/// nothing handed over: how a tracing supervisor's child starts the run it
/// observes. Descriptors are left as they are. Returns only on failure.
pub fn exec_self(
  exe: &std::path::Path,
  env: Vec<(OsString, OsString)>,
) -> io::Error {
  let Ok(exe) = CString::new(exe.as_os_str().as_bytes()) else {
    return io::Error::other("the program's path holds a NUL byte");
  };
  let argv: Option<Vec<CString>> = std::env::args_os()
    .map(|a| CString::new(a.as_bytes()).ok())
    .collect();
  let Some(argv) = argv else {
    return io::Error::other("an argument holds a NUL byte");
  };
  let envp: Vec<CString> = env
    .into_iter()
    .filter_map(|(k, v)| {
      let mut kv = k.into_vec();
      kv.push(b'=');
      kv.extend_from_slice(v.as_bytes());
      CString::new(kv).ok()
    })
    .collect();
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
  io::Error::last_os_error()
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

/// `adopt`: also become a child subreaper (Linux), so what the worker
/// orphans is this process's to end.
fn harden(
  #[allow(unused_variables, reason = "the subreaper is Linux's")] adopt: bool,
) {
  #[cfg(target_os = "linux")]
  {
    // SAFETY: prctl with integer arguments
    unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
    if adopt {
      crate::linux::become_subreaper();
    }
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

/// Closes every descriptor of this process but stdio and `keep`. The
/// supervisor holds what the image before it kept for the worker (a
/// listening socket the service manager passed, a parent's pipe); the
/// worker has its own copies, and a copy left open here would keep a pipe
/// from reaching its end, or a socket from closing, when the worker closes
/// its own.
fn close_other_fds(keep: &[i32]) {
  // the open descriptors, listed (a number per possible descriptor would
  // be tens of thousands of calls under a raised limit); where the listing
  // can't be read (inside an outer sandbox), every number
  let listing = if cfg!(target_os = "linux") {
    "/proc/self/fd"
  } else {
    "/dev/fd"
  };
  let mut open: Vec<i32> = std::fs::read_dir(listing)
    .map(|entries| {
      entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .collect()
    })
    .unwrap_or_default();
  if open.is_empty() {
    open = (3..open_max()).collect();
  }
  for fd in open {
    if fd > 2 && !keep.contains(&fd) {
      // SAFETY: closing a descriptor number this process doesn't use (one
      // that isn't open just fails; the directory listing's own descriptor
      // is already closed)
      unsafe { libc::close(fd) };
    }
  }
}

/// The supervisor's half after the fork: serves its channels (`ours`, the
/// only descriptors it keeps besides stdio) with `serve` (which starts the
/// broker and returns), passes signals on, reaps, and exits as the worker
/// `pid` exited. `adopt`: what the worker orphans ends with the supervisor
/// (a sandbox's supervisor; an observer leaves the processes a run left
/// behind running, as they would without it).
fn supervise_worker(
  pid: libc::pid_t,
  ours: Vec<OwnedFd>,
  adopt: bool,
  serve: impl FnOnce(Vec<OwnedFd>) -> io::Result<()>,
  before_exit: impl FnOnce(),
) -> ! {
  WORKER.store(pid, Ordering::SeqCst);
  harden(adopt);
  forward_signals();
  let keep: Vec<i32> = ours.iter().map(|fd| fd.as_raw_fd()).collect();
  close_other_fds(&keep);
  if let Err(err) = serve(ours) {
    // SAFETY: the worker can't run without its broker
    unsafe { libc::kill(pid, libc::SIGKILL) };
    eprintln!("kernel sandbox: the supervisor can't start: the broker: {err}");
    std::process::exit(1);
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
      if adopt {
        end_orphans();
      }
      before_exit();
      exit_like(status);
    }
  }
}

/// The forked child's first steps: it never outlives its supervisor
/// (Linux; kept across an exec).
fn tie_to_supervisor(
  #[allow(unused_variables, reason = "the parent-death signal is Linux's")]
  supervisor: libc::pid_t,
) {
  #[cfg(target_os = "linux")]
  // SAFETY: prctl with integer arguments; getppid has no preconditions
  unsafe {
    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
    if libc::getppid() != supervisor {
      libc::_exit(1);
    }
  }
}

/// Prints a fatal error for the user just before the process exits. The
/// supervisor and its forked child have no logger set up, so stderr is the
/// only channel.
#[allow(
  clippy::print_stderr,
  reason = "fatal start-up errors go to the user before exit; no logger here"
)]
fn fatal_note(message: std::fmt::Arguments<'_>) {
  eprintln!("{message}");
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
    fatal_note(format_args!(
      "kernel sandbox: the supervisor can't start: {what}: {err}"
    ));
    std::process::exit(1);
  };
  let (ours, theirs) = match channel() {
    Ok(pair) => pair,
    Err(err) => fail("the broker channel", err),
  };
  // SAFETY: our end is the supervisor's alone
  unsafe { libc::fcntl(ours.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
  let supervisor = std::process::id() as libc::pid_t;
  // SAFETY: the process has one thread, so the child is an ordinary process
  let pid = unsafe { libc::fork() };
  if pid < 0 {
    fail("fork", io::Error::last_os_error());
  }
  if pid == 0 {
    drop(ours);
    tie_to_supervisor(supervisor);
    let err = exec_worker(theirs.as_raw_fd());
    fatal_note(format_args!(
      "kernel sandbox: re-executing under the sandbox failed: {err}"
    ));
    // SAFETY: ending the forked child without running the parent's exit
    // handlers
    unsafe { libc::_exit(126) };
  }
  drop(theirs);
  supervise_worker(pid, vec![ours], true, one_channel(serve), || {})
}

/// `serve` for the supervisor's one channel.
fn one_channel(
  serve: impl FnOnce(OwnedFd) -> io::Result<()>,
) -> impl FnOnce(Vec<OwnedFd>) -> io::Result<()> {
  move |ours| match ours.into_iter().next() {
    Some(ours) => serve(ours),
    None => Err(io::Error::other("the supervisor has no channel")),
  }
}

/// `serve` for the supervisor of a gated worker: the broker's channel when
/// there is one, then the exec gate's.
#[cfg(target_os = "linux")]
fn gated_channels(
  broker: bool,
  serve: impl FnOnce(Option<OwnedFd>, OwnedFd) -> io::Result<()>,
) -> impl FnOnce(Vec<OwnedFd>) -> io::Result<()> {
  move |ours| {
    let mut ours = ours.into_iter();
    let broker = if broker { ours.next() } else { None };
    match ours.next() {
      Some(gate) => serve(broker, gate),
      None => Err(io::Error::other("the supervisor has no channel")),
    }
  }
}

/// [`run`] for a worker whose exec system calls the supervisor answers
/// (Linux, [`crate::exec_gate`]), with a broker (`broker`) or without one.
/// The child calls `exec_worker` with the numbers of its ends of the
/// broker's channel (when there is one) and the gate's (it names them in
/// the worker's environment and re-executes; it returns only an error);
/// this process serves the other ends with `serve` (which starts the
/// broker and the gate's thread and returns), then waits and exits as the
/// worker did. Call while the process has one thread.
#[cfg(target_os = "linux")]
pub fn run_with_gate(
  broker: bool,
  exec_worker: impl FnOnce(Option<i32>, i32) -> io::Error,
  serve: impl FnOnce(Option<OwnedFd>, OwnedFd) -> io::Result<()>,
) -> ! {
  let fail = |what: &str, err: io::Error| -> ! {
    fatal_note(format_args!(
      "kernel sandbox: the supervisor can't start: {what}: {err}"
    ));
    std::process::exit(1);
  };
  let broker_pair = if broker {
    match channel() {
      Ok(pair) => Some(pair),
      Err(err) => fail("the broker channel", err),
    }
  } else {
    None
  };
  let (gate_ours, gate_theirs) = match channel() {
    Ok(pair) => pair,
    Err(err) => fail("the exec channel", err),
  };
  let (broker_ours, broker_theirs) = match broker_pair {
    Some((ours, theirs)) => (Some(ours), Some(theirs)),
    None => (None, None),
  };
  for fd in broker_ours.iter().chain([&gate_ours]) {
    // SAFETY: our ends are the supervisor's alone
    unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
  }
  let supervisor = std::process::id() as libc::pid_t;
  // SAFETY: the process has one thread, so the child is an ordinary process
  let pid = unsafe { libc::fork() };
  if pid < 0 {
    fail("fork", io::Error::last_os_error());
  }
  if pid == 0 {
    drop(broker_ours);
    drop(gate_ours);
    tie_to_supervisor(supervisor);
    let err = exec_worker(
      broker_theirs.as_ref().map(|fd| fd.as_raw_fd()),
      gate_theirs.as_raw_fd(),
    );
    fatal_note(format_args!(
      "kernel sandbox: re-executing under the sandbox failed: {err}"
    ));
    // SAFETY: ending the forked child without running the parent's exit
    // handlers
    unsafe { libc::_exit(126) };
  }
  drop(broker_theirs);
  drop(gate_theirs);
  let ours: Vec<OwnedFd> = broker_ours.into_iter().chain([gate_ours]).collect();
  supervise_worker(pid, ours, true, gated_channels(broker, serve), || {})
}

/// The threads of this process (Linux), when they can be counted.
#[cfg(target_os = "linux")]
fn thread_count() -> Option<usize> {
  std::fs::read_dir("/proc/self/task").ok().map(|d| d.count())
}

/// The supervisor of a program that forks in place (a compiled program:
/// see the module docs): forks; **returns in the child**, the worker, with
/// its end of the broker channel (close-on-exec); the parent serves the
/// other end with `serve` and never returns: it exits as the worker exits.
///
/// Call while the process has one thread and before it confines itself
/// (the supervisor must stay unconfined). An `Err` means nothing happened:
/// no fork, the caller is still the one process (a second thread exists, or
/// the channel or the fork failed).
pub fn fork_worker(
  serve: impl FnOnce(OwnedFd) -> io::Result<()>,
) -> io::Result<OwnedFd> {
  #[cfg(target_os = "linux")]
  match thread_count() {
    Some(1) | None => {}
    Some(threads) => {
      return Err(io::Error::other(format!(
        "the process has {threads} threads; the supervisor must fork while it has one"
      )));
    }
  }
  // macOS: the child goes on without an exec, and the system's own
  // libraries are safe on the far side of a fork only when the process
  // never had a second thread
  #[cfg(target_os = "macos")]
  {
    unsafe extern "C" {
      fn pthread_is_threaded_np() -> libc::c_int;
    }
    // SAFETY: a query with no arguments
    if unsafe { pthread_is_threaded_np() } != 0 {
      return Err(io::Error::other(
        "the process already started a thread; the supervisor must fork before any",
      ));
    }
  }
  let (ours, theirs) = channel()?;
  for fd in [&ours, &theirs] {
    // SAFETY: each end belongs to one process alone
    unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
  }
  let supervisor = std::process::id() as libc::pid_t;
  // SAFETY: the process has one thread, so the child is an ordinary process
  let pid = unsafe { libc::fork() };
  if pid < 0 {
    return Err(io::Error::last_os_error());
  }
  if pid == 0 {
    drop(ours);
    tie_to_supervisor(supervisor);
    return Ok(theirs);
  }
  drop(theirs);
  supervise_worker(pid, vec![ours], true, one_channel(serve), || {})
}

/// [`fork_worker`] for a program whose exec system calls the supervisor
/// answers (Linux, [`crate::exec_gate`]), with a broker (`broker`) or
/// without one: **returns in the child**, the worker, with its ends of the
/// broker's channel (when there is one) and the gate's (close-on-exec);
/// the parent serves the other ends with `serve` (as [`run_with_gate`]'s)
/// and never returns. The same conditions as [`fork_worker`]; an `Err`
/// means nothing happened.
#[cfg(target_os = "linux")]
pub fn fork_worker_with_gate(
  broker: bool,
  serve: impl FnOnce(Option<OwnedFd>, OwnedFd) -> io::Result<()>,
) -> io::Result<(Option<OwnedFd>, OwnedFd)> {
  match thread_count() {
    Some(1) | None => {}
    Some(threads) => {
      return Err(io::Error::other(format!(
        "the process has {threads} threads; the supervisor must fork while it has one"
      )));
    }
  }
  let (broker_ours, broker_theirs) = if broker {
    let (ours, theirs) = channel()?;
    (Some(ours), Some(theirs))
  } else {
    (None, None)
  };
  let (gate_ours, gate_theirs) = channel()?;
  for fd in broker_ours
    .iter()
    .chain(&broker_theirs)
    .chain([&gate_ours, &gate_theirs])
  {
    // SAFETY: each end belongs to one process alone
    unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
  }
  let supervisor = std::process::id() as libc::pid_t;
  // SAFETY: the process has one thread, so the child is an ordinary process
  let pid = unsafe { libc::fork() };
  if pid < 0 {
    return Err(io::Error::last_os_error());
  }
  if pid == 0 {
    drop(broker_ours);
    drop(gate_ours);
    tie_to_supervisor(supervisor);
    return Ok((broker_theirs, gate_theirs));
  }
  drop(broker_theirs);
  drop(gate_theirs);
  let ours: Vec<OwnedFd> = broker_ours.into_iter().chain([gate_ours]).collect();
  supervise_worker(pid, ours, true, gated_channels(broker, serve), || {})
}

/// Runs the tracing supervisor of an explicit `--kernel-sandbox=warn`
/// (Linux): forks; the child installs the tracing filter, hands its
/// listener over and calls `exec_worker` (told whether the filter is in
/// place; it re-executes the run, unconfined, and returns only an error);
/// this process answers the tree's traced system calls with a
/// [`crate::trace::Tracer`] over `trace.policy` (every call continues),
/// reports to stderr and the warn log, then prints its summary and exits as
/// the worker did. A filter that can't be installed is said once, and the
/// run goes on unobserved: warn mode never stops a program.
#[cfg(target_os = "linux")]
pub fn run_traced(
  exec_worker: impl FnOnce(bool) -> io::Error,
  trace: TraceHandoff,
) -> ! {
  use std::sync::Arc;
  use std::sync::atomic::AtomicBool;

  let fail = |what: &str, err: io::Error| -> ! {
    eprintln!("kernel sandbox: the supervisor can't start: {what}: {err}");
    std::process::exit(1);
  };
  let (ours, theirs) = match channel() {
    Ok(pair) => pair,
    Err(err) => fail("the listener's channel", err),
  };
  for fd in [&ours, &theirs] {
    // SAFETY: the hand-over's ends never reach the run
    unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
  }
  let supervisor = std::process::id() as libc::pid_t;
  // SAFETY: the process has one thread, so the child is an ordinary process
  let pid = unsafe { libc::fork() };
  if pid < 0 {
    fail("fork", io::Error::last_os_error());
  }
  if pid == 0 {
    drop(ours);
    tie_to_supervisor(supervisor);
    let traced = match crate::trace::install_listener() {
      Ok(listener) => {
        crate::linux::send_fd(theirs.as_raw_fd(), listener.as_raw_fd()).is_ok()
      }
      Err(_) => false,
    };
    // closed with nothing sent: the supervisor learns there is no listener
    drop(theirs);
    let err = exec_worker(traced);
    eprintln!("kernel sandbox: starting the observed run failed: {err}");
    // SAFETY: ending the forked child without running the parent's exit
    // handlers
    unsafe { libc::_exit(126) };
  }
  drop(theirs);
  let stop = Arc::new(AtomicBool::new(false));
  // the reader thread hands the listener back when it stops
  let reader: std::cell::RefCell<Option<std::thread::JoinHandle<OwnedFd>>> =
    std::cell::RefCell::new(None);
  let policy = trace.policy.clone();
  let serve = |ours: OwnedFd| -> io::Result<()> {
    // blocks until the child sent its listener, or closed its end without
    let Ok(listener) = crate::linux::recv_fd(&ours) else {
      eprintln!(
        "Warning kernel sandbox (warn): what native code and child processes would be refused isn't observed in this run: the tracing filter can't be installed here (a container's or an outer sandbox's seccomp profile refuses it)"
      );
      return Ok(());
    };
    crate::would_deny::install(crate::would_deny::ReporterConfig {
      log: trace.log.clone(),
      print: true,
      direct: true,
    });
    let tracer = crate::trace::Tracer::new(policy.clone(), pid as u32);
    let stop = stop.clone();
    let thread = std::thread::Builder::new()
      .name("deno-trace".to_string())
      .spawn(move || {
        crate::notify::serve_notifications(&listener, &stop, |call| {
          tracer.answer(call)
        });
        listener
      })?;
    *reader.borrow_mut() = Some(thread);
    Ok(())
  };
  // not a subreaper, and nothing is ended with the run: an observer leaves
  // what the run left behind as it would be without it
  supervise_worker(pid, vec![ours], false, one_channel(serve), || {
    stop.store(true, Ordering::SeqCst);
    let listener = reader
      .borrow_mut()
      .take()
      .and_then(|reader| reader.join().ok());
    let summary = crate::would_deny::summary();
    if let Some(line) = crate::would_deny::summary_line(&summary) {
      eprintln!("Warning {line}");
    }
    if let Some(listener) = listener {
      linger(
        listener,
        crate::trace::Tracer::new(policy.clone(), pid as u32),
      );
    }
  })
}

/// The run ended and processes it started are still running under the
/// tracing filter (a server it left in the background): with the listener
/// closed their traced system calls would fail, so a process of its own
/// keeps answering them (every one continues; reports still go to the warn
/// log and stderr) until the last such process is gone. It is forked here,
/// when the supervisor has one thread again, and outlives the supervisor,
/// which exits with the run's status as soon as this returns.
#[cfg(target_os = "linux")]
fn linger(listener: OwnedFd, tracer: crate::trace::Tracer) {
  let mut poll = libc::pollfd {
    fd: listener.as_raw_fd(),
    events: libc::POLLIN,
    revents: 0,
  };
  // SAFETY: one pollfd, no wait
  unsafe { libc::poll(&mut poll, 1, 0) };
  if poll.revents & libc::POLLHUP != 0 {
    // nothing uses the filter any more
    return;
  }
  // SAFETY: the reader thread was joined, so the process has one thread
  let pid = unsafe { libc::fork() };
  if pid != 0 {
    // the supervisor (or no fork: the listener closes with it)
    return;
  }
  // the lingering observer: nothing the launcher sends the run's group
  // ends it, and it holds no terminal input
  // SAFETY: plain calls in the forked child, which only serves and exits
  unsafe {
    libc::setsid();
    for signal in FORWARDED {
      libc::signal(*signal, libc::SIG_IGN);
    }
    libc::close(0);
  }
  // it reports what the run itself started reporting
  tracer.mark_started();
  let stop = std::sync::atomic::AtomicBool::new(false);
  crate::notify::serve_notifications(&listener, &stop, |call| {
    tracer.answer(call)
  });
  // SAFETY: ending the forked child without the supervisor's exit handlers
  unsafe { libc::_exit(0) };
}
