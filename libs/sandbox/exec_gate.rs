// Copyright 2018-2026 the Deno authors. MIT license.

//! `execve` / `execveat` answered by a supervising process (Linux): what
//! holds a strict exec scope (`--kernel-sandbox-exec=strict`) to the files
//! an exec system call may name.
//!
//! Landlock's execute rules name files, and the kernel opens more files for
//! execution than the one a call names: a script's `#!` interpreter and a
//! program's ELF loader ([`crate::exec`]). Those carry an execute rule so
//! the listed programs load, and a rule can't say who opens the file. Files
//! with no path at all (an anonymous memory file) are outside every path
//! rule. So the confined tree's seccomp filter hands every exec call to the
//! supervisor ([`crate::notify::notify_program`] with [`exec_syscalls`]),
//! and an [`ExecGate`] answers it:
//!
//! - the file the call names is resolved as the caller would reach it (its
//!   root, its working directory or directory descriptor, the descriptor
//!   itself for `AT_EMPTY_PATH`, the caller's own `/proc/self/exe` and
//!   `/proc/self/fd/<n>`) to an `O_PATH` descriptor of the supervisor;
//! - a file among the gate's targets ([`crate::exec::ProgramClosure::
//!   targets`], compared by device and inode as they were when the gate was
//!   made) continues: the kernel runs the call as it was made
//!   (`SECCOMP_USER_NOTIF_FLAG_CONTINUE`), under the Landlock rules, which
//!   stay the outer bound of whatever the kernel finally opens;
//! - everything else is refused with `EACCES`.
//!
//! The answer is made from the caller's memory. Native code that rewrites
//! the path while its own call is in flight is still held to the files the
//! Landlock rules let execute; a strict scope bounds which programs start,
//! never what native code already running does with its own address space.
//!
//! Fails closed like [`crate::notify`]: a call that can't be read or
//! resolved is refused (a process that made itself non-dumpable can't be
//! read, so it starts nothing), and with the supervisor gone exec fails
//! with `ENOSYS`. A filter chain holds one listener: where another one is
//! already installed (an outer supervisor), or the supervisor can't read
//! the tree's memory (a ptrace policy), there is no gate, and the policy
//! says so ([`available`]).

#![allow(
  clippy::disallowed_methods,
  reason = "the gate reads the real /proc and file system, never a virtual one"
)]

use std::ffi::CStr;
use std::ffi::CString;
use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use crate::notify::AT_FDCWD;
use crate::notify::Answer;
use crate::notify::Call;
use crate::notify::fstat;
use crate::notify::open_path;
use crate::notify::read_cstr;
use crate::notify::resolve;
use crate::notify::tgid_of;
pub use crate::plan::EXEC_GATE_FD_ENV_VAR;
use crate::seccomp::SockFilter;
use crate::seccomp::SockFprog;

const SECCOMP_SET_MODE_FILTER: libc::c_ulong = 1;
const SECCOMP_FILTER_FLAG_NEW_LISTENER: libc::c_ulong = 1 << 3;
const SECCOMP_USER_NOTIF_FLAG_CONTINUE: u32 = 1;

const NR_EXECVE: u32 = libc::SYS_execve as u32;
const NR_EXECVEAT: u32 = libc::SYS_execveat as u32;

/// The syscalls an [`ExecGate`] answers.
pub fn exec_syscalls() -> Vec<u32> {
  vec![NR_EXECVE, NR_EXECVEAT]
}

/// A file as the gate compares it: its device and inode.
type FileId = (libc::dev_t, libc::ino_t);

fn file_id_of(st: &libc::stat) -> Option<FileId> {
  (st.st_mode & libc::S_IFMT == libc::S_IFREG).then_some((st.st_dev, st.st_ino))
}

/// Answers the exec calls of a confined process tree: only the files it
/// lists.
#[derive(Debug, Clone, Default)]
pub struct ExecGate {
  targets: Vec<FileId>,
}

impl ExecGate {
  /// A gate that lets an exec call name exactly `files` (the files as they
  /// are now; one that doesn't exist, or isn't a regular file, is left
  /// out).
  pub fn new(files: &[PathBuf]) -> Self {
    let targets = files
      .iter()
      .filter_map(|file| {
        let path = CString::new(file.as_os_str().as_bytes()).ok()?;
        // SAFETY: a zeroed stat the kernel fills, a NUL-terminated path
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: as above
        let res = unsafe { libc::stat(path.as_ptr(), &mut st) };
        if res != 0 {
          return None;
        }
        file_id_of(&st)
      })
      .collect();
    Self { targets }
  }

  /// Whether `call` is one this gate answers.
  pub fn handles(&self, call: &Call) -> bool {
    call.nr == NR_EXECVE || call.nr == NR_EXECVEAT
  }

  /// The answer to `execve(path, ..)` / `execveat(dirfd, path, .., flags)`:
  /// [`Answer::Continue`] for a listed file, a refusal for anything else.
  pub fn answer(&self, call: &Call) -> Answer {
    let a = call.args;
    let (dirfd, path, flags) = match call.nr {
      NR_EXECVE => (AT_FDCWD, a[0], 0),
      NR_EXECVEAT => (a[0] as u32 as i32, a[1], a[4] as u32 as i32),
      _ => return Answer::Done(Err(libc::ENOSYS)),
    };
    match named_file(call.tid, dirfd, path, flags) {
      Ok(id) if self.targets.contains(&id) => Answer::Continue,
      Ok(_) => Answer::Done(Err(libc::EACCES)),
      Err(errno) => Answer::Done(Err(errno)),
    }
  }

  /// Answers the exec calls on `listener` until every process under the
  /// filter is gone or `stop` is set (checked every 100 ms). An answer that
  /// panics is a refusal: the caller is never left waiting.
  pub fn serve(&self, listener: OwnedFd, stop: &AtomicBool) {
    crate::notify::serve_notifications(&listener, stop, |call| {
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        self.answer(call)
      }))
      .unwrap_or(Answer::Done(Err(libc::EACCES)))
    });
  }
}

/// The file an exec call of the thread `tid` names, as the kernel would
/// reach it: its device and inode, or the errno the call gets.
fn named_file(
  tid: u32,
  dirfd: i32,
  path: u64,
  flags: i32,
) -> Result<FileId, i32> {
  let path = read_cstr(tid, path, libc::PATH_MAX as usize)?;
  let fd = if path.as_bytes().is_empty() {
    if flags & libc::AT_EMPTY_PATH == 0 {
      return Err(libc::ENOENT);
    }
    if dirfd < 0 {
      // the working directory (or no descriptor at all): never a program
      return Err(libc::EACCES);
    }
    // the descriptor's own object, whatever name it has (or none)
    open_path(&format!("/proc/{tid}/fd/{dirfd}"), libc::O_PATH)
      .map_err(|e| if e == libc::ENOENT { libc::EBADF } else { e })?
  } else if let Some(link) = own_proc_link(tid, path.as_bytes()) {
    // the caller's own program or descriptor, through the caller's entry
    open_path(&link, libc::O_PATH)
      .map_err(|e| if e == libc::EPERM { libc::EACCES } else { e })?
  } else {
    let follow = flags & libc::AT_SYMLINK_NOFOLLOW == 0;
    // any other path through a procfs magic link is refused
    resolve(tid, dirfd, path, follow, false)
      .map_err(|e| if e == libc::EPERM { libc::EACCES } else { e })?
  };
  // a folder, a link that isn't followed, a device: nothing to execute
  fstat(&fd).as_ref().and_then(file_id_of).ok_or(libc::EACCES)
}

/// The supervisor's path to what `path` names when it is, word for word,
/// the caller's own program or one of its descriptors: `/proc/self/exe`,
/// `/proc/self/fd/<n>`, and the same beneath `/proc/thread-self` or the
/// caller's own number. These are how a program starts itself again and
/// how `fexecve` falls back; the general lookup ([`resolve`]) refuses
/// every procfs magic link, since resolved here it would name the
/// supervisor's objects. `None`: any other path, or a caller whose root
/// isn't the supervisor's (its `/proc` may be something else).
fn own_proc_link(tid: u32, path: &[u8]) -> Option<String> {
  if !path.starts_with(b"/") {
    return None;
  }
  let parts: Vec<&[u8]> = path
    .split(|b| *b == b'/')
    .filter(|part| !part.is_empty() && *part != b".")
    .collect();
  let number = |part: &[u8]| -> Option<u32> {
    if part.is_empty() || part.len() > 10 || part[0] == b'0' && part.len() > 1 {
      return None;
    }
    std::str::from_utf8(part).ok()?.parse().ok()
  };
  let (who, rest) = match parts.as_slice() {
    [proc, who, rest @ ..] if *proc == b"proc" => (*who, rest),
    _ => return None,
  };
  let own = match who {
    b"self" | b"thread-self" => true,
    other => {
      let pid = number(other)?;
      pid == tid || tgid_of(tid) == Some(pid)
    }
  };
  if !own {
    return None;
  }
  let link = match rest {
    [exe] if *exe == b"exe" => format!("/proc/{tid}/exe"),
    [fd, n] if *fd == b"fd" => format!("/proc/{tid}/fd/{}", number(n)?),
    _ => return None,
  };
  same_root(tid).then_some(link)
}

/// Whether the thread `tid` has the supervisor's root directory.
fn same_root(tid: u32) -> bool {
  let id = |path: &CStr| -> Option<FileId> {
    // SAFETY: a zeroed stat the kernel fills, a NUL-terminated path
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: as above
    let res = unsafe { libc::stat(path.as_ptr(), &mut st) };
    (res == 0).then_some((st.st_dev, st.st_ino))
  };
  let Ok(theirs) = CString::new(format!("/proc/{tid}/root")) else {
    return false;
  };
  match (id(c"/"), id(&theirs)) {
    (Some(ours), Some(theirs)) => ours == theirs,
    _ => false,
  }
}

// ---------------------------------------------------------------------------
// The confined process's side
// ---------------------------------------------------------------------------

/// Set once this process installed the gate's filter and handed its
/// listener over ([`install`]).
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// A channel handed to this process directly (a program that forked its
/// supervisor in place), for [`take_channel`].
static HANDED: Mutex<Option<OwnedFd>> = Mutex::new(None);

/// Whether this process runs under a gate it installed itself.
pub fn installed() -> bool {
  INSTALLED.load(Ordering::Relaxed)
}

/// Records the channel to this process's supervisor, for the plan that is
/// applied next (a program that forks its supervisor in place; a
/// re-executed one finds the channel through [`EXEC_GATE_FD_ENV_VAR`]).
pub fn hand_channel(channel: OwnedFd) {
  if let Ok(mut handed) = HANDED.lock() {
    *handed = Some(channel);
  }
}

/// The channel to the supervisor that answers this process's exec calls,
/// if one was handed over ([`hand_channel`]) or the variable names one
/// (removed here; the descriptor is made close-on-exec). Call while the
/// process has one thread. A variable that doesn't name an open socket is
/// an error: it is never ignored.
pub fn take_channel() -> Result<Option<OwnedFd>, String> {
  let named = match std::env::var_os(EXEC_GATE_FD_ENV_VAR) {
    None => None,
    Some(value) => {
      // SAFETY: called while the process has one thread
      unsafe { std::env::remove_var(EXEC_GATE_FD_ENV_VAR) };
      let fd: i32 = value
        .to_str()
        .and_then(|v| v.parse().ok())
        .filter(|fd| *fd > 2)
        .ok_or_else(|| {
          format!("{EXEC_GATE_FD_ENV_VAR}={value:?} is not a descriptor")
        })?;
      // SAFETY: a zeroed stat is a valid out parameter
      let mut st: libc::stat = unsafe { std::mem::zeroed() };
      // SAFETY: a descriptor number and an out parameter
      if unsafe { libc::fstat(fd, &mut st) } != 0
        || st.st_mode & libc::S_IFMT != libc::S_IFSOCK
      {
        return Err(format!(
          "{EXEC_GATE_FD_ENV_VAR}: descriptor {fd} is not a socket"
        ));
      }
      // SAFETY: an open descriptor this process now owns
      let fd = unsafe { OwnedFd::from_raw_fd(fd) };
      // SAFETY: a valid descriptor
      unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
      Some(fd)
    }
  };
  let handed = HANDED.lock().ok().and_then(|mut handed| handed.take());
  Ok(handed.or(named))
}

/// Installs the gate's filter in this process and hands its listener to
/// the supervisor over `channel` (closed afterwards, like the listener:
/// the confined tree keeps neither). Needs `no_new_privs`; call while the
/// process has one thread, before its own seccomp program. The filter is
/// inherited by everything the process starts.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn install(channel: OwnedFd) -> io::Result<()> {
  let program = crate::notify::notify_program(&exec_syscalls());
  let prog = SockFprog {
    len: (program.len() / 8) as u16,
    filter: program.as_ptr() as *const SockFilter,
  };
  // SAFETY: a plain seccomp call on this (only) thread; the program
  // outlives the call
  let listener = unsafe {
    libc::syscall(
      libc::SYS_seccomp,
      SECCOMP_SET_MODE_FILTER,
      SECCOMP_FILTER_FLAG_NEW_LISTENER,
      &prog as *const SockFprog,
    )
  };
  if listener < 0 {
    return Err(io::Error::last_os_error());
  }
  // from here on every exec of this process waits for the supervisor
  // SAFETY: just created, owned here
  let listener = unsafe { OwnedFd::from_raw_fd(listener as libc::c_int) };
  crate::linux::send_fd(channel.as_raw_fd(), listener.as_raw_fd())?;
  INSTALLED.store(true, Ordering::Relaxed);
  Ok(())
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub fn install(_channel: OwnedFd) -> io::Result<()> {
  Err(io::Error::other("no seccomp program for this architecture"))
}

// ---------------------------------------------------------------------------
// The supervisor's side
// ---------------------------------------------------------------------------

/// Starts the gate's thread in the supervisor: it waits for the confined
/// process to hand its listener over `channel` (it does so as it applies
/// its plan; one that ends first hands nothing) and then answers the
/// tree's exec calls for `files` until the last process under the filter
/// is gone.
pub fn spawn_gate(channel: OwnedFd, files: &[PathBuf]) -> io::Result<()> {
  let gate = ExecGate::new(files);
  std::thread::Builder::new()
    .name("deno-exec-gate".to_string())
    .spawn(move || {
      let listener = loop {
        match crate::linux::recv_fd(&channel) {
          Ok(listener) => break listener,
          // still starting (a loaded machine)
          Err(err) if err.kind() == io::ErrorKind::TimedOut => continue,
          Err(_) => return,
        }
      };
      drop(channel);
      let stop = AtomicBool::new(false);
      gate.serve(listener, &stop);
    })
    .map(|_| ())
}

// ---------------------------------------------------------------------------
// Whether a gate works here
// ---------------------------------------------------------------------------

/// The path the probe's child executes. Nothing is there.
const PROBE_PATH: &CStr = c"/.deno-kernel-sandbox-exec-gate/probe";

/// Whether a supervising process can answer exec calls here: measured once,
/// with a forked child that installs the gate's filter, hands its listener
/// over and makes one exec call, which this process must receive, read from
/// the child's memory and let continue. Not so where a seccomp filter with
/// a listener can't be installed (a kernel without it, an outer profile, a
/// listener already in the filter chain: an outer supervisor) or the
/// child's memory is out of reach (a ptrace policy).
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn available() -> bool {
  static AVAILABLE: OnceLock<bool> = OnceLock::new();
  *AVAILABLE.get_or_init(probe)
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub fn available() -> bool {
  false
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn probe() -> bool {
  let program = crate::notify::notify_program(&exec_syscalls());
  let mut pair = [0 as libc::c_int; 2];
  // SAFETY: an array of two descriptors for the kernel to fill
  if unsafe {
    libc::socketpair(
      libc::AF_UNIX,
      libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
      0,
      pair.as_mut_ptr(),
    )
  } != 0
  {
    return false;
  }
  // SAFETY: just created, owned here
  let (ours, theirs) =
    unsafe { (OwnedFd::from_raw_fd(pair[0]), OwnedFd::from_raw_fd(pair[1])) };
  let theirs_raw = theirs.as_raw_fd();
  let argv: [*const libc::c_char; 2] = [PROBE_PATH.as_ptr(), std::ptr::null()];
  let envp: [*const libc::c_char; 1] = [std::ptr::null()];
  // SAFETY: fork; the child runs only raw system calls on memory prepared
  // before the fork, then _exit
  let pid = unsafe { libc::fork() };
  if pid < 0 {
    return false;
  }
  if pid == 0 {
    // SAFETY: as above
    unsafe {
      if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
        libc::_exit(2);
      }
      let prog = SockFprog {
        len: (program.len() / 8) as u16,
        filter: program.as_ptr() as *const SockFilter,
      };
      let listener = libc::syscall(
        libc::SYS_seccomp,
        SECCOMP_SET_MODE_FILTER,
        SECCOMP_FILTER_FLAG_NEW_LISTENER,
        &prog as *const SockFprog,
      );
      if listener < 0 {
        libc::_exit(3);
      }
      if crate::linux::send_fd(theirs_raw, listener as libc::c_int).is_err() {
        libc::_exit(4);
      }
      libc::close(listener as libc::c_int);
      // answered by this process's parent; the file doesn't exist
      libc::execve(PROBE_PATH.as_ptr(), argv.as_ptr(), envp.as_ptr());
      libc::_exit(0);
    }
  }
  drop(theirs);
  let answered = crate::linux::recv_fd(&ours)
    .ok()
    .is_some_and(|listener| answer_probe(&listener));
  if !answered {
    // a child still waiting for an answer nobody gives
    // SAFETY: our own child
    unsafe { libc::kill(pid, libc::SIGKILL) };
  }
  let mut status = 0;
  // SAFETY: waiting for the child just forked
  unsafe { libc::waitpid(pid, &mut status, 0) };
  answered
}

/// Receives the probe child's one exec call on `listener`, reads the path
/// from the child's memory and lets the call continue.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn answer_probe(listener: &OwnedFd) -> bool {
  let fd = listener.as_raw_fd();
  let mut poll = libc::pollfd {
    fd,
    events: libc::POLLIN,
    revents: 0,
  };
  // SAFETY: one pollfd
  if unsafe { libc::poll(&mut poll, 1, 5_000) } <= 0
    || poll.revents & libc::POLLIN == 0
  {
    return false;
  }
  // SAFETY: the kernel wants a zeroed request
  let mut req: libc::seccomp_notif = unsafe { std::mem::zeroed() };
  // SAFETY: a valid listener and request
  if unsafe { libc::ioctl(fd, libc::SECCOMP_IOCTL_NOTIF_RECV as _, &mut req) }
    != 0
  {
    return false;
  }
  let read = req.data.nr as u32 == NR_EXECVE
    && read_cstr(req.pid, req.data.args[0], libc::PATH_MAX as usize)
      .is_ok_and(|path| path.as_c_str() == PROBE_PATH);
  let resp = libc::seccomp_notif_resp {
    id: req.id,
    val: 0,
    error: 0,
    flags: SECCOMP_USER_NOTIF_FLAG_CONTINUE,
  };
  // SAFETY: a valid listener and response
  let sent =
    unsafe { libc::ioctl(fd, libc::SECCOMP_IOCTL_NOTIF_SEND as _, &resp) } == 0;
  read && sent
}

#[cfg(test)]
mod tests {
  use std::io::Read;
  use std::os::fd::IntoRawFd;
  use std::path::Path;

  use super::*;

  fn tid() -> u32 {
    // SAFETY: a call with no arguments
    unsafe { libc::syscall(libc::SYS_gettid) as u32 }
  }

  fn canonical(path: &str) -> PathBuf {
    std::fs::canonicalize(path).unwrap()
  }

  /// A scratch folder removed when the test ends.
  struct Scratch(PathBuf);

  impl Scratch {
    fn new(name: &str) -> Self {
      let dir = std::env::temp_dir()
        .join(format!("deno-exec-gate-{name}-{}", std::process::id()));
      std::fs::create_dir_all(&dir).unwrap();
      Self(std::fs::canonicalize(dir).unwrap())
    }
  }

  impl Drop for Scratch {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }

  fn cstr(path: &Path) -> CString {
    CString::new(path.as_os_str().as_bytes()).unwrap()
  }

  /// `execve(path)` as this thread would make it.
  fn execve(path: &CStr) -> Call {
    Call {
      tid: tid(),
      nr: NR_EXECVE,
      args: [path.as_ptr() as u64, 0, 0, 0, 0, 0],
    }
  }

  /// `execveat(dirfd, path, .., flags)` as this thread would make it.
  fn execveat(dirfd: i32, path: &CStr, flags: i32) -> Call {
    Call {
      tid: tid(),
      nr: NR_EXECVEAT,
      args: [
        dirfd as u32 as u64,
        path.as_ptr() as u64,
        0,
        0,
        flags as u32 as u64,
        0,
      ],
    }
  }

  const REFUSED: Answer = Answer::Done(Err(libc::EACCES));

  fn open(path: &Path, flags: libc::c_int) -> OwnedFd {
    let path = cstr(path);
    // SAFETY: a NUL-terminated path
    let fd = unsafe { libc::open(path.as_ptr(), flags | libc::O_CLOEXEC) };
    assert!(fd >= 0, "{}", io::Error::last_os_error());
    // SAFETY: just opened
    unsafe { OwnedFd::from_raw_fd(fd) }
  }

  /// An anonymous memory file holding a copy of `file`.
  fn memory_copy(file: &Path) -> OwnedFd {
    // SAFETY: a NUL-terminated name
    let fd = unsafe { libc::memfd_create(c"copy".as_ptr(), libc::MFD_CLOEXEC) };
    assert!(fd >= 0, "{}", io::Error::last_os_error());
    // SAFETY: just created
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let bytes = std::fs::read(file).unwrap();
    let mut out = std::fs::File::from(fd.try_clone().unwrap());
    std::io::Write::write_all(&mut out, &bytes).unwrap();
    fd
  }

  #[test]
  fn the_callers_own_proc_links_are_named_word_for_word() {
    let tid = tid();
    let pid = std::process::id();
    let exe = Some(format!("/proc/{tid}/exe"));
    assert_eq!(own_proc_link(tid, b"/proc/self/exe"), exe);
    assert_eq!(own_proc_link(tid, b"//proc/./self//exe"), exe);
    assert_eq!(own_proc_link(tid, b"/proc/thread-self/exe"), exe);
    assert_eq!(
      own_proc_link(tid, format!("/proc/{pid}/exe").as_bytes()),
      exe
    );
    assert_eq!(
      own_proc_link(tid, b"/proc/self/fd/7"),
      Some(format!("/proc/{tid}/fd/7"))
    );
    for other in [
      &b"proc/self/exe"[..],
      b"/proc/1/exe",
      b"/proc/self/cwd",
      b"/proc/self/fd",
      b"/proc/self/fd/07",
      b"/proc/self/fd/-1",
      b"/proc/self/fd/7/x",
      b"/proc/self/../self/exe",
      b"/proc/self/task/1/exe",
      b"/dev/fd/7",
      b"/bin/sh",
    ] {
      assert_eq!(
        own_proc_link(tid, other),
        None,
        "{}",
        String::from_utf8_lossy(other)
      );
    }
  }

  #[test]
  fn a_call_continues_only_for_a_listed_file() {
    let scratch = Scratch::new("listed");
    let sh = canonical("/bin/sh");
    let gate = ExecGate::new(std::slice::from_ref(&sh));
    assert_eq!(gate.answer(&execve(&cstr(&sh))), Answer::Continue);
    // by any name that reaches the same file
    assert_eq!(gate.answer(&execve(c"/bin/sh")), Answer::Continue);
    let link = scratch.0.join("link");
    std::os::unix::fs::symlink(&sh, &link).unwrap();
    assert_eq!(gate.answer(&execve(&cstr(&link))), Answer::Continue);
    // a link that isn't followed is not the program
    assert_eq!(
      gate.answer(&execveat(AT_FDCWD, &cstr(&link), libc::AT_SYMLINK_NOFOLLOW)),
      REFUSED
    );
    // relative to a directory descriptor
    let dir = open(sh.parent().unwrap(), libc::O_PATH | libc::O_DIRECTORY);
    let name = CString::new(sh.file_name().unwrap().as_bytes()).unwrap();
    assert_eq!(
      gate.answer(&execveat(dir.as_raw_fd(), &name, 0)),
      Answer::Continue
    );
    // another file, a copy of the program included
    let copy = scratch.0.join("copy");
    std::fs::copy(&sh, &copy).unwrap();
    assert_eq!(gate.answer(&execve(&cstr(&copy))), REFUSED);
    // a folder, a missing file, an unreadable argument
    assert_eq!(gate.answer(&execve(c"/")), REFUSED);
    assert_eq!(
      gate.answer(&execve(&cstr(&scratch.0.join("missing")))),
      Answer::Done(Err(libc::ENOENT))
    );
    let mut null = execve(c"/bin/sh");
    null.args[0] = 0;
    assert_eq!(gate.answer(&null), Answer::Done(Err(libc::EFAULT)));
    // a call the gate doesn't know
    let mut other = execve(c"/bin/sh");
    other.nr = u32::MAX;
    assert_eq!(gate.answer(&other), Answer::Done(Err(libc::ENOSYS)));
    // nothing listed: nothing continues
    assert_eq!(ExecGate::default().answer(&execve(c"/bin/sh")), REFUSED);
  }

  #[test]
  fn an_interpreter_the_kernel_opens_is_not_a_target() {
    let scratch = Scratch::new("interpreter");
    // a script: its `#!` interpreter carries an execute rule, and no call
    // may name it
    let script = scratch.0.join("tool");
    std::fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
    let sh = canonical("/bin/sh");
    let parts =
      crate::exec::program_closure_parts(std::slice::from_ref(&script), None);
    assert!(parts.files.contains(&sh), "{parts:?}");
    let gate = ExecGate::new(&parts.targets);
    assert_eq!(gate.answer(&execve(&cstr(&script))), Answer::Continue);
    assert_eq!(gate.answer(&execve(&cstr(&sh))), REFUSED);
    // a dynamically linked program: the same for its loader
    let parts =
      crate::exec::program_closure_parts(std::slice::from_ref(&sh), None);
    let gate = ExecGate::new(&parts.targets);
    assert_eq!(gate.answer(&execve(&cstr(&sh))), Answer::Continue);
    if let Some(loader) = crate::exec::elf_interpreter(&sh) {
      let loader = std::fs::canonicalize(loader).unwrap();
      assert!(parts.files.contains(&loader), "{parts:?}");
      assert_eq!(gate.answer(&execve(&cstr(&loader))), REFUSED);
    }
  }

  #[test]
  fn a_descriptor_is_held_to_the_file_it_is() {
    let sh = canonical("/bin/sh");
    let gate = ExecGate::new(std::slice::from_ref(&sh));
    let empty = libc::AT_EMPTY_PATH;
    // the listed program, by descriptor and by the descriptor's proc name
    let program = open(&sh, libc::O_RDONLY);
    let raw = program.as_raw_fd();
    assert_eq!(gate.answer(&execveat(raw, c"", empty)), Answer::Continue);
    let name = CString::new(format!("/proc/self/fd/{raw}")).unwrap();
    assert_eq!(gate.answer(&execve(&name)), Answer::Continue);
    // an empty path needs its flag, and a descriptor
    assert_eq!(
      gate.answer(&execveat(raw, c"", 0)),
      Answer::Done(Err(libc::ENOENT))
    );
    assert_eq!(gate.answer(&execveat(AT_FDCWD, c"", empty)), REFUSED);
    // a file with no path, though it holds the same bytes
    let memory = memory_copy(&sh);
    let raw = memory.as_raw_fd();
    assert_eq!(gate.answer(&execveat(raw, c"", empty)), REFUSED);
    let name = CString::new(format!("/proc/self/fd/{raw}")).unwrap();
    assert_eq!(gate.answer(&execve(&name)), REFUSED);
    // a descriptor that isn't open
    let closed = memory.into_raw_fd();
    // SAFETY: closing the descriptor just released
    unsafe { libc::close(closed) };
    assert_eq!(
      gate.answer(&execveat(closed, c"", empty)),
      Answer::Done(Err(libc::EBADF))
    );
  }

  #[test]
  fn a_program_starts_itself_again_only_when_listed() {
    let exe = std::env::current_exe().unwrap().canonicalize().unwrap();
    let gate = ExecGate::new(std::slice::from_ref(&exe));
    assert_eq!(gate.answer(&execve(c"/proc/self/exe")), Answer::Continue);
    let gate = ExecGate::new(&[canonical("/bin/sh")]);
    assert_eq!(gate.answer(&execve(c"/proc/self/exe")), REFUSED);
    // any other procfs magic link is refused, whatever it leads to
    let gate = ExecGate::new(std::slice::from_ref(&exe));
    assert_eq!(gate.answer(&execve(c"/proc/self/cwd/x")), REFUSED);
    assert_eq!(gate.answer(&execve(c"/proc/self/root/bin/sh")), REFUSED);
  }

  /// A forked child under the gate's filter: it makes each exec call of
  /// `refused` (reporting the errno through a pipe), then executes
  /// `/bin/sh -c "exit 7"`. Returns the errnos and the child's exit code;
  /// `None` where the filter can't be installed.
  fn run_gated(gate: ExecGate, refused: &[&CStr]) -> Option<(Vec<i32>, i32)> {
    let program = crate::notify::notify_program(&exec_syscalls());
    let mut pair = [0; 2];
    let mut out = [0; 2];
    // SAFETY: out arrays of two descriptors
    unsafe {
      assert_eq!(
        libc::socketpair(
          libc::AF_UNIX,
          libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
          0,
          pair.as_mut_ptr()
        ),
        0
      );
      assert_eq!(libc::pipe2(out.as_mut_ptr(), libc::O_CLOEXEC), 0);
    }
    let sh = c"/bin/sh";
    let argv = [
      sh.as_ptr(),
      c"-c".as_ptr(),
      c"exit 7".as_ptr(),
      std::ptr::null(),
    ];
    let envp: [*const libc::c_char; 1] = [std::ptr::null()];
    // SAFETY: the child runs raw syscalls on memory prepared before the fork
    let pid = unsafe { libc::fork() };
    if pid == 0 {
      // SAFETY: as above
      unsafe {
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
        let prog = SockFprog {
          len: (program.len() / 8) as u16,
          filter: program.as_ptr() as *const SockFilter,
        };
        let listener = libc::syscall(
          libc::SYS_seccomp,
          SECCOMP_SET_MODE_FILTER,
          SECCOMP_FILTER_FLAG_NEW_LISTENER,
          &prog as *const SockFprog,
        );
        if listener < 0 {
          libc::_exit(3);
        }
        if crate::linux::send_fd(pair[1], listener as libc::c_int).is_err() {
          libc::_exit(4);
        }
        libc::close(listener as libc::c_int);
        for path in refused {
          let one = [path.as_ptr(), std::ptr::null()];
          libc::execve(path.as_ptr(), one.as_ptr(), envp.as_ptr());
          let errno = *libc::__errno_location();
          libc::write(out[1], (&errno as *const i32).cast(), 4);
        }
        libc::execve(sh.as_ptr(), argv.as_ptr(), envp.as_ptr());
        libc::_exit(5);
      }
    }
    assert!(pid > 0);
    // SAFETY: our ends of the pair and the pipe
    let (parent, reader) = unsafe {
      libc::close(pair[1]);
      libc::close(out[1]);
      (OwnedFd::from_raw_fd(pair[0]), OwnedFd::from_raw_fd(out[0]))
    };
    let listener = crate::linux::recv_fd(&parent).ok();
    let mut results = Vec::new();
    if let Some(listener) = listener {
      let stop = AtomicBool::new(false);
      std::thread::scope(|scope| {
        scope.spawn(|| gate.serve(listener, &stop));
        let mut bytes = Vec::new();
        std::fs::File::from(reader).read_to_end(&mut bytes).unwrap();
        for chunk in bytes.as_chunks::<4>().0 {
          results.push(i32::from_ne_bytes(*chunk));
        }
        // the pipe closed: the child executed the shell, or ended
        let mut status = 0;
        // SAFETY: our own child, reaped once
        unsafe { libc::waitpid(pid, &mut status, 0) };
        stop.store(true, Ordering::Relaxed);
        assert!(libc::WIFEXITED(status), "{status:#x}");
        Some((results, libc::WEXITSTATUS(status)))
      })
    } else {
      let mut status = 0;
      // SAFETY: our own child, reaped once
      unsafe { libc::waitpid(pid, &mut status, 0) };
      assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 3,
        "the gated child failed: {status:#x}"
      );
      None
    }
  }

  #[test]
  #[allow(clippy::print_stderr, reason = "test code: a skip note")]
  fn a_gated_process_starts_listed_programs_only() {
    if !available() {
      eprintln!("skipped: no seccomp listener can be installed here");
      return;
    }
    let sh = canonical("/bin/sh");
    let parts =
      crate::exec::program_closure_parts(std::slice::from_ref(&sh), None);
    let loader = crate::exec::elf_interpreter(&sh)
      .map(|loader| cstr(&std::fs::canonicalize(loader).unwrap()));
    let mut refused: Vec<&CStr> = vec![c"/proc/self/exe", c"/"];
    if let Some(loader) = &loader {
      refused.push(loader);
    }
    let (errnos, code) = run_gated(ExecGate::new(&parts.targets), &refused)
      .expect("the probe said a listener can be installed");
    assert_eq!(errnos, vec![libc::EACCES; refused.len()]);
    // the listed program ran, loaded by the kernel through its loader
    assert_eq!(code, 7);
  }

  #[test]
  fn the_probe_answers_the_same_twice() {
    assert_eq!(available(), available());
  }

  #[test]
  fn a_variable_that_names_no_socket_is_never_ignored() {
    // (the variable is read in a forked child: the test process's own
    // environment stays as it is)
    let check = |value: &CStr| -> i32 {
      // SAFETY: fork; the child sets one variable and reads it back
      let pid = unsafe { libc::fork() };
      assert!(pid >= 0);
      if pid == 0 {
        // SAFETY: the child's own environment, one thread
        unsafe {
          libc::setenv(
            c"DENO_INTERNAL_KERNEL_EXEC_GATE_FD".as_ptr(),
            value.as_ptr(),
            1,
          );
          let code = match take_channel() {
            Ok(Some(_)) => 0,
            Ok(None) => 1,
            Err(_) => 2,
          };
          libc::_exit(code);
        }
      }
      let mut status = 0;
      // SAFETY: our own child
      unsafe { libc::waitpid(pid, &mut status, 0) };
      libc::WEXITSTATUS(status)
    };
    assert_eq!(check(c"not-a-number"), 2);
    assert_eq!(check(c"1"), 2);
    // stdin's number plus a large offset: not open
    assert_eq!(check(c"987654"), 2);
  }
}
