// Copyright 2018-2026 the Deno authors. MIT license.

//! A seccomp user-notification supervisor for what Landlock doesn't confine
//! on every kernel: signals, file metadata changes and (before Landlock ABI
//! 3, Linux 6.2) truncation. A second seccomp filter in the confined process
//! (installed after the jail's own program, [`NotifyHandoff`]) hands those
//! syscalls to the installer; the [`Supervisor`] answers each one:
//!
//! - **Signals** (`kill`, `tkill`, `tgkill`, `rt_sigqueueinfo`,
//!   `rt_tgsigqueueinfo`, `pidfd_send_signal`) reach only the confined
//!   process tree: the caller itself, its own process group, or a process in
//!   the confined process's session or descended from it. `kill(-1)` and a
//!   signal to anything else (the installer, the user's other processes)
//!   fail with EPERM; a pid that doesn't exist gets the kernel's own ESRCH.
//!   A `pidfd_send_signal` is sent by the supervisor itself, on its own copy
//!   of the caller's pidfd, so swapping the descriptor meanwhile can't
//!   redirect it.
//! - **Metadata** (`chmod` / `fchmod*`, `chown` / `fchown*` / `lchown`,
//!   `utime*` / `futimesat`, `setxattr` / `removexattr` and their `l` / `f`
//!   forms) and **`truncate`** apply only to objects beneath the write roots:
//!   the supervisor resolves the caller's path (or descriptor) to an
//!   `O_PATH` descriptor of its own, checks where that object really is, and
//!   performs the change on that descriptor, so a path or descriptor swapped
//!   after the check can't redirect it. Every path is resolved within the
//!   caller's own root (its mount namespace, or a chroot of its own): `..`
//!   stops there and an absolute symlink starts there. The caller's private
//!   `/dev/shm`, if it has one, is recognized by its file system. Pipes, sockets and other objects
//!   with no path, and unlinked files, are not files outside the roots. A
//!   path through a procfs magic link (`/dev/fd/N`, `/proc/self/fd/N`,
//!   `/proc/<pid>/cwd/...`) is refused: resolved here it would name the
//!   supervisor's own descriptors; the descriptor forms (`fchmod`, ...) and
//!   `AT_EMPTY_PATH` reach the caller's.
//!
//! Everything fails closed: a request the supervisor can't decide is
//! refused, and once the supervisor is gone the notified syscalls fail with
//! ENOSYS (the kernel's answer when no listener remains).

#![allow(
  clippy::disallowed_methods,
  reason = "the supervisor acts on the real file system and /proc, never a virtual one"
)]

use std::ffi::CString;
use std::ffi::OsStr;
use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use crate::seccomp::AUDIT_ARCH;
use crate::seccomp::BPF_JEQ_K;
use crate::seccomp::BPF_LD_W_ABS;
use crate::seccomp::BPF_RET_K;
use crate::seccomp::SECCOMP_DATA_ARCH;
use crate::seccomp::SECCOMP_DATA_NR;
use crate::seccomp::SECCOMP_RET_ALLOW;
use crate::seccomp::SECCOMP_RET_KILL_PROCESS;
use crate::seccomp::SockFilter;
use crate::seccomp::SockFprog;

pub const SECCOMP_RET_USER_NOTIF: u32 = 0x7fc0_0000;
const SECCOMP_SET_MODE_FILTER: libc::c_ulong = 1;
const SECCOMP_FILTER_FLAG_NEW_LISTENER: libc::c_ulong = 1 << 3;
const SECCOMP_USER_NOTIF_FLAG_CONTINUE: u32 = 1;
const PIDFD_SIGNAL_PROCESS_GROUP: u64 = 1 << 2;

/// Syscall numbers this module answers; `NONE` where the architecture has
/// no such syscall (it never matches).
pub mod nr {
  pub const NONE: u32 = u32::MAX;
  // the same on every architecture (the unified table from Linux 5.1)
  pub const PIDFD_SEND_SIGNAL: u32 = 424;
  pub const PIDFD_OPEN: u32 = 434;
  pub const PIDFD_GETFD: u32 = 438;
  pub const FCHMODAT2: u32 = 452;
  pub const SETXATTRAT: u32 = 463;
  pub const REMOVEXATTRAT: u32 = 466;

  #[cfg(target_arch = "x86_64")]
  mod arch {
    pub const KILL: u32 = 62;
    pub const TKILL: u32 = 200;
    pub const TGKILL: u32 = 234;
    pub const RT_SIGQUEUEINFO: u32 = 129;
    pub const RT_TGSIGQUEUEINFO: u32 = 297;
    pub const CHMOD: u32 = 90;
    pub const FCHMOD: u32 = 91;
    pub const CHOWN: u32 = 92;
    pub const FCHOWN: u32 = 93;
    pub const LCHOWN: u32 = 94;
    pub const FCHMODAT: u32 = 268;
    pub const FCHOWNAT: u32 = 260;
    pub const UTIME: u32 = 132;
    pub const UTIMES: u32 = 235;
    pub const FUTIMESAT: u32 = 261;
    pub const UTIMENSAT: u32 = 280;
    pub const SETXATTR: u32 = 188;
    pub const LSETXATTR: u32 = 189;
    pub const FSETXATTR: u32 = 190;
    pub const REMOVEXATTR: u32 = 197;
    pub const LREMOVEXATTR: u32 = 198;
    pub const FREMOVEXATTR: u32 = 199;
    pub const TRUNCATE: u32 = 76;
  }
  #[cfg(target_arch = "aarch64")]
  mod arch {
    use super::NONE;
    pub const KILL: u32 = 129;
    pub const TKILL: u32 = 130;
    pub const TGKILL: u32 = 131;
    pub const RT_SIGQUEUEINFO: u32 = 138;
    pub const RT_TGSIGQUEUEINFO: u32 = 240;
    pub const CHMOD: u32 = NONE;
    pub const FCHMOD: u32 = 52;
    pub const CHOWN: u32 = NONE;
    pub const FCHOWN: u32 = 55;
    pub const LCHOWN: u32 = NONE;
    pub const FCHMODAT: u32 = 53;
    pub const FCHOWNAT: u32 = 54;
    pub const UTIME: u32 = NONE;
    pub const UTIMES: u32 = NONE;
    pub const FUTIMESAT: u32 = NONE;
    pub const UTIMENSAT: u32 = 88;
    pub const SETXATTR: u32 = 5;
    pub const LSETXATTR: u32 = 6;
    pub const FSETXATTR: u32 = 7;
    pub const REMOVEXATTR: u32 = 14;
    pub const LREMOVEXATTR: u32 = 15;
    pub const FREMOVEXATTR: u32 = 16;
    pub const TRUNCATE: u32 = 45;
  }
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  pub use arch::*;
}

/// The signal syscalls (see the module docs).
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn signal_syscalls() -> Vec<u32> {
  vec![
    nr::KILL,
    nr::TKILL,
    nr::TGKILL,
    nr::RT_SIGQUEUEINFO,
    nr::RT_TGSIGQUEUEINFO,
    nr::PIDFD_SEND_SIGNAL,
  ]
}

/// The metadata syscalls this architecture has (see the module docs).
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn metadata_syscalls() -> Vec<u32> {
  [
    nr::CHMOD,
    nr::FCHMOD,
    nr::FCHMODAT,
    nr::FCHMODAT2,
    nr::CHOWN,
    nr::FCHOWN,
    nr::LCHOWN,
    nr::FCHOWNAT,
    nr::UTIME,
    nr::UTIMES,
    nr::FUTIMESAT,
    nr::UTIMENSAT,
    nr::SETXATTR,
    nr::LSETXATTR,
    nr::FSETXATTR,
    nr::REMOVEXATTR,
    nr::LREMOVEXATTR,
    nr::FREMOVEXATTR,
    nr::SETXATTRAT,
    nr::REMOVEXATTRAT,
  ]
  .into_iter()
  .filter(|n| *n != nr::NONE)
  .collect()
}

/// The program of the notifying filter: another architecture's syscalls
/// kill the process (as in the jail's own program), each of `syscalls` goes
/// to the supervisor, everything else is allowed (the jail's own program
/// decides it).
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn notify_program(syscalls: &[u32]) -> Vec<u8> {
  let ins = |code: u16, jt: u8, jf: u8, k: u32| SockFilter { code, jt, jf, k };
  let mut f = vec![
    ins(BPF_LD_W_ABS, 0, 0, SECCOMP_DATA_ARCH),
    ins(BPF_JEQ_K, 1, 0, AUDIT_ARCH),
    ins(BPF_RET_K, 0, 0, SECCOMP_RET_KILL_PROCESS),
    ins(BPF_LD_W_ABS, 0, 0, SECCOMP_DATA_NR),
  ];
  for nr in syscalls {
    f.push(ins(BPF_JEQ_K, 0, 1, *nr));
    f.push(ins(BPF_RET_K, 0, 0, SECCOMP_RET_USER_NOTIF));
  }
  f.push(ins(BPF_RET_K, 0, 0, SECCOMP_RET_ALLOW));
  let mut bytes = Vec::with_capacity(f.len() * 8);
  for i in f {
    bytes.extend_from_slice(&i.code.to_ne_bytes());
    bytes.push(i.jt);
    bytes.push(i.jf);
    bytes.extend_from_slice(&i.k.to_ne_bytes());
  }
  bytes
}

/// Installs the notifying filter in a command's process (between `fork`
/// and `exec`, after the jail's own confinement, which sets
/// `no_new_privs`) and hands its listener to the installer over a
/// socketpair.
pub struct NotifyHandoff {
  parent: OwnedFd,
  child: OwnedFd,
}

impl NotifyHandoff {
  pub fn new() -> io::Result<Self> {
    let mut fds = [0; 2];
    // SAFETY: a socketpair into a two-element array
    let res = unsafe {
      libc::socketpair(
        libc::AF_UNIX,
        libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
        0,
        fds.as_mut_ptr(),
      )
    };
    if res != 0 {
      return Err(io::Error::last_os_error());
    }
    // SAFETY: just created, owned here
    let (parent, child) =
      unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    Ok(Self { parent, child })
  }

  /// Adds the filter to the command's `pre_exec`; it must come after the
  /// jail's own `pre_exec` (`linux::apply_to_command`).
  pub fn apply_to_command(
    &self,
    command: &mut tokio::process::Command,
    program: Vec<u8>,
  ) {
    let child_fd = self.child.as_raw_fd();
    // SAFETY: only async-signal-safe syscalls run between fork and exec, on
    // memory allocated before the fork
    unsafe {
      command.pre_exec(move || {
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
          return Err(io::Error::last_os_error());
        }
        let sent = crate::linux::send_fd(child_fd, listener as libc::c_int);
        libc::close(listener as libc::c_int);
        libc::close(child_fd);
        sent
      });
    }
  }

  /// Receives the listener (blocking; within a few seconds of the spawn).
  pub fn receive(self) -> io::Result<OwnedFd> {
    let Self { parent, child } = self;
    drop(child);
    crate::linux::recv_fd(&parent)
  }
}

/// Answers the notifications of `listener` with `answer` until every
/// process under the filter is gone or `stop` is set (checked every 100 ms).
pub fn serve_notifications(
  listener: &OwnedFd,
  stop: &AtomicBool,
  mut answer: impl FnMut(&Call) -> Answer,
) {
  let fd = listener.as_raw_fd();
  loop {
    if stop.load(Ordering::Relaxed) {
      return;
    }
    let mut poll = libc::pollfd {
      fd,
      events: libc::POLLIN,
      revents: 0,
    };
    // SAFETY: one pollfd
    let ready = unsafe { libc::poll(&mut poll, 1, 100) };
    if ready < 0 {
      if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
        continue;
      }
      return;
    }
    if ready == 0 {
      continue;
    }
    if poll.revents & libc::POLLIN == 0 {
      // POLLHUP: no process uses the filter any more
      return;
    }
    // SAFETY: the kernel wants a zeroed request
    let mut req: libc::seccomp_notif = unsafe { std::mem::zeroed() };
    // SAFETY: a valid listener and request
    if unsafe { libc::ioctl(fd, libc::SECCOMP_IOCTL_NOTIF_RECV as _, &mut req) }
      != 0
    {
      match io::Error::last_os_error().raw_os_error() {
        // the caller died, or a signal interrupted the wait
        Some(libc::ENOENT) | Some(libc::EINTR) => continue,
        _ => return,
      }
    }
    let call = Call {
      tid: req.pid,
      nr: req.data.nr as u32,
      args: req.data.args,
    };
    let answer = answer(&call);
    // what the answer read of the caller (its memory, cwd, descriptors)
    // was the caller's only if it is still waiting
    // SAFETY: a valid listener and id
    let valid = unsafe {
      libc::ioctl(fd, libc::SECCOMP_IOCTL_NOTIF_ID_VALID as _, &req.id)
    } == 0;
    if !valid {
      continue;
    }
    let mut resp = libc::seccomp_notif_resp {
      id: req.id,
      val: 0,
      error: 0,
      flags: 0,
    };
    match answer {
      Answer::Continue => resp.flags = SECCOMP_USER_NOTIF_FLAG_CONTINUE,
      Answer::Done(Ok(val)) => resp.val = val,
      Answer::Done(Err(errno)) => resp.error = -errno,
    }
    // SAFETY: a valid listener and response; a stale id is refused
    unsafe {
      libc::ioctl(fd, libc::SECCOMP_IOCTL_NOTIF_SEND as _, &resp);
    }
  }
}

/// The answer to one notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
  /// Let the kernel run the syscall as the caller made it.
  Continue,
  /// The syscall's result (done by the supervisor, or refused): a value,
  /// or an errno.
  Done(Result<i64, i32>),
}

/// One notification's syscall: its number and arguments (as the kernel
/// passes them) and the calling thread.
#[derive(Debug, Clone, Copy)]
pub struct Call {
  pub tid: u32,
  pub nr: u32,
  pub args: [u64; 6],
}

/// Answers the notified syscalls of one confined process tree.
pub struct Supervisor {
  /// Canonical directories metadata changes and truncation may touch.
  write_roots: Vec<PathBuf>,
  /// The confined process: a session leader (the jail detaches it), so its
  /// pid is also its session's id.
  root_pid: u32,
  /// The confined process has a private `/dev/shm`
  /// (`linux::PrivateShm`), whose files it may change too.
  private_shm: bool,
}

impl Supervisor {
  pub fn new(write_roots: Vec<PathBuf>, root_pid: u32) -> Self {
    Self {
      write_roots,
      root_pid,
      private_shm: false,
    }
  }

  /// Also allows the files of the confined process's private `/dev/shm`,
  /// recognized by its file system (the host's `/dev/shm`, which the same
  /// path names here, is not).
  pub fn with_private_shm(mut self, private_shm: bool) -> Self {
    self.private_shm = private_shm;
    self
  }

  /// The device of the confined process's own `/dev/shm`.
  fn private_shm_dev(&self) -> Option<u64> {
    if !self.private_shm {
      return None;
    }
    let path =
      CString::new(format!("/proc/{}/root/dev/shm", self.root_pid)).ok()?;
    // SAFETY: a zeroed stat the kernel fills, a NUL-terminated path
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: as above
    let res = unsafe { libc::stat(path.as_ptr(), &mut st) };
    (res == 0).then_some(st.st_dev as u64)
  }

  /// Answers notifications on `listener` until every process under the
  /// filter is gone or `stop` is set (checked every 100 ms).
  pub fn serve(&self, listener: OwnedFd, stop: &AtomicBool) {
    serve_notifications(&listener, stop, |call| self.answer(call));
  }

  /// The answer to one syscall.
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  pub fn answer(&self, c: &Call) -> Answer {
    let a = c.args;
    let int = |i: usize| a[i] as u32 as i32;
    let done = Answer::Done;
    match c.nr {
      n if n == nr::KILL => self.signal_kill(c.tid, int(0)),
      n if n == nr::TKILL => match tgid_of(int(0) as u32) {
        Some(tgid) => self.signal_process(c.tid, tgid),
        None => Answer::Continue,
      },
      n if n == nr::TGKILL
        || n == nr::RT_SIGQUEUEINFO
        || n == nr::RT_TGSIGQUEUEINFO =>
      {
        self.signal_process(c.tid, int(0) as u32)
      }
      n if n == nr::PIDFD_SEND_SIGNAL => {
        done(self.pidfd_send_signal(c.tid, int(0), int(1), a[2], a[3]))
      }
      n if n == nr::CHMOD => {
        done(self.chmod(c, Obj::path(AT_FDCWD, a[0], true), a[1]))
      }
      n if n == nr::FCHMOD => done(self.chmod(c, Obj::Fd(int(0)), a[1])),
      n if n == nr::FCHMODAT => {
        done(self.chmod(c, Obj::path(int(0), a[1], true), a[2]))
      }
      n if n == nr::FCHMODAT2 => {
        done(self.chmod(c, Obj::at(int(0), a[1], int(3)), a[2]))
      }
      n if n == nr::CHOWN => {
        done(self.chown(c, Obj::path(AT_FDCWD, a[0], true), a[1], a[2]))
      }
      n if n == nr::LCHOWN => {
        done(self.chown(c, Obj::path(AT_FDCWD, a[0], false), a[1], a[2]))
      }
      n if n == nr::FCHOWN => done(self.chown(c, Obj::Fd(int(0)), a[1], a[2])),
      n if n == nr::FCHOWNAT => {
        done(self.chown(c, Obj::at(int(0), a[1], int(4)), a[2], a[3]))
      }
      n if n == nr::UTIME => done(self.utimes(
        c,
        Obj::path(AT_FDCWD, a[0], true),
        Times::Utimbuf(a[1]),
      )),
      n if n == nr::UTIMES => done(self.utimes(
        c,
        Obj::path(AT_FDCWD, a[0], true),
        Times::Timeval(a[1]),
      )),
      n if n == nr::FUTIMESAT => {
        let obj = if a[1] == 0 {
          Obj::Fd(int(0))
        } else {
          Obj::path(int(0), a[1], true)
        };
        done(self.utimes(c, obj, Times::Timeval(a[2])))
      }
      n if n == nr::UTIMENSAT => {
        // a null path: the descriptor itself
        let obj = if a[1] == 0 {
          Obj::Fd(int(0))
        } else {
          Obj::at(int(0), a[1], int(3))
        };
        done(self.utimes(c, obj, Times::Timespec(a[2])))
      }
      n if n == nr::SETXATTR => done(self.setxattr(
        c,
        Obj::path(AT_FDCWD, a[0], true),
        a[1],
        a[2],
        a[3],
        a[4],
      )),
      n if n == nr::LSETXATTR => done(self.setxattr(
        c,
        Obj::path(AT_FDCWD, a[0], false),
        a[1],
        a[2],
        a[3],
        a[4],
      )),
      n if n == nr::FSETXATTR => {
        done(self.setxattr(c, Obj::Fd(int(0)), a[1], a[2], a[3], a[4]))
      }
      n if n == nr::REMOVEXATTR => {
        done(self.removexattr(c, Obj::path(AT_FDCWD, a[0], true), a[1]))
      }
      n if n == nr::LREMOVEXATTR => {
        done(self.removexattr(c, Obj::path(AT_FDCWD, a[0], false), a[1]))
      }
      n if n == nr::FREMOVEXATTR => {
        done(self.removexattr(c, Obj::Fd(int(0)), a[1]))
      }
      // libc doesn't call these yet; a caller falls back to the forms above
      n if n == nr::SETXATTRAT || n == nr::REMOVEXATTRAT => {
        done(Err(libc::ENOSYS))
      }
      n if n == nr::TRUNCATE => {
        done(self.truncate(c, Obj::path(AT_FDCWD, a[0], true), a[1] as i64))
      }
      // a syscall this supervisor doesn't know: refused
      _ => done(Err(libc::ENOSYS)),
    }
  }

  // ---- signals ----

  /// Whether `pid` is in the confined tree: the confined process, a process
  /// in its session, or one descended from it.
  fn inside(&self, pid: u32) -> bool {
    if pid == self.root_pid {
      return true;
    }
    let Some(stat) = proc_stat(pid) else {
      return false;
    };
    if stat.session == self.root_pid {
      return true;
    }
    let mut ppid = stat.ppid;
    for _ in 0..256 {
      if ppid == self.root_pid {
        return true;
      }
      if ppid <= 1 {
        return false;
      }
      match proc_stat(ppid) {
        Some(next) => ppid = next.ppid,
        None => return false,
      }
    }
    false
  }

  /// Whether the process group `pgid` has a member in the confined tree
  /// (`None`: no such group).
  fn group_inside(&self, pgid: u32) -> Option<bool> {
    let members = group_members(pgid);
    if members.is_empty() {
      return None;
    }
    Some(members.iter().any(|pid| self.inside(*pid)))
  }

  fn signal_kill(&self, caller: u32, pid: i32) -> Answer {
    match pid {
      0 => Answer::Continue,
      -1 => Answer::Done(Err(libc::EPERM)),
      p if p > 0 => self.signal_process(caller, p as u32),
      p => match self.group_inside(p.unsigned_abs()) {
        None | Some(true) => Answer::Continue,
        Some(false) => Answer::Done(Err(libc::EPERM)),
      },
    }
  }

  /// A signal to the process (thread group) `tgid`.
  fn signal_process(&self, caller: u32, tgid: u32) -> Answer {
    if tgid_of(caller) == Some(tgid) {
      return Answer::Continue;
    }
    if proc_stat(tgid).is_none() {
      // gone (or never there): the kernel's ESRCH
      return Answer::Continue;
    }
    if self.inside(tgid) {
      Answer::Continue
    } else {
      Answer::Done(Err(libc::EPERM))
    }
  }

  fn pidfd_send_signal(
    &self,
    caller: u32,
    pidfd: i32,
    sig: i32,
    info: u64,
    flags: u64,
  ) -> Result<i64, i32> {
    let mine = steal_fd(caller, pidfd)?;
    let target = pidfd_target(&mine).ok_or(libc::EBADF)?;
    let allowed = match target {
      // exited: the kernel says so
      None => true,
      Some(pid) if flags & PIDFD_SIGNAL_PROCESS_GROUP != 0 => {
        match proc_stat(pid) {
          Some(stat) => self.group_inside(stat.pgrp).unwrap_or(true),
          None => true,
        }
      }
      Some(pid) => {
        let tgid = tgid_of(pid).unwrap_or(pid);
        tgid_of(caller) == Some(tgid)
          || proc_stat(tgid).is_none()
          || self.inside(tgid)
      }
    };
    if !allowed {
      return Err(libc::EPERM);
    }
    let mut siginfo = [0u8; 128];
    let info_ptr = if info == 0 {
      std::ptr::null()
    } else {
      read_exact(caller, info, &mut siginfo)?;
      siginfo.as_ptr()
    };
    // SAFETY: our own pidfd, a siginfo buffer or null
    let res = unsafe {
      libc::syscall(
        nr::PIDFD_SEND_SIGNAL as libc::c_long,
        mine.as_raw_fd(),
        sig,
        info_ptr,
        flags as libc::c_uint,
      )
    };
    if res < 0 { Err(errno()) } else { Ok(0) }
  }

  // ---- metadata and truncation ----

  /// Resolves `obj` as the caller sees it to an `O_PATH` descriptor here and
  /// checks the object lies beneath the write roots.
  fn object(&self, c: &Call, obj: Obj) -> Result<Resolved, i32> {
    let fd = open_object(c.tid, obj)?;
    self.check_within_roots(&fd)?;
    let symlink = is_symlink(&fd);
    Ok(Resolved { fd, symlink })
  }

  fn check_within_roots(&self, fd: &OwnedFd) -> Result<(), i32> {
    let link = std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd()))
      .map_err(|_| libc::EPERM)?;
    let bytes = link.as_os_str().as_bytes();
    if !bytes.starts_with(b"/") {
      // a pipe, socket or other object with no path
      return Ok(());
    }
    if bytes.ends_with(b" (deleted)") {
      // unlinked: harmless when no name is left anywhere
      return if link_count(fd) == Some(0) {
        Ok(())
      } else {
        Err(libc::EPERM)
      };
    }
    if within(Path::new(OsStr::from_bytes(bytes)), &self.write_roots) {
      return Ok(());
    }
    // the private /dev/shm, by its file system, whatever path it shows here
    if let Some(dev) = self.private_shm_dev()
      && fstat(fd).is_some_and(|st| st.st_dev == dev)
    {
      return Ok(());
    }
    Err(libc::EPERM)
  }

  fn chmod(&self, c: &Call, obj: Obj, mode: u64) -> Result<i64, i32> {
    let o = self.object(c, obj)?;
    if o.symlink {
      // Linux has no mode on a symlink
      return Err(libc::EOPNOTSUPP);
    }
    // SAFETY: a NUL-terminated path
    cvt(unsafe { libc::chmod(o.proc_path().as_ptr(), mode as libc::mode_t) })
  }

  fn chown(&self, c: &Call, obj: Obj, uid: u64, gid: u64) -> Result<i64, i32> {
    let o = self.object(c, obj)?;
    // SAFETY: an O_PATH descriptor with AT_EMPTY_PATH
    cvt(unsafe {
      libc::fchownat(
        o.fd.as_raw_fd(),
        c"".as_ptr(),
        uid as u32,
        gid as u32,
        libc::AT_EMPTY_PATH,
      )
    })
  }

  fn utimes(&self, c: &Call, obj: Obj, times: Times) -> Result<i64, i32> {
    let ts = times.read(c.tid)?;
    let o = self.object(c, obj)?;
    let ptr = ts.as_ref().map_or(std::ptr::null(), |t| t.as_ptr());
    // SAFETY: an O_PATH descriptor with AT_EMPTY_PATH, two timespecs or null
    let res = unsafe {
      libc::utimensat(o.fd.as_raw_fd(), c"".as_ptr(), ptr, libc::AT_EMPTY_PATH)
    };
    if res == 0 {
      return Ok(0);
    }
    let err = errno();
    if err == libc::EINVAL && !o.symlink {
      // a kernel without AT_EMPTY_PATH here: through the descriptor's path
      // SAFETY: a NUL-terminated path, two timespecs or null
      return cvt(unsafe {
        libc::utimensat(libc::AT_FDCWD, o.proc_path().as_ptr(), ptr, 0)
      });
    }
    Err(err)
  }

  fn setxattr(
    &self,
    c: &Call,
    obj: Obj,
    name: u64,
    value: u64,
    size: u64,
    flags: u64,
  ) -> Result<i64, i32> {
    let name = read_cstr(c.tid, name, 256)?;
    if size > 65536 {
      return Err(libc::E2BIG);
    }
    let mut buf = vec![0u8; size as usize];
    if size > 0 {
      read_exact(c.tid, value, &mut buf)?;
    }
    let o = self.object(c, obj)?;
    if o.symlink {
      return Err(libc::EPERM);
    }
    // SAFETY: NUL-terminated strings, a buffer of `size` bytes
    cvt(unsafe {
      libc::setxattr(
        o.proc_path().as_ptr(),
        name.as_ptr(),
        buf.as_ptr() as *const libc::c_void,
        buf.len(),
        flags as libc::c_int,
      )
    })
  }

  fn removexattr(&self, c: &Call, obj: Obj, name: u64) -> Result<i64, i32> {
    let name = read_cstr(c.tid, name, 256)?;
    let o = self.object(c, obj)?;
    if o.symlink {
      return Err(libc::EPERM);
    }
    // SAFETY: NUL-terminated strings
    cvt(unsafe { libc::removexattr(o.proc_path().as_ptr(), name.as_ptr()) })
  }

  fn truncate(&self, c: &Call, obj: Obj, len: i64) -> Result<i64, i32> {
    let o = self.object(c, obj)?;
    // SAFETY: a NUL-terminated path
    cvt(unsafe { libc::truncate(o.proc_path().as_ptr(), len) })
  }
}

pub(crate) const AT_FDCWD: i32 = libc::AT_FDCWD;

/// What a metadata syscall names: a path relative to a directory
/// descriptor (or the working directory), or a descriptor.
#[derive(Debug, Clone, Copy)]
enum Obj {
  Path {
    dirfd: i32,
    path: u64,
    follow: bool,
    empty_path: bool,
  },
  Fd(i32),
}

impl Obj {
  fn path(dirfd: i32, path: u64, follow: bool) -> Self {
    Obj::Path {
      dirfd,
      path,
      follow,
      empty_path: false,
    }
  }

  /// A `*at` call's path and `AT_*` flags.
  fn at(dirfd: i32, path: u64, flags: i32) -> Self {
    Obj::Path {
      dirfd,
      path,
      follow: flags & libc::AT_SYMLINK_NOFOLLOW == 0,
      empty_path: flags & libc::AT_EMPTY_PATH != 0,
    }
  }
}

struct Resolved {
  fd: OwnedFd,
  symlink: bool,
}

impl Resolved {
  /// The path through this process's descriptor table to the object.
  fn proc_path(&self) -> CString {
    CString::new(format!("/proc/self/fd/{}", self.fd.as_raw_fd())).unwrap()
  }
}

/// A timestamps argument as each syscall passes it.
#[derive(Debug, Clone, Copy)]
enum Times {
  Timespec(u64),
  Timeval(u64),
  Utimbuf(u64),
}

impl Times {
  /// The two timespecs, or `None` for "now".
  fn read(self, tid: u32) -> Result<Option<[libc::timespec; 2]>, i32> {
    let ts = |sec: i64, nsec: i64| libc::timespec {
      tv_sec: sec as libc::time_t,
      tv_nsec: nsec as _,
    };
    let words = |addr: u64, n: usize| -> Result<Vec<i64>, i32> {
      let mut buf = vec![0u8; n * 8];
      read_exact(tid, addr, &mut buf)?;
      Ok(
        buf
          .chunks(8)
          .map(|c| i64::from_ne_bytes(c.try_into().unwrap()))
          .collect(),
      )
    };
    match self {
      Times::Timespec(0) | Times::Timeval(0) | Times::Utimbuf(0) => Ok(None),
      Times::Timespec(addr) => {
        let w = words(addr, 4)?;
        Ok(Some([ts(w[0], w[1]), ts(w[2], w[3])]))
      }
      Times::Timeval(addr) => {
        let w = words(addr, 4)?;
        if !(0..1_000_000).contains(&w[1]) || !(0..1_000_000).contains(&w[3]) {
          return Err(libc::EINVAL);
        }
        Ok(Some([ts(w[0], w[1] * 1000), ts(w[2], w[3] * 1000)]))
      }
      Times::Utimbuf(addr) => {
        let w = words(addr, 2)?;
        Ok(Some([ts(w[0], 0), ts(w[1], 0)]))
      }
    }
  }
}

pub(crate) fn errno() -> i32 {
  io::Error::last_os_error()
    .raw_os_error()
    .unwrap_or(libc::EIO)
}

fn cvt(res: libc::c_int) -> Result<i64, i32> {
  if res < 0 {
    Err(errno())
  } else {
    Ok(i64::from(res))
  }
}

fn within(path: &Path, roots: &[PathBuf]) -> bool {
  roots.iter().any(|root| path.starts_with(root))
}

pub(crate) fn open_path(
  path: &str,
  flags: libc::c_int,
) -> Result<OwnedFd, i32> {
  let c = CString::new(path).map_err(|_| libc::EINVAL)?;
  // SAFETY: a NUL-terminated path
  let fd = unsafe { libc::open(c.as_ptr(), flags | libc::O_CLOEXEC) };
  if fd < 0 {
    return Err(errno());
  }
  // SAFETY: just opened
  Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The caller's object as an `O_PATH` descriptor of this process.
fn open_object(tid: u32, obj: Obj) -> Result<OwnedFd, i32> {
  match obj {
    // the descriptor's object (a later dup2 over it can only swap in
    // another object, which is checked the same way)
    Obj::Fd(fd) => {
      if fd < 0 {
        return Err(libc::EBADF);
      }
      open_path(&format!("/proc/{tid}/fd/{fd}"), libc::O_PATH)
        .map_err(|e| if e == libc::ENOENT { libc::EBADF } else { e })
    }
    Obj::Path {
      dirfd,
      path,
      follow,
      empty_path,
    } => {
      let path = read_cstr(tid, path, libc::PATH_MAX as usize)?;
      resolve(tid, dirfd, path, follow, empty_path)
    }
  }
}

/// The caller's `path` (relative to `dirfd`, or its working directory)
/// resolved as the caller resolves it, confined to the caller's own root
/// (its mount namespace, or a chroot of its own): `openat2` with
/// `RESOLVE_IN_ROOT` beneath `/proc/<tid>/root`, so `..` stops at that root
/// and an absolute symlink starts from it, never from this process's. A
/// relative path is joined to the base directory's path beneath that root
/// (a rename meanwhile can only move the lookup elsewhere beneath the same
/// root, and the object found is checked like any other); a base outside
/// the caller's root is refused.
///
/// No magic link either: this process resolves the path, so `/proc/self`
/// (`/dev/fd`, `/dev/stdin`, `/proc/thread-self`) would name the
/// supervisor, and `/proc/self/fd/N` its own descriptor N, not the
/// caller's.
pub(crate) fn resolve(
  tid: u32,
  dirfd: i32,
  path: CString,
  follow: bool,
  empty_path: bool,
) -> Result<OwnedFd, i32> {
  if path.as_bytes().is_empty() {
    if !empty_path {
      return Err(libc::ENOENT);
    }
    return if dirfd == AT_FDCWD {
      open_path(&format!("/proc/{tid}/cwd"), libc::O_PATH)
    } else {
      open_object(tid, Obj::Fd(dirfd))
    };
  }
  let root = open_path(
    &format!("/proc/{tid}/root"),
    libc::O_PATH | libc::O_DIRECTORY,
  )?;
  let path = if path.as_bytes().starts_with(b"/") {
    path
  } else {
    let base = if dirfd == AT_FDCWD {
      open_path(
        &format!("/proc/{tid}/cwd"),
        libc::O_PATH | libc::O_DIRECTORY,
      )?
    } else if dirfd < 0 {
      return Err(libc::EBADF);
    } else {
      open_path(
        &format!("/proc/{tid}/fd/{dirfd}"),
        libc::O_PATH | libc::O_DIRECTORY,
      )
      .map_err(|e| if e == libc::ENOENT { libc::EBADF } else { e })?
    };
    let mut joined = beneath(&root, &base)?;
    if !joined.is_empty() {
      joined.push(b'/');
    }
    joined.extend_from_slice(path.as_bytes());
    CString::new(joined).map_err(|_| libc::EINVAL)?
  };
  // `/proc/self` names the process that looks it up, and the lookup below
  // is this process's: name the caller's entry instead
  let path = match caller_proc_view(tid, path.as_bytes()) {
    Some(view) => CString::new(view).map_err(|_| libc::EINVAL)?,
    None => path,
  };
  let nofollow = if follow { 0 } else { libc::O_NOFOLLOW };
  let flags = libc::O_PATH | libc::O_CLOEXEC | nofollow;
  match openat2(
    &root,
    &path,
    flags,
    libc::RESOLVE_IN_ROOT | libc::RESOLVE_NO_MAGICLINKS,
  ) {
    Ok(fd) => Ok(fd),
    // ELOOP: a real symlink loop (the caller's own ELOOP), or a magic link
    // on the way, refused (a scoped lookup refuses those with EXDEV)
    Err(libc::ELOOP) => {
      match openat2(&root, &path, flags, libc::RESOLVE_IN_ROOT) {
        Err(libc::ELOOP) => Err(libc::ELOOP),
        Err(libc::EXDEV) | Ok(_) => Err(libc::EPERM),
        Err(e) => Err(e),
      }
    }
    Err(e) => Err(e),
  }
}

/// `path` with a leading `/proc/self` or `/proc/thread-self` (the process
/// or thread that looks it up) replaced by the entry of the calling thread
/// `tid`: `/proc/<its process>` and `/proc/<its process>/task/<tid>`. A
/// supervisor resolving a caller's path would otherwise reach its own entry
/// (the tracing observer reported the run's own `/proc/self` reads as the
/// supervisor's). `None`: no such prefix, or the caller is gone.
fn caller_proc_view(tid: u32, path: &[u8]) -> Option<Vec<u8>> {
  let lead = path.iter().take_while(|b| **b == b'/').count();
  let rest = &path[lead..];
  let strip = |prefix: &[u8]| -> Option<usize> {
    (rest.starts_with(prefix)
      && (rest.len() == prefix.len() || rest[prefix.len()] == b'/'))
      .then_some(prefix.len())
  };
  let (len, thread) = match strip(b"proc/self") {
    Some(len) => (len, false),
    None => (strip(b"proc/thread-self")?, true),
  };
  let tgid = tgid_of(tid)?;
  let mut out = path[..lead].to_vec();
  out.extend_from_slice(format!("proc/{tgid}").as_bytes());
  if thread {
    out.extend_from_slice(format!("/task/{tid}").as_bytes());
  }
  out.extend_from_slice(&rest[len..]);
  Some(out)
}

/// The path of the directory `base` beneath the directory `root` (both
/// this process's descriptors), without a leading `/`: empty for the root
/// itself. A base outside the root (a chroot the caller left its working
/// directory outside of) is refused; one that was removed has no entries.
fn beneath(root: &OwnedFd, base: &OwnedFd) -> Result<Vec<u8>, i32> {
  let name = |fd: &OwnedFd| {
    std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd()))
      .map(|p| p.into_os_string().into_vec())
      .map_err(|_| libc::EPERM)
  };
  let (root, base) = (name(root)?, name(base)?);
  if root.ends_with(b" (deleted)") || !root.starts_with(b"/") {
    return Err(libc::EPERM);
  }
  if base.ends_with(b" (deleted)") {
    return Err(libc::ENOENT);
  }
  let rest = if root == b"/" {
    base.strip_prefix(b"/")
  } else {
    base.strip_prefix(root.as_slice()).and_then(|r| {
      if r.is_empty() {
        Some(r)
      } else {
        r.strip_prefix(b"/")
      }
    })
  };
  rest.map(<[u8]>::to_vec).ok_or(libc::EPERM)
}

/// `path` opened beneath `base` with `openat2`'s `resolve` restrictions; a
/// kernel without `openat2` (older than any with Landlock) fails closed.
fn openat2(
  base: &OwnedFd,
  path: &CString,
  flags: libc::c_int,
  resolve: u64,
) -> Result<OwnedFd, i32> {
  // SAFETY: a plain struct; zeroed is "no mode, no restriction"
  let mut how: libc::open_how = unsafe { std::mem::zeroed() };
  how.flags = flags as u64;
  how.resolve = resolve;
  // SAFETY: a valid directory descriptor, NUL-terminated path and an
  // open_how of the size given
  let fd = unsafe {
    libc::syscall(
      libc::SYS_openat2,
      base.as_raw_fd(),
      path.as_ptr(),
      &how as *const libc::open_how,
      std::mem::size_of::<libc::open_how>(),
    )
  };
  if fd < 0 {
    return Err(match errno() {
      libc::ENOSYS => libc::EPERM,
      e => e,
    });
  }
  // SAFETY: just opened
  Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

pub(crate) fn fstat(fd: &OwnedFd) -> Option<libc::stat> {
  // SAFETY: a zeroed stat the kernel fills
  let mut st: libc::stat = unsafe { std::mem::zeroed() };
  // SAFETY: a valid descriptor (O_PATH included) with AT_EMPTY_PATH
  let res = unsafe {
    libc::fstatat(
      fd.as_raw_fd(),
      c"".as_ptr(),
      &mut st,
      libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
    )
  };
  (res == 0).then_some(st)
}

fn is_symlink(fd: &OwnedFd) -> bool {
  fstat(fd).is_some_and(|st| st.st_mode & libc::S_IFMT == libc::S_IFLNK)
}

#[allow(
  clippy::unnecessary_cast,
  reason = "st_nlink is u64 on x86_64 and u32 on aarch64"
)]
fn link_count(fd: &OwnedFd) -> Option<u64> {
  fstat(fd).map(|st| st.st_nlink as u64)
}

/// Reads exactly `buf.len()` bytes of the caller's memory at `addr`.
pub(crate) fn read_exact(
  tid: u32,
  addr: u64,
  buf: &mut [u8],
) -> Result<(), i32> {
  if addr == 0 {
    return Err(libc::EFAULT);
  }
  let local = libc::iovec {
    iov_base: buf.as_mut_ptr() as *mut libc::c_void,
    iov_len: buf.len(),
  };
  let remote = libc::iovec {
    iov_base: addr as *mut libc::c_void,
    iov_len: buf.len(),
  };
  // SAFETY: one local buffer of the given length; the remote side is read
  // by the kernel, which checks it
  let n = unsafe {
    libc::process_vm_readv(tid as libc::pid_t, &local, 1, &remote, 1, 0)
  };
  if n < 0 {
    // the caller's memory is out of reach (gone, or a ptrace policy): refuse
    return Err(match errno() {
      libc::EFAULT => libc::EFAULT,
      _ => libc::EPERM,
    });
  }
  if n as usize != buf.len() {
    return Err(libc::EFAULT);
  }
  Ok(())
}

/// Reads a NUL-terminated string of at most `max` bytes from the caller.
pub(crate) fn read_cstr(
  tid: u32,
  addr: u64,
  max: usize,
) -> Result<CString, i32> {
  if addr == 0 {
    return Err(libc::EFAULT);
  }
  let mut out = Vec::new();
  let mut at = addr;
  while out.len() < max {
    // up to the end of the page, so a string near unmapped memory reads
    let room = 4096 - (at % 4096) as usize;
    let n = room.min(max - out.len());
    let mut chunk = vec![0u8; n];
    read_exact(tid, at, &mut chunk)?;
    if let Some(end) = chunk.iter().position(|b| *b == 0) {
      out.extend_from_slice(&chunk[..end]);
      return Ok(CString::new(out).unwrap());
    }
    out.extend_from_slice(&chunk);
    at += n as u64;
  }
  Err(libc::ENAMETOOLONG)
}

/// A copy of the caller's descriptor `fd` in this process.
pub(crate) fn steal_fd(tid: u32, fd: i32) -> Result<OwnedFd, i32> {
  let tgid = tgid_of(tid).ok_or(libc::ESRCH)?;
  // SAFETY: plain syscalls; the descriptors are owned once valid
  unsafe {
    let pidfd = libc::syscall(nr::PIDFD_OPEN as libc::c_long, tgid, 0);
    if pidfd < 0 {
      return Err(libc::EPERM);
    }
    let pidfd = OwnedFd::from_raw_fd(pidfd as i32);
    let mine =
      libc::syscall(nr::PIDFD_GETFD as libc::c_long, pidfd.as_raw_fd(), fd, 0);
    if mine < 0 {
      return Err(match errno() {
        libc::EBADF => libc::EBADF,
        _ => libc::EPERM,
      });
    }
    Ok(OwnedFd::from_raw_fd(mine as i32))
  }
}

/// The process a pidfd (or a `/proc/<pid>` directory descriptor, which
/// `pidfd_send_signal` also takes) refers to: `Some(None)` when it has
/// exited, `None` when the descriptor is neither.
fn pidfd_target(fd: &OwnedFd) -> Option<Option<u32>> {
  let raw = fd.as_raw_fd();
  if let Ok(info) = std::fs::read_to_string(format!("/proc/self/fdinfo/{raw}"))
    && let Some(line) = info.lines().find(|l| l.starts_with("Pid:"))
  {
    let pid = line["Pid:".len()..].trim().parse::<i64>().ok()?;
    return Some((pid > 0).then_some(pid as u32));
  }
  let link = std::fs::read_link(format!("/proc/self/fd/{raw}")).ok()?;
  let rest = link.to_str()?.strip_prefix("/proc/")?;
  rest.parse::<u32>().ok().map(Some)
}

struct ProcStat {
  ppid: u32,
  pgrp: u32,
  session: u32,
}

fn proc_stat(pid: u32) -> Option<ProcStat> {
  let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
  // the command name is in parentheses and may hold anything
  let mut fields = stat[stat.rfind(')')? + 1..].split_whitespace();
  let _state = fields.next()?;
  Some(ProcStat {
    ppid: fields.next()?.parse().ok()?,
    pgrp: fields.next()?.parse().ok()?,
    session: fields.next()?.parse().ok()?,
  })
}

/// The thread group (process) of a thread.
pub(crate) fn tgid_of(tid: u32) -> Option<u32> {
  let status = std::fs::read_to_string(format!("/proc/{tid}/status")).ok()?;
  status
    .lines()
    .find_map(|l| l.strip_prefix("Tgid:"))?
    .trim()
    .parse()
    .ok()
}

/// The processes of a process group.
fn group_members(pgid: u32) -> Vec<u32> {
  let Ok(entries) = std::fs::read_dir("/proc") else {
    return Vec::new();
  };
  entries
    .flatten()
    .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
    .filter(|pid| proc_stat(*pid).is_some_and(|s| s.pgrp == pgid))
    .collect()
}

#[cfg(all(test, any(target_arch = "x86_64", target_arch = "aarch64")))]
mod tests {
  use super::*;
  use crate::seccomp::run_filter;

  #[test]
  fn proc_self_names_the_callers_entry() {
    let pid = std::process::id();
    // this thread (the test's own) as the caller
    // SAFETY: gettid has no preconditions
    let tid = unsafe { libc::syscall(libc::SYS_gettid) } as u32;
    let view = |p: &str| {
      caller_proc_view(tid, p.as_bytes()).map(|v| String::from_utf8(v).unwrap())
    };
    assert_eq!(view("/proc/self"), Some(format!("/proc/{pid}")));
    assert_eq!(
      view("/proc/self/cgroup"),
      Some(format!("/proc/{pid}/cgroup"))
    );
    assert_eq!(
      view("//proc/thread-self/comm"),
      Some(format!("//proc/{pid}/task/{tid}/comm"))
    );
    assert_eq!(view("proc/self/maps"), Some(format!("proc/{pid}/maps")));
    assert_eq!(view("/proc/selfish"), None);
    assert_eq!(view("/proc/1/cgroup"), None);
    assert_eq!(view("/tmp/proc/self"), None);
  }

  #[test]
  fn notify_program_routes_only_the_listed_syscalls() {
    let p = notify_program(&[nr::KILL, nr::FCHMODAT]);
    assert_eq!(run_filter(&p, nr::KILL, [0; 6]), SECCOMP_RET_USER_NOTIF);
    assert_eq!(run_filter(&p, nr::FCHMODAT, [0; 6]), SECCOMP_RET_USER_NOTIF);
    assert_eq!(run_filter(&p, nr::FCHOWN, [0; 6]), SECCOMP_RET_ALLOW);
    assert_eq!(run_filter(&p, 0, [0; 6]), SECCOMP_RET_ALLOW);
    // the arch check comes first
    let ins = |i: usize| {
      u32::from_ne_bytes(p[i * 8 + 4..i * 8 + 8].try_into().unwrap())
    };
    assert_eq!(ins(1), AUDIT_ARCH);
    assert_eq!(ins(2), SECCOMP_RET_KILL_PROCESS);
  }

  #[test]
  fn the_lists_have_no_placeholder() {
    for n in signal_syscalls().into_iter().chain(metadata_syscalls()) {
      assert_ne!(n, nr::NONE);
    }
    assert!(metadata_syscalls().contains(&nr::FCHMODAT));
    assert!(signal_syscalls().contains(&nr::PIDFD_SEND_SIGNAL));
  }

  /// A child of this process chrooted into a directory, its working
  /// directory that root; killed and reaped on drop.
  struct Chrooted(libc::pid_t);

  impl Drop for Chrooted {
    fn drop(&mut self) {
      // SAFETY: our own child, reaped once
      unsafe {
        libc::kill(self.0, libc::SIGKILL);
        libc::waitpid(self.0, std::ptr::null_mut(), 0);
      }
    }
  }

  /// `dir` as a chrooted child's root: by `chroot` itself where this
  /// process may (CAP_SYS_CHROOT), else in a user namespace of its own;
  /// `None` where neither works (a container's seccomp profile, Ubuntu's
  /// AppArmor restriction on unprivileged user namespaces), and then no
  /// unprivileged process here has a root of its own either.
  fn chrooted(dir: &Path) -> Option<Chrooted> {
    let dir = CString::new(dir.as_os_str().as_bytes()).unwrap();
    let mut fds = [0; 2];
    // SAFETY: an out array of two descriptors
    assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
    // SAFETY: the child makes only async-signal-safe syscalls, then exits
    let pid = unsafe { libc::fork() };
    if pid == 0 {
      // SAFETY: raw syscalls on arguments prepared before the fork
      unsafe {
        let ok = (libc::chroot(dir.as_ptr()) == 0
          || (libc::unshare(libc::CLONE_NEWUSER) == 0
            && libc::chroot(dir.as_ptr()) == 0))
          && libc::chdir(c"/".as_ptr()) == 0;
        if ok {
          libc::write(fds[1], b"r".as_ptr().cast(), 1);
          libc::close(fds[1]);
          loop {
            libc::pause();
          }
        }
        libc::_exit(1);
      }
    }
    assert!(pid > 0);
    let child = Chrooted(pid);
    let mut byte = 0u8;
    // SAFETY: our pipe's ends and a one-byte buffer
    let n = unsafe {
      libc::close(fds[1]);
      let n = libc::read(fds[0], (&mut byte as *mut u8).cast(), 1);
      libc::close(fds[0]);
      n
    };
    (n == 1).then_some(child)
  }

  /// A fresh directory (canonical) removed on drop.
  struct Scratch(PathBuf);

  impl Scratch {
    fn new(name: &str) -> Self {
      let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
      let dir = base.join(format!("notify-{name}-{}", std::process::id()));
      let _ = std::fs::remove_dir_all(&dir);
      std::fs::create_dir(&dir).unwrap();
      Scratch(dir)
    }
  }

  impl Drop for Scratch {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }

  fn identity(fd: &OwnedFd) -> (u64, u64) {
    let st = fstat(fd).unwrap();
    (st.st_dev, st.st_ino)
  }

  fn identity_of(path: &Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path).unwrap();
    (m.dev(), m.ino())
  }

  // An absolute symlink in a caller with a root of its own starts from that
  // root, as the caller's own lookup does, not from the supervisor's: the
  // link names a supervisor-side file, and the caller's file at that path
  // is what the supervisor finds
  #[test]
  #[allow(clippy::print_stderr, reason = "test code: a skip note")]
  fn absolute_symlinks_start_from_the_callers_root() {
    let scratch = Scratch::new("abslink");
    let jail = scratch.0.join("jail");
    let host = scratch.0.join("host/target");
    std::fs::create_dir_all(host.parent().unwrap()).unwrap();
    std::fs::write(&host, "supervisor side\n").unwrap();
    let mine = jail.join(host.strip_prefix("/").unwrap());
    std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
    std::fs::write(&mine, "caller side\n").unwrap();
    std::os::unix::fs::symlink(&host, jail.join("link")).unwrap();
    let Some(child) = chrooted(&jail) else {
      eprintln!("skipped: no chroot or user namespace here");
      return;
    };
    for (dirfd, path) in [(AT_FDCWD, c"/link"), (AT_FDCWD, c"link")] {
      let fd = resolve(child.0 as u32, dirfd, path.into(), true, false)
        .unwrap_or_else(|e| panic!("{path:?}: errno {e}"));
      assert_eq!(identity(&fd), identity_of(&mine), "{path:?}");
    }
    // not following the link: the link itself, beneath the caller's root
    let fd =
      resolve(child.0 as u32, AT_FDCWD, c"/link".into(), false, false).unwrap();
    assert!(is_symlink(&fd));
  }

  // `..` stops at a caller's own root, as the caller's own lookup does: a
  // `../../..` escape finds the caller's file, not the supervisor-side file
  // above that root
  #[test]
  #[allow(clippy::print_stderr, reason = "test code: a skip note")]
  fn dot_dot_stops_at_the_callers_root() {
    let scratch = Scratch::new("dotdot");
    let jail = scratch.0.join("x/y/jail");
    std::fs::create_dir_all(jail.join("sub")).unwrap();
    std::fs::write(scratch.0.join("secret"), "supervisor side\n").unwrap();
    std::fs::write(jail.join("secret"), "caller side\n").unwrap();
    let Some(child) = chrooted(&jail) else {
      eprintln!("skipped: no chroot or user namespace here");
      return;
    };
    for path in [
      c"../../../secret",
      c"/../../../secret",
      c"sub/../../../../secret",
    ] {
      let fd = resolve(child.0 as u32, AT_FDCWD, path.into(), true, false)
        .unwrap_or_else(|e| panic!("{path:?}: errno {e}"));
      assert_eq!(identity(&fd), identity_of(&jail.join("secret")), "{path:?}");
    }
  }

  #[test]
  fn proc_stat_of_self() {
    // SAFETY: plain getters
    let (pid, ppid, pgrp, sid) = unsafe {
      (
        libc::getpid(),
        libc::getppid(),
        libc::getpgrp(),
        libc::getsid(0),
      )
    };
    let stat = proc_stat(pid as u32).unwrap();
    assert_eq!(stat.ppid, ppid as u32);
    assert_eq!(stat.pgrp, pgrp as u32);
    assert_eq!(stat.session, sid as u32);
    assert_eq!(tgid_of(pid as u32), Some(pid as u32));
  }
}
