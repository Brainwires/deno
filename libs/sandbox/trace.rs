// Copyright 2018-2026 the Deno authors. MIT license.

//! The tracing observer of an explicit `--kernel-sandbox=warn` on Linux
//! (denext; `would_deny::Observer::SupervisorTrace`).
//!
//! Landlock has no permissive mode, so on Linux nothing in the kernel can
//! say what native code and child processes *would* be refused. A
//! supervising process can: the run's process tree gets a seccomp filter
//! that hands its file, exec and socket-address system calls to the
//! supervisor ([`crate::supervise::run_traced`], the same exec-then-fork
//! shape as the broker's supervisor). The [`Tracer`] looks at each one,
//! reports what the `enforce` plan of the same grant would have refused
//! ([`crate::would_deny::TracePolicy`]), and always lets the kernel run the
//! call as it was made (`SECCOMP_USER_NOTIF_FLAG_CONTINUE`). Nothing is
//! refused and no result is changed.
//!
//! What it costs, and why only a `warn` asked for by name gets it:
//!
//! - the filter needs `no_new_privs`, so setuid programs in the tree
//!   (`sudo`, `ping` on old systems) don't gain privileges, as under
//!   `enforce`;
//! - a filter chain holds one listener: a program in the tree that installs
//!   a seccomp user-notification listener of its own fails with `EBUSY`;
//! - every traced call waits for the supervisor.
//!
//! It is an observer, never a boundary: the answers are made from the
//! caller's memory, which the caller can change afterwards. That is fine
//! for a report and the reason nothing here decides an access.
//!
//! Reporting starts when the run says its user code is about to start
//! ([`signal_start`], where `enforce` would confine the process): what the
//! runtime does before that (finding its configuration, loading and
//! checking the module graph, fetching) is what an enforced run's first
//! image does unconfined.

#![allow(
  clippy::disallowed_methods,
  reason = "the tracer reads the real /proc and file system, never a virtual one"
)]

use std::ffi::CString;
use std::io;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::Ipv6Addr;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use crate::notify::AT_FDCWD;
use crate::notify::Answer;
use crate::notify::Call;
use crate::notify::read_cstr;
use crate::notify::read_exact;
use crate::notify::resolve;
use crate::notify::tgid_of;
use crate::seccomp::SockFilter;
use crate::seccomp::SockFprog;
use crate::would_deny;
use crate::would_deny::TracePolicy;
use crate::would_deny::WouldDeny;

/// Set for the process tree a tracing supervisor started:
/// `<pid>:<observed>`, the process the supervisor's child executed and
/// whether the tracing filter is in place (`1`) or couldn't be installed
/// (`0`). That process reports from its own decision points and tells the
/// supervisor when its user code starts; nothing in the tree starts a
/// second observer (a filter chain holds one listener). Setting it by hand
/// only turns the observer off.
pub const TRACED_ENV_VAR: &str = "DENO_INTERNAL_KERNEL_SANDBOX_TRACED";

/// The value of [`TRACED_ENV_VAR`] for the run this process is about to
/// execute as (called by the supervisor's forked child).
pub fn traced_env_value(observed: bool) -> String {
  format!("{}:{}", std::process::id(), u8::from(observed))
}

/// How this process stands to a tracing supervisor ([`TRACED_ENV_VAR`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TracedRun {
  /// The run the supervisor started; `observed`: the filter is in place.
  Root { observed: bool },
  /// A process that run started (the supervisor sees it too, when the
  /// filter is in place).
  Inside,
}

/// See [`TracedRun`]; `None`: no tracing supervisor started this tree.
pub fn traced_run() -> Option<TracedRun> {
  parse_traced(
    std::env::var_os(TRACED_ENV_VAR)?.to_str()?,
    std::process::id(),
  )
}

fn parse_traced(value: &str, own_pid: u32) -> Option<TracedRun> {
  let (pid, observed) = value.split_once(':')?;
  let pid: u32 = pid.parse().ok()?;
  let observed = match observed {
    "1" => true,
    "0" => false,
    _ => return None,
  };
  Some(if pid == own_pid {
    TracedRun::Root { observed }
  } else {
    TracedRun::Inside
  })
}

/// The path a traced run opens to say its user code is about to start
/// ([`signal_start`]). Nothing is there; the tracer sees the attempt.
pub const START_SENTINEL: &str = "/.deno-kernel-sandbox-trace/start";

const SECCOMP_SET_MODE_FILTER: libc::c_ulong = 1;
const SECCOMP_FILTER_FLAG_NEW_LISTENER: libc::c_ulong = 1 << 3;

/// The system calls the tracer looks at: opens, the calls that make, remove
/// or rename a name, `execve`, and `connect` / `bind` (their addresses).
pub fn traced_syscalls() -> Vec<u32> {
  #[allow(unused_mut, reason = "x86_64 adds its legacy calls")]
  let mut out = vec![
    libc::SYS_openat,
    libc::SYS_openat2,
    libc::SYS_execve,
    libc::SYS_execveat,
    libc::SYS_connect,
    libc::SYS_bind,
    libc::SYS_mkdirat,
    libc::SYS_unlinkat,
    libc::SYS_renameat,
    libc::SYS_renameat2,
    libc::SYS_symlinkat,
    libc::SYS_linkat,
    libc::SYS_truncate,
  ];
  #[cfg(target_arch = "x86_64")]
  out.extend([
    libc::SYS_open,
    libc::SYS_creat,
    libc::SYS_mkdir,
    libc::SYS_unlink,
    libc::SYS_rmdir,
    libc::SYS_rename,
    libc::SYS_symlink,
    libc::SYS_link,
  ]);
  out.into_iter().map(|nr| nr as u32).collect()
}

/// Installs the tracing filter in this process (`no_new_privs`, then the
/// filter with a listener) and returns the listener. Called by the forked
/// child before it executes the run, while it has one thread; the filter
/// and the flag are inherited by everything the run starts.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn install_listener() -> io::Result<OwnedFd> {
  let program = crate::notify::notify_program(&traced_syscalls());
  let prog = SockFprog {
    len: (program.len() / 8) as u16,
    filter: program.as_ptr() as *const SockFilter,
  };
  // SAFETY: plain prctl / seccomp calls on this (only) thread; the program
  // outlives the call
  unsafe {
    if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
      return Err(io::Error::last_os_error());
    }
    let listener = libc::syscall(
      libc::SYS_seccomp,
      SECCOMP_SET_MODE_FILTER,
      SECCOMP_FILTER_FLAG_NEW_LISTENER,
      &prog as *const SockFprog,
    );
    if listener < 0 {
      return Err(io::Error::last_os_error());
    }
    Ok(OwnedFd::from_raw_fd(listener as libc::c_int))
  }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub fn install_listener() -> io::Result<OwnedFd> {
  Err(io::Error::other("no seccomp program for this architecture"))
}

/// Tells the tracing supervisor that user code is about to start: reports
/// begin here. Opens [`START_SENTINEL`], which doesn't exist; the tracer
/// sees the call before the kernel answers it, so every access after this
/// returns is observed.
pub fn signal_start() {
  let _ = std::fs::File::open(START_SENTINEL);
}

/// What a traced system call names.
enum Name {
  /// The object itself (an open of something that exists, `truncate`).
  Object,
  /// The folder the name is in (creating, removing, renaming).
  Parent,
}

/// Looks at the traced calls of one process tree and reports what the
/// `enforce` plan would have refused.
pub struct Tracer {
  policy: TracePolicy,
  started: AtomicBool,
  /// This runtime's own file: a caller running it has the broker under
  /// `enforce`.
  exe: Option<PathBuf>,
  /// The run's own `/proc` entry (`/proc/<pid>` of the process the
  /// supervisor forked): what the plan's `/proc/self` rule would name. A
  /// child's own entry is another, which the rule doesn't cover (as under
  /// `enforce`).
  own_proc: PathBuf,
}

impl Tracer {
  /// A tracer for the tree of process `root` (the forked run).
  pub fn new(policy: TracePolicy, root: u32) -> Self {
    Self {
      policy,
      started: AtomicBool::new(false),
      exe: std::fs::read_link("/proc/self/exe").ok(),
      own_proc: PathBuf::from(format!("/proc/{root}")),
    }
  }

  /// Whether the run said its user code started.
  pub fn started(&self) -> bool {
    self.started.load(Ordering::Relaxed)
  }

  /// A tracer that takes over from one whose run already started.
  pub fn mark_started(&self) {
    self.started.store(true, Ordering::SeqCst);
  }

  /// The answer to every traced call: the kernel runs it as it was made.
  pub fn answer(&self, c: &Call) -> Answer {
    self.observe(c);
    Answer::Continue
  }

  fn observe(&self, c: &Call) {
    let a = c.args;
    let int = |i: usize| a[i] as u32 as i32;
    let nr = c.nr as libc::c_long;
    // before the run starts only the sentinel matters, and only an open
    // can be it
    if !self.started() {
      let path = match nr {
        libc::SYS_openat | libc::SYS_openat2 => a[1],
        #[cfg(target_arch = "x86_64")]
        libc::SYS_open => a[0],
        _ => return,
      };
      if read_cstr(c.tid, path, libc::PATH_MAX as usize)
        .is_ok_and(|p| p.as_bytes() == START_SENTINEL.as_bytes())
      {
        self.started.store(true, Ordering::SeqCst);
      }
      return;
    }
    #[cfg(target_arch = "x86_64")]
    let wronly_create = libc::O_CREAT | libc::O_WRONLY | libc::O_TRUNC;
    match nr {
      libc::SYS_openat => self.open(c, int(0), a[1], int(2)),
      libc::SYS_openat2 => {
        // struct open_how { u64 flags; u64 mode; u64 resolve; }
        let mut flags = [0u8; 8];
        if read_exact(c.tid, a[2], &mut flags).is_ok() {
          let flags = u64::from_ne_bytes(flags) as u32 as i32;
          self.open(c, int(0), a[1], flags);
        }
      }
      libc::SYS_execve => self.exec(c, AT_FDCWD, a[0]),
      libc::SYS_execveat => self.exec(c, int(0), a[1]),
      libc::SYS_connect => self.address(c, a[1], a[2], true),
      libc::SYS_bind => self.address(c, a[1], a[2], false),
      libc::SYS_mkdirat | libc::SYS_unlinkat => {
        self.write(c, int(0), a[1], Name::Parent)
      }
      libc::SYS_renameat | libc::SYS_renameat2 => {
        self.write(c, int(0), a[1], Name::Parent);
        self.write(c, int(2), a[3], Name::Parent);
      }
      libc::SYS_symlinkat => self.write(c, int(1), a[2], Name::Parent),
      libc::SYS_linkat => self.write(c, int(2), a[3], Name::Parent),
      libc::SYS_truncate => self.write(c, AT_FDCWD, a[0], Name::Object),
      #[cfg(target_arch = "x86_64")]
      libc::SYS_open => self.open(c, AT_FDCWD, a[0], int(1)),
      #[cfg(target_arch = "x86_64")]
      libc::SYS_creat => self.open(c, AT_FDCWD, a[0], wronly_create),
      #[cfg(target_arch = "x86_64")]
      libc::SYS_mkdir | libc::SYS_unlink | libc::SYS_rmdir => {
        self.write(c, AT_FDCWD, a[0], Name::Parent)
      }
      #[cfg(target_arch = "x86_64")]
      libc::SYS_rename => {
        self.write(c, AT_FDCWD, a[0], Name::Parent);
        self.write(c, AT_FDCWD, a[1], Name::Parent);
      }
      #[cfg(target_arch = "x86_64")]
      libc::SYS_symlink | libc::SYS_link => {
        self.write(c, AT_FDCWD, a[1], Name::Parent)
      }
      _ => {}
    }
  }

  /// The process a thread belongs to, its name, and whether it runs this
  /// runtime.
  fn caller(&self, tid: u32) -> (u32, Option<String>, bool) {
    let pid = tgid_of(tid).unwrap_or(tid);
    let name = std::fs::read_to_string(format!("/proc/{pid}/comm"))
      .ok()
      .map(|name| name.trim_end().to_string())
      .filter(|name| !name.is_empty());
    let runtime = self.exe.is_some()
      && std::fs::read_link(format!("/proc/{pid}/exe")).ok() == self.exe;
    (pid, name, runtime)
  }

  fn report(&self, tid: u32, event: Option<WouldDeny>, op: &str) {
    if let Some(event) = event {
      let (pid, name, _) = self.caller(tid);
      would_deny::report(event.by(pid, name.as_deref()).with_op(op));
    }
  }

  /// The real path of the caller's `path`: the object's own, or for a name
  /// that may not exist the folder's with the name appended. `None`: it
  /// doesn't resolve (nothing to report), or it has no path (a pipe, a
  /// socket, an unlinked file).
  fn real_path(
    &self,
    tid: u32,
    dirfd: i32,
    path: &CString,
    follow: bool,
    name: Name,
  ) -> Option<PathBuf> {
    let real = |fd: &OwnedFd| -> Option<PathBuf> {
      let link =
        std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd())).ok()?;
      let bytes = link.into_os_string().into_vec();
      if !bytes.starts_with(b"/") || bytes.ends_with(b" (deleted)") {
        return None;
      }
      let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
      // the run's own entry, as the plan's rule names it
      Some(match path.strip_prefix(&self.own_proc) {
        Ok(rest) => Path::new("/proc/self").join(rest),
        Err(_) => path,
      })
    };
    match name {
      Name::Object => {
        real(&resolve(tid, dirfd, path.clone(), follow, false).ok()?)
      }
      Name::Parent => {
        let bytes = path.as_bytes();
        // ("/" or empty: no name to make or remove)
        let last = bytes.iter().rposition(|b| *b != b'/')?;
        let trimmed = &bytes[..=last];
        let (parent, leaf) = match trimmed.iter().rposition(|b| *b == b'/') {
          Some(0) => (&b"/"[..], &trimmed[1..]),
          Some(slash) => (&trimmed[..slash], &trimmed[slash + 1..]),
          None => (&b"."[..], trimmed),
        };
        if leaf == b"." || leaf == b".." {
          return None;
        }
        let parent = CString::new(parent).ok()?;
        let folder = real(&resolve(tid, dirfd, parent, true, false).ok()?)?;
        Some(folder.join(std::ffi::OsString::from_vec(leaf.to_vec())))
      }
    }
  }

  fn open(&self, c: &Call, dirfd: i32, path: u64, flags: i32) {
    // an O_PATH open reads and writes nothing (Landlock doesn't hold it)
    if flags & libc::O_PATH != 0 {
      return;
    }
    let Ok(path) = read_cstr(c.tid, path, libc::PATH_MAX as usize) else {
      return;
    };
    let mode = flags & libc::O_ACCMODE;
    let reads = mode != libc::O_WRONLY;
    let writes = mode != libc::O_RDONLY || flags & libc::O_TRUNC != 0;
    let follow = flags & libc::O_NOFOLLOW == 0;
    let (_, _, runtime) = self.caller(c.tid);
    match self.real_path(c.tid, dirfd, &path, follow, Name::Object) {
      Some(real) => {
        if reads {
          // a folder opened to list it needs only Landlock's READ_DIR,
          // which the folders the module graph listed have
          let event = if std::fs::metadata(&real).is_ok_and(|m| m.is_dir()) {
            self.policy.list_event(&real)
          } else {
            self.policy.read_event(&real)
          };
          self.report(c.tid, event, "open");
        }
        if writes {
          self.report(c.tid, self.policy.write_event(&real, runtime), "open");
        }
      }
      // nothing there: an O_CREAT makes the name in its folder
      None if flags & libc::O_CREAT != 0 => {
        if let Some(real) =
          self.real_path(c.tid, dirfd, &path, follow, Name::Parent)
        {
          self.report(c.tid, self.policy.write_event(&real, runtime), "open");
        }
      }
      None => {}
    }
  }

  fn write(&self, c: &Call, dirfd: i32, path: u64, name: Name) {
    let Ok(path) = read_cstr(c.tid, path, libc::PATH_MAX as usize) else {
      return;
    };
    let (_, _, runtime) = self.caller(c.tid);
    if let Some(real) = self.real_path(c.tid, dirfd, &path, true, name) {
      self.report(c.tid, self.policy.write_event(&real, runtime), "write");
    }
  }

  fn exec(&self, c: &Call, dirfd: i32, path: u64) {
    if !self.policy.refuse_exec && self.policy.exec_only.is_none() {
      return;
    }
    let Ok(path) = read_cstr(c.tid, path, libc::PATH_MAX as usize) else {
      return;
    };
    if let Some(real) = self.real_path(c.tid, dirfd, &path, true, Name::Object)
    {
      self.report(c.tid, self.policy.exec_event(&real), "execve");
    }
  }

  /// `connect` / `bind`: the address behind the pointer.
  fn address(&self, c: &Call, addr: u64, len: u64, connect: bool) {
    let len = (len as usize).min(128);
    if len < 2 {
      return;
    }
    let mut buf = vec![0u8; len];
    if read_exact(c.tid, addr, &mut buf).is_err() {
      return;
    }
    let family = u16::from_ne_bytes([buf[0], buf[1]]) as libc::c_int;
    let (_, _, runtime) = self.caller(c.tid);
    let op = if connect { "connect" } else { "bind" };
    let port = |buf: &[u8]| u16::from_be_bytes([buf[2], buf[3]]);
    let ip_event = |ip: IpAddr, port: u16| {
      if connect {
        self.policy.connect_event(ip, port, runtime)
      } else {
        self.policy.bind_event(port, runtime)
      }
    };
    match family {
      libc::AF_INET if len >= 8 => {
        let ip = Ipv4Addr::new(buf[4], buf[5], buf[6], buf[7]);
        self.report(c.tid, ip_event(IpAddr::V4(ip), port(&buf)), op);
      }
      libc::AF_INET6 if len >= 24 => {
        let mut octets = [0u8; 16];
        octets.copy_from_slice(&buf[8..24]);
        let ip = Ipv6Addr::from(octets);
        self.report(c.tid, ip_event(IpAddr::V6(ip), port(&buf)), op);
      }
      // a socket by path (an abstract one starts with a NUL byte)
      libc::AF_UNIX if len > 2 && buf[2] != 0 => {
        let end = buf[2..].iter().position(|b| *b == 0).unwrap_or(len - 2);
        let path =
          PathBuf::from(std::ffi::OsString::from_vec(buf[2..2 + end].to_vec()));
        self.report(
          c.tid,
          self.policy.unix_event(Path::new(&path), runtime),
          op,
        );
      }
      _ => {}
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_traced_variable_names_the_run_and_its_state() {
    assert_eq!(
      parse_traced("42:1", 42),
      Some(TracedRun::Root { observed: true })
    );
    assert_eq!(
      parse_traced("42:0", 42),
      Some(TracedRun::Root { observed: false })
    );
    // a process the run started
    assert_eq!(parse_traced("42:1", 43), Some(TracedRun::Inside));
    for bad in ["", "1", "42", "42:", "42:2", "x:1", ":1"] {
      assert_eq!(parse_traced(bad, 42), None, "{bad:?}");
    }
    let value = traced_env_value(true);
    assert_eq!(
      parse_traced(&value, std::process::id()),
      Some(TracedRun::Root { observed: true })
    );
  }

  #[test]
  fn the_traced_list_has_each_call_once() {
    let mut list = traced_syscalls();
    let n = list.len();
    list.sort_unstable();
    list.dedup();
    assert_eq!(list.len(), n);
    assert!(list.contains(&(libc::SYS_openat as u32)));
    assert!(list.contains(&(libc::SYS_execve as u32)));
    assert!(list.contains(&(libc::SYS_connect as u32)));
  }

  /// A forked child installs the filter and makes calls; this process
  /// answers them with a [`Tracer`] and checks what it reported. Skipped
  /// where a filter can't be installed (an outer seccomp profile).
  #[test]
  fn a_traced_child_reads_and_is_reported() {
    use std::os::unix::ffi::OsStrExt;
    let dir = std::env::temp_dir()
      .join(format!("deno-trace-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("granted")).unwrap();
    let dir = dir.canonicalize().unwrap();
    let inside = dir.join("granted/in.txt");
    let outside = dir.join("outside.txt");
    let early = dir.join("early.txt");
    std::fs::write(&inside, b"in").unwrap();
    std::fs::write(&outside, b"out").unwrap();
    std::fs::write(&early, b"early").unwrap();
    let c_inside = CString::new(inside.as_os_str().as_bytes()).unwrap();
    let c_outside = CString::new(outside.as_os_str().as_bytes()).unwrap();
    let c_early = CString::new(early.as_os_str().as_bytes()).unwrap();
    let c_sentinel = CString::new(START_SENTINEL).unwrap();
    let mut pair = [0 as libc::c_int; 2];
    // SAFETY: a socketpair into a two-element array
    let made = unsafe {
      libc::socketpair(
        libc::AF_UNIX,
        libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
        0,
        pair.as_mut_ptr(),
      )
    };
    assert_eq!(made, 0);
    // SAFETY: fork; the child makes only async-signal-safe calls
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
      // SAFETY: the forked child: plain system calls, then _exit
      unsafe {
        let Ok(listener) = install_listener() else {
          libc::_exit(77);
        };
        if crate::linux::send_fd(pair[1], listener.as_raw_fd()).is_err() {
          libc::_exit(78);
        }
        libc::close(listener.as_raw_fd());
        // before the sentinel: not reported
        libc::close(libc::open(c_early.as_ptr(), libc::O_RDONLY));
        libc::open(c_sentinel.as_ptr(), libc::O_RDONLY);
        let a = libc::open(c_inside.as_ptr(), libc::O_RDONLY);
        let b = libc::open(c_outside.as_ptr(), libc::O_RDONLY);
        // both opens went ahead
        libc::_exit(if a >= 0 && b >= 0 { 0 } else { 1 });
      }
    }
    // SAFETY: the child's end is the child's
    unsafe { libc::close(pair[1]) };
    // SAFETY: just created
    let ours = unsafe { OwnedFd::from_raw_fd(pair[0]) };
    let listener = crate::linux::recv_fd(&ours);
    let tracer = Tracer::new(
      TracePolicy {
        read: Some(vec![dir.join("granted")]),
        ..Default::default()
      },
      pid as u32,
    );
    would_deny::install(would_deny::ReporterConfig::default());
    let stop = AtomicBool::new(false);
    if let Ok(listener) = &listener {
      crate::notify::serve_notifications(listener, &stop, |c| tracer.answer(c));
    }
    let mut status = 0;
    // SAFETY: the child forked above
    unsafe { libc::waitpid(pid, &mut status, 0) };
    let code = libc::WEXITSTATUS(status);
    std::fs::remove_dir_all(&dir).unwrap();
    if code == 77 {
      // no filter can be installed here
      return;
    }
    assert_eq!(code, 0, "the traced opens must go ahead");
    assert!(tracer.started());
    // the reporter is the process's (other tests report too): this run's
    // own lines are told by their paths
    let grants = would_deny::summary().grants;
    let grant =
      |p: &Path| would_deny::Grant::new("--allow-read", p.to_string_lossy());
    assert!(grants.contains(&grant(&outside)), "{grants:?}");
    assert!(!grants.contains(&grant(&inside)), "{grants:?}");
    assert!(!grants.contains(&grant(&early)), "{grants:?}");
  }
}
