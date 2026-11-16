// Copyright 2018-2026 the Deno authors. MIT license.

//! The OS confinement of a jailed lifecycle script on Linux, applied to the
//! script's process between `fork` and `exec` and inherited by everything it
//! starts (the jail, `cli/supply_chain/jail.rs`, decides the policy; the
//! seccomp rules below are the [`SeccompSpec`] it builds):
//!
//! - Landlock: writes only beneath the jail's roots; reads everywhere but the
//!   credential files (by allowing every path that isn't one, since Landlock
//!   rules only allow). Landlock checks the real file hierarchy, so a symlink
//!   in a writable root can't lead outside it, and it keeps the script from
//!   reading another process's memory or `/proc/<pid>/environ` (ptrace
//!   scoping): the installer's environment stays out of reach.
//! - seccomp: no Unix sockets (other daemons, such as Docker's, would run
//!   anything), and without network no IP sockets either; no `io_uring`
//!   (which can open sockets without `socket(2)`). `no_new_privs` keeps
//!   setuid programs from gaining privileges.
//! - A script that may reach only some hosts (DENEXT.md item 111) goes
//!   through its allow-listing proxy (`crate::proxy`): Landlock ABI 4+
//!   (Linux 6.7+) allows TCP `connect` only to the proxy's port and no
//!   `bind`, and the jail's seccomp rules refuse `listen(2)` (no listening
//!   for connections from outside: `listen` on an unbound socket binds an
//!   ephemeral port itself, an implicit bind Landlock's `BIND_TCP` doesn't
//!   see; GHSA-9363-h8qp-jg52), and seccomp allows only TCP sockets (no UDP, so no DNS or other datagrams; no SCTP
//!   or MPTCP, which Landlock's port rules don't cover) and no TCP Fast Open
//!   sends (`MSG_FASTOPEN` connects without `connect(2)`). Landlock matches
//!   the port, not the address, so where unprivileged user namespaces are
//!   allowed the script also runs in a network namespace of its own
//!   (`unshare(CLONE_NEWUSER | CLONE_NEWNET)`, [`NetnsHandoff`], DENEXT.md
//!   item 124): only a loopback interface exists there, the proxy's
//!   listening socket is bound on it by the script's process before it
//!   starts and handed to the installer over a socketpair (`SCM_RIGHTS`),
//!   and no route leads anywhere else. Without a namespace (a sysctl or
//!   AppArmor restriction), a connection to another machine on the proxy's
//!   (random) port number isn't refused by the kernel, which a warning says;
//!   without ABI 4 either, the jail decides what to do
//!   (`jail::choose_os_net`).
//! - seccomp also refuses `ioctl(TIOCSTI)` / `ioctl(TIOCLINUX)`, which type
//!   into a terminal; the script has none (its own session) and `/dev/tty`
//!   is not writable, but a kernel before 6.2 would still take `TIOCSTI` on
//!   a terminal opened by path.
//! - The installer is a child subreaper (`PR_SET_CHILD_SUBREAPER`): a
//!   descendant the script orphaned becomes the installer's child, and is
//!   ended and reaped with the script's process group
//!   ([`end_adopted_strays`]).

#![allow(
  clippy::disallowed_methods,
  reason = "Landlock rules and namespaces act on the real file system and /proc, never a virtual one"
)]

use std::ffi::CString;
use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::HomeReadPolicy;
use crate::seccomp::SeccompSpec;
use crate::seccomp::SockFilter;
use crate::seccomp::SockFprog;

/// Makes this process a child subreaper, once: orphaned descendants of a
/// jailed script are re-parented here rather than to init, so they can be
/// ended and reaped.
pub fn become_subreaper() {
  static DONE: OnceLock<()> = OnceLock::new();
  DONE.get_or_init(|| {
    // SAFETY: a plain prctl with no pointers
    let res = unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
    if res != 0 {
      log::debug!(
        "jail-scripts: PR_SET_CHILD_SUBREAPER failed: {}",
        io::Error::last_os_error()
      );
    }
  });
}

/// The parent of a process from `/proc/<pid>/stat`.
fn parent_of(pid: u32) -> Option<u32> {
  let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
  // the command name is in parentheses and may hold spaces
  let after = &stat[stat.rfind(')')? + 1..];
  after.split_whitespace().nth(1)?.parse().ok()
}

/// The pids of this process's children.
pub fn children_of_self() -> Vec<u32> {
  // SAFETY: getpid has no failure mode
  let me = unsafe { libc::getpid() } as u32;
  let Ok(entries) = std::fs::read_dir("/proc") else {
    return Vec::new();
  };
  entries
    .flatten()
    .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
    .filter(|pid| parent_of(*pid) == Some(me))
    .collect()
}

/// Ends and reaps the children this process adopted since `before` (the
/// subreaper's) that are not in its own session: what a jailed script left
/// running (it runs in a session of its own, as does anything that detached
/// from it). The installer's own children, in its session, are left alone.
pub fn end_adopted_strays(before: &[u32]) {
  // SAFETY: getsid(0) is this process's session
  let my_session = unsafe { libc::getsid(0) };
  for pid in children_of_self() {
    if before.contains(&pid) {
      continue;
    }
    let Ok(raw) = libc::pid_t::try_from(pid) else {
      continue;
    };
    // SAFETY: a pid lookup; a gone process means -1
    let session = unsafe { libc::getsid(raw) };
    if session < 0 || session == my_session {
      continue;
    }
    log::debug!(
      "jail-scripts: ending a process the script left behind ({pid})"
    );
    // SAFETY: a child of this process, by the /proc scan above; after
    // SIGKILL the blocking wait returns at once
    unsafe {
      libc::kill(raw, libc::SIGKILL);
      let mut status = 0;
      libc::waitpid(raw, &mut status, 0);
    }
  }
}

/// Whether this process can give a child a user and network namespace of
/// its own (unprivileged user namespaces), or why not.
pub fn netns_available() -> Result<(), String> {
  static AVAILABLE: OnceLock<Result<(), String>> = OnceLock::new();
  AVAILABLE
    .get_or_init(|| {
      if let Ok(max) = std::fs::read_to_string("/proc/sys/user/max_user_namespaces")
        && max.trim() == "0"
      {
        return Err(
          "user namespaces are disabled (user.max_user_namespaces = 0)".into(),
        );
      }
      // SAFETY: getuid / getgid have no failure mode
      let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
      let uid_map = format!("{uid} {uid} 1\n");
      let gid_map = format!("{gid} {gid} 1\n");
      // tried in a child, so this process keeps its namespaces; the child
      // runs only async-signal-safe calls and exits with the errno
      // SAFETY: fork; the child does no allocation
      let pid = unsafe { libc::fork() };
      if pid < 0 {
        return Err(format!("fork failed: {}", io::Error::last_os_error()));
      }
      if pid == 0 {
        let code = match enter_user_netns(&uid_map, &gid_map) {
          Ok(()) => 0,
          Err(err) => err.raw_os_error().unwrap_or(libc::EPERM),
        };
        // SAFETY: the child ends here
        unsafe { libc::_exit(code) };
      }
      let mut status = 0;
      // SAFETY: waiting for the child just forked
      unsafe { libc::waitpid(pid, &mut status, 0) };
      if !libc::WIFEXITED(status) {
        return Err("the namespace probe died".into());
      }
      match libc::WEXITSTATUS(status) {
        0 => Ok(()),
        errno => {
          let err = io::Error::from_raw_os_error(errno);
          let hint = if errno == libc::EPERM || errno == libc::EACCES {
            " (unprivileged user namespaces are restricted here: sysctl kernel.unprivileged_userns_clone, kernel.apparmor_restrict_unprivileged_userns, or a seccomp policy)"
          } else {
            ""
          };
          Err(format!("unshare(CLONE_NEWUSER | CLONE_NEWNET) failed: {err}{hint}"))
        }
      }
    })
    .clone()
}

/// Writes `data` to `path` whole (async-signal-safe: open, write, close).
fn write_proc_file(path: &[u8], data: &[u8]) -> io::Result<()> {
  // SAFETY: a NUL-terminated path (the callers pass literals)
  let fd = unsafe {
    libc::open(
      path.as_ptr() as *const libc::c_char,
      libc::O_WRONLY | libc::O_CLOEXEC,
    )
  };
  if fd < 0 {
    return Err(io::Error::last_os_error());
  }
  // SAFETY: a valid fd and buffer
  let written = unsafe {
    libc::write(fd, data.as_ptr() as *const libc::c_void, data.len())
  };
  // SAFETY: just opened
  unsafe { libc::close(fd) };
  if written != data.len() as isize {
    return Err(io::Error::last_os_error());
  }
  Ok(())
}

/// Moves this process into a new user and network namespace, maps its
/// user and group to themselves, and brings the loopback interface up.
/// Async-signal-safe (it runs between fork and exec).
fn enter_user_netns(uid_map: &str, gid_map: &str) -> io::Result<()> {
  // SAFETY: a plain syscall
  if unsafe { libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNET) } != 0 {
    return Err(io::Error::last_os_error());
  }
  write_proc_file(b"/proc/self/setgroups\0", b"deny")?;
  write_proc_file(b"/proc/self/uid_map\0", uid_map.as_bytes())?;
  write_proc_file(b"/proc/self/gid_map\0", gid_map.as_bytes())?;
  // SAFETY: a datagram socket for the interface ioctl
  let sock = unsafe {
    libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0)
  };
  if sock < 0 {
    return Err(io::Error::last_os_error());
  }
  // SAFETY: an ifreq zeroed, then named and flagged; the ioctl reads it
  let res = unsafe {
    let mut req: libc::ifreq = std::mem::zeroed();
    req.ifr_name[0] = b'l' as libc::c_char;
    req.ifr_name[1] = b'o' as libc::c_char;
    req.ifr_ifru.ifru_flags =
      (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;
    libc::ioctl(sock, libc::SIOCSIFFLAGS, &req as *const libc::ifreq)
  };
  let err = io::Error::last_os_error();
  // SAFETY: just opened
  unsafe { libc::close(sock) };
  if res != 0 {
    return Err(err);
  }
  Ok(())
}

/// A port for the proxy inside a script's network namespace: nothing else
/// is on that loopback, so any port does; a random one keeps a script from
/// assuming it.
pub fn pick_proxy_port() -> u16 {
  use rand::Rng;
  rand::thread_rng().gen_range(20_000..60_000)
}

/// Hands the installer a listening socket bound inside the script's
/// network namespace: the script's process (between fork and exec) enters
/// the namespace, binds `127.0.0.1:<port>`, and sends the socket over a
/// socketpair; the installer receives it and serves the proxy on it.
pub struct NetnsHandoff {
  parent: OwnedFd,
  child: OwnedFd,
  port: u16,
}

impl NetnsHandoff {
  pub fn new(port: u16) -> io::Result<Self> {
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
    Ok(Self {
      parent,
      child,
      port,
    })
  }

  /// Adds the namespace setup to the command's `pre_exec` (before the
  /// Landlock / seccomp one, which must come last).
  pub fn apply_to_command(&self, command: &mut tokio::process::Command) {
    let child_fd = self.child.as_raw_fd();
    let port = self.port;
    // SAFETY: getuid / getgid have no failure mode
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let uid_map = format!("{uid} {uid} 1\n");
    let gid_map = format!("{gid} {gid} 1\n");
    // SAFETY: only async-signal-safe syscalls run between fork and exec, on
    // memory allocated before the fork
    unsafe {
      command.pre_exec(move || {
        enter_user_netns(&uid_map, &gid_map)?;
        let listener = bind_loopback(port)?;
        let sent = send_fd(child_fd, listener);
        libc::close(listener);
        libc::close(child_fd);
        sent
      });
    }
  }

  /// Receives the listener the child bound (within a few seconds, or an
  /// error: the child failed before sending it).
  pub async fn receive(self) -> io::Result<tokio::net::TcpListener> {
    let Self { parent, child, .. } = self;
    // the child's end closes here, so a child that exits without sending
    // ends the wait with EOF rather than a timeout
    drop(child);
    let fd = tokio::task::spawn_blocking(move || recv_fd(&parent))
      .await
      .map_err(io::Error::other)??;
    let listener = std::net::TcpListener::from(fd);
    listener.set_nonblocking(true)?;
    tokio::net::TcpListener::from_std(listener)
  }
}

/// Binds and listens on `127.0.0.1:<port>` (async-signal-safe); returns the
/// raw descriptor.
fn bind_loopback(port: u16) -> io::Result<libc::c_int> {
  // SAFETY: a TCP socket
  let fd = unsafe {
    libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0)
  };
  if fd < 0 {
    return Err(io::Error::last_os_error());
  }
  let addr = libc::sockaddr_in {
    sin_family: libc::AF_INET as libc::sa_family_t,
    sin_port: port.to_be(),
    sin_addr: libc::in_addr {
      s_addr: u32::from(std::net::Ipv4Addr::LOCALHOST).to_be(),
    },
    sin_zero: [0; 8],
  };
  // SAFETY: a valid socket and address
  let res = unsafe {
    libc::bind(
      fd,
      &addr as *const libc::sockaddr_in as *const libc::sockaddr,
      std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
    )
  };
  if res != 0 {
    let err = io::Error::last_os_error();
    // SAFETY: just opened
    unsafe { libc::close(fd) };
    return Err(err);
  }
  // SAFETY: a bound socket
  if unsafe { libc::listen(fd, 128) } != 0 {
    let err = io::Error::last_os_error();
    // SAFETY: just opened
    unsafe { libc::close(fd) };
    return Err(err);
  }
  Ok(fd)
}

/// Sends a descriptor over a Unix socket (`SCM_RIGHTS`); async-signal-safe.
pub(crate) fn send_fd(sock: libc::c_int, fd: libc::c_int) -> io::Result<()> {
  let mut byte = [1u8];
  let mut iov = libc::iovec {
    iov_base: byte.as_mut_ptr() as *mut libc::c_void,
    iov_len: 1,
  };
  // SAFETY: CMSG_SPACE of one int is a constant small size
  let space =
    unsafe { libc::CMSG_SPACE(std::mem::size_of::<libc::c_int>() as u32) }
      as usize;
  let mut control = [0u8; 64];
  debug_assert!(space <= control.len());
  // SAFETY: a msghdr over stack buffers, the control message filled through
  // the CMSG macros
  let res = unsafe {
    let mut msg: libc::msghdr = std::mem::zeroed();
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = space as _;
    let cmsg = libc::CMSG_FIRSTHDR(&msg);
    (*cmsg).cmsg_level = libc::SOL_SOCKET;
    (*cmsg).cmsg_type = libc::SCM_RIGHTS;
    (*cmsg).cmsg_len =
      libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as u32) as _;
    std::ptr::write_unaligned(libc::CMSG_DATA(cmsg) as *mut libc::c_int, fd);
    libc::sendmsg(sock, &msg, 0)
  };
  if res < 0 {
    return Err(io::Error::last_os_error());
  }
  Ok(())
}

/// Receives one descriptor over a Unix socket (`SCM_RIGHTS`), waiting at
/// most a few seconds.
pub(crate) fn recv_fd(sock: &OwnedFd) -> io::Result<OwnedFd> {
  let mut poll = libc::pollfd {
    fd: sock.as_raw_fd(),
    events: libc::POLLIN,
    revents: 0,
  };
  // SAFETY: one pollfd
  let ready = unsafe { libc::poll(&mut poll, 1, 10_000) };
  if ready < 0 {
    return Err(io::Error::last_os_error());
  }
  if ready == 0 {
    return Err(io::Error::new(
      io::ErrorKind::TimedOut,
      "the jailed script's process didn't hand its listener over",
    ));
  }
  let mut byte = [0u8];
  let mut iov = libc::iovec {
    iov_base: byte.as_mut_ptr() as *mut libc::c_void,
    iov_len: 1,
  };
  let mut control = [0u8; 64];
  // SAFETY: a msghdr over stack buffers
  let (received, fd) = unsafe {
    let mut msg: libc::msghdr = std::mem::zeroed();
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = control.len() as _;
    let received =
      libc::recvmsg(sock.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC);
    let mut fd = None;
    if received > 0 {
      let cmsg = libc::CMSG_FIRSTHDR(&msg);
      if !cmsg.is_null()
        && (*cmsg).cmsg_level == libc::SOL_SOCKET
        && (*cmsg).cmsg_type == libc::SCM_RIGHTS
      {
        fd = Some(std::ptr::read_unaligned(
          libc::CMSG_DATA(cmsg) as *const libc::c_int
        ));
      }
    }
    (received, fd)
  };
  if received < 0 {
    return Err(io::Error::last_os_error());
  }
  match fd {
    // SAFETY: a descriptor the kernel just installed for this process
    Some(fd) if fd >= 0 => Ok(unsafe { OwnedFd::from_raw_fd(fd) }),
    _ => Err(io::Error::new(
      io::ErrorKind::UnexpectedEof,
      "the jailed script's process ended before handing its listener over",
    )),
  }
}

// the same numbers on every architecture since Linux 5.13
pub(crate) const SYS_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
pub(crate) const SYS_LANDLOCK_ADD_RULE: libc::c_long = 445;
pub(crate) const SYS_LANDLOCK_RESTRICT_SELF: libc::c_long = 446;
const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;
pub(crate) const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;
pub(crate) const LANDLOCK_RULE_NET_PORT: u32 = 2;
pub(crate) const ACCESS_NET_BIND_TCP: u64 = 1 << 0;
pub(crate) const ACCESS_NET_CONNECT_TCP: u64 = 1 << 1;
/// The first ABI with network rules (Linux 6.7).
pub const LANDLOCK_NET_ABI: u32 = 4;

pub(crate) const ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
pub(crate) const ACCESS_FS_READ_FILE: u64 = 1 << 2;
pub(crate) const ACCESS_FS_READ_DIR: u64 = 1 << 3;
pub(crate) const ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
pub(crate) const ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
pub(crate) const ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
pub(crate) const ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
pub(crate) const ACCESS_FS_MAKE_REG: u64 = 1 << 8;
pub(crate) const ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
pub(crate) const ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
pub(crate) const ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
pub(crate) const ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
pub(crate) const ACCESS_FS_REFER: u64 = 1 << 13;
pub(crate) const ACCESS_FS_TRUNCATE: u64 = 1 << 14;

/// The rights that apply to a file (not a directory) in a rule.
pub(crate) const FILE_RIGHTS: u64 = 1 // execute
  | ACCESS_FS_WRITE_FILE
  | ACCESS_FS_READ_FILE
  | ACCESS_FS_TRUNCATE;

#[repr(C)]
struct RulesetAttr {
  handled_access_fs: u64,
  /// ABI 4+: passed only when the size given covers it
  handled_access_net: u64,
}

#[repr(C, packed)]
pub(crate) struct NetPortAttr {
  pub(crate) allowed_access: u64,
  pub(crate) port: u64,
}

#[repr(C, packed)]
pub(crate) struct PathBeneathAttr {
  pub(crate) allowed_access: u64,
  pub(crate) parent_fd: i32,
}

/// The Landlock ABI version the kernel supports, or why there is none.
pub fn landlock_abi() -> Result<u32, String> {
  static ABI: OnceLock<Result<u32, String>> = OnceLock::new();
  ABI
    .get_or_init(|| {
      // SAFETY: a version query takes no attribute
      let abi = unsafe {
        libc::syscall(
          SYS_LANDLOCK_CREATE_RULESET,
          std::ptr::null::<RulesetAttr>(),
          0usize,
          LANDLOCK_CREATE_RULESET_VERSION,
        )
      };
      if abi < 1 {
        let err = io::Error::last_os_error();
        Err(format!(
          "Landlock is not available ({err}; it needs Linux 5.13+ with Landlock enabled in the kernel's LSM list)"
        ))
      } else {
        Ok(abi as u32)
      }
    })
    .clone()
}

fn write_rights(abi: u32) -> u64 {
  let mut rights = ACCESS_FS_WRITE_FILE
    | ACCESS_FS_REMOVE_DIR
    | ACCESS_FS_REMOVE_FILE
    | ACCESS_FS_MAKE_CHAR
    | ACCESS_FS_MAKE_DIR
    | ACCESS_FS_MAKE_REG
    | ACCESS_FS_MAKE_SOCK
    | ACCESS_FS_MAKE_FIFO
    | ACCESS_FS_MAKE_BLOCK
    | ACCESS_FS_MAKE_SYM;
  if abi >= 2 {
    rights |= ACCESS_FS_REFER;
  }
  if abi >= 3 {
    rights |= ACCESS_FS_TRUNCATE;
  }
  rights
}

fn add_rule(ruleset: &OwnedFd, path: &Path, access: u64) -> io::Result<()> {
  let c_path = CString::new(path.as_os_str().as_bytes())
    .map_err(|_| io::Error::other("a path with a NUL"))?;
  // SAFETY: a valid C string
  let fd =
    unsafe { libc::open(c_path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
  if fd < 0 {
    // gone or unreadable: nothing to allow
    return Ok(());
  }
  // SAFETY: just opened
  let fd = unsafe { OwnedFd::from_raw_fd(fd) };
  let is_dir = std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false);
  let allowed_access = if is_dir { access } else { access & FILE_RIGHTS };
  if allowed_access == 0 {
    return Ok(());
  }
  let attr = PathBeneathAttr {
    allowed_access,
    parent_fd: fd.as_raw_fd(),
  };
  // SAFETY: a valid ruleset fd and attribute
  let res = unsafe {
    libc::syscall(
      SYS_LANDLOCK_ADD_RULE,
      ruleset.as_raw_fd(),
      LANDLOCK_RULE_PATH_BENEATH,
      &attr as *const PathBeneathAttr,
      0u32,
    )
  };
  if res != 0 {
    return Err(io::Error::last_os_error());
  }
  Ok(())
}

/// How many directory levels below `/` the read walk goes toward a denied
/// path; a denied path deeper than that leaves its enclosing directory
/// unreadable.
const READ_WALK_MAX_DEPTH: usize = 32;

/// How many levels below `/` a read walk root is (the depth the walk starts
/// at: its limit counts from `/`, whatever the root).
fn root_depth(root: &Path) -> usize {
  root.components().count().saturating_sub(1)
}

/// Allows reading everything but `deny` (canonical paths): every directory
/// entry on the way down to a denied path is allowed but that one.
fn add_read_rules(
  ruleset: &OwnedFd,
  dir: &Path,
  deny: &[PathBuf],
  home_read: Option<&HomeReadPolicy>,
  depth: usize,
) -> io::Result<()> {
  let rights = ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR;
  walk_read_rules(dir, deny, home_read, depth, &mut |path| {
    add_rule(ruleset, path, rights)
  })
}

/// The read walk of [`add_read_rules`], calling `allow` for each path to
/// allow. Past [`READ_WALK_MAX_DEPTH`] a directory that holds a denied path
/// is not allowed at all: it fails closed rather than allowing the denied
/// path with it. A symlink's rule follows the link, so one is allowed only
/// when where it resolves to is neither denied, nor holds a denied path,
/// nor is kept out by `home_read`.
fn walk_read_rules(
  dir: &Path,
  deny: &[PathBuf],
  home_read: Option<&HomeReadPolicy>,
  depth: usize,
  allow: &mut dyn FnMut(&Path) -> io::Result<()>,
) -> io::Result<()> {
  if deny.iter().any(|d| d == dir) {
    return Ok(());
  }
  if !deny.iter().any(|d| d.starts_with(dir)) {
    return allow(dir);
  }
  if depth > READ_WALK_MAX_DEPTH {
    return Ok(());
  }
  let Ok(entries) = std::fs::read_dir(dir) else {
    return Ok(());
  };
  for entry in entries.flatten() {
    let path = entry.path();
    let is_symlink = entry.file_type().map(|t| t.is_symlink()).unwrap_or(true);
    if is_symlink {
      // a rule follows the link: allow it only when where it leads is
      // neither denied nor holds a denied path
      let Ok(target) = std::fs::canonicalize(&path) else {
        continue;
      };
      if deny
        .iter()
        .any(|d| target.starts_with(d) || d.starts_with(&target))
        || home_read.is_some_and(|h| h.keeps_out(&target))
      {
        continue;
      }
      allow(&path)?;
    } else {
      walk_read_rules(&path, deny, home_read, depth + 1, allow)?;
    }
  }
  Ok(())
}

/// Whether Landlock can hold TCP connections to one port (ABI 4+).
pub fn can_confine_to_port() -> bool {
  landlock_abi().is_ok_and(|abi| abi >= LANDLOCK_NET_ABI)
}

/// Builds the Landlock ruleset for a jailed script; with `proxy_port`, TCP
/// `connect` is allowed only to that port and `bind` not at all, where the
/// kernel has network rules (ABI 4+; before that the script's own network
/// namespace is what holds it to the proxy, `jail::choose_os_net`).
pub fn build_ruleset(
  write_roots: &[PathBuf],
  deny_read: &[PathBuf],
  proxy_port: Option<u16>,
) -> io::Result<OwnedFd> {
  build_ruleset_opts(write_roots, deny_read, None, proxy_port, true)
}

/// Where the read rules start, each with the paths left out beneath it:
/// everything but `deny_read` (and, with a home policy, the home directories),
/// then each path the policy allows in the home directory but `deny_read`
/// (DENEXT.md item 437). Landlock rules only add access, so a path allowed
/// under the home directory is a rule of its own, and the denied paths
/// within it stay out of it.
fn read_rule_roots(
  deny_read: &[PathBuf],
  home_read: Option<&HomeReadPolicy>,
) -> Vec<(PathBuf, Vec<PathBuf>)> {
  let mut outer = deny_read.to_vec();
  let mut roots = Vec::new();
  if let Some(home_read) = home_read {
    outer.extend(home_read.homes.iter().cloned());
    for allowed in &home_read.allowed {
      if deny_read.iter().any(|d| allowed.starts_with(d)) {
        continue;
      }
      roots.push((allowed.clone(), deny_read.to_vec()));
    }
  }
  roots.insert(0, (PathBuf::from("/"), outer));
  roots
}

/// [`build_ruleset`], with no reads under the home directory but the paths
/// `home_read` allows, when given (DENEXT.md item 437).
pub fn build_ruleset_with_home_read(
  write_roots: &[PathBuf],
  deny_read: &[PathBuf],
  home_read: Option<&HomeReadPolicy>,
  proxy_port: Option<u16>,
) -> io::Result<OwnedFd> {
  build_ruleset_opts(write_roots, deny_read, home_read, proxy_port, true)
}

/// [`build_ruleset_with_home_read`], with the host's `/dev/shm` granted only
/// when `host_shm` is set: a producer that gives the process a private one
/// ([`PrivateShm`]), or none at all, leaves it out, since every same-user
/// process's POSIX shared memory and semaphores live there.
pub fn build_ruleset_opts(
  write_roots: &[PathBuf],
  deny_read: &[PathBuf],
  home_read: Option<&HomeReadPolicy>,
  proxy_port: Option<u16>,
  host_shm: bool,
) -> io::Result<OwnedFd> {
  let abi = landlock_abi().map_err(io::Error::other)?;
  let proxy_port = proxy_port.filter(|_| abi >= LANDLOCK_NET_ABI);
  let write = write_rights(abi);
  let read = if deny_read.is_empty() && home_read.is_none() {
    0
  } else {
    ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR
  };
  let attr = RulesetAttr {
    handled_access_fs: write | read,
    // binding (to listen for connections from outside) has no rule: refused
    handled_access_net: if proxy_port.is_some() {
      ACCESS_NET_CONNECT_TCP | ACCESS_NET_BIND_TCP
    } else {
      0
    },
  };
  // an older kernel refuses a larger attribute with fields it doesn't know
  let size = if abi >= LANDLOCK_NET_ABI {
    std::mem::size_of::<RulesetAttr>()
  } else {
    std::mem::size_of::<u64>()
  };
  // SAFETY: a valid attribute of at least the given size
  let fd = unsafe {
    libc::syscall(
      SYS_LANDLOCK_CREATE_RULESET,
      &attr as *const RulesetAttr,
      size,
      0u32,
    )
  };
  if fd < 0 {
    return Err(io::Error::last_os_error());
  }
  // SAFETY: just created
  let ruleset = unsafe { OwnedFd::from_raw_fd(fd as i32) };
  for root in write_roots {
    add_rule(&ruleset, root, write)?;
  }
  // not /dev/tty: output goes through the pipes, and a terminal is what
  // TIOCSTI types into (DENEXT.md item 124)
  // read too: a producer may deny reading the rest of /dev (the terminals,
  // audio and video capture, raw USB)
  for dev in [
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
  ] {
    add_rule(&ruleset, Path::new(dev), (write | read) & FILE_RIGHTS)?;
  }
  if host_shm {
    add_rule(&ruleset, Path::new("/dev/shm"), write | read)?;
  }
  if read != 0 {
    for (root, deny) in read_rule_roots(deny_read, home_read) {
      add_read_rules(&ruleset, &root, &deny, home_read, root_depth(&root))?;
    }
  }
  if let Some(port) = proxy_port {
    let attr = NetPortAttr {
      allowed_access: ACCESS_NET_CONNECT_TCP,
      port: u64::from(port),
    };
    // SAFETY: a valid ruleset fd and attribute
    let res = unsafe {
      libc::syscall(
        SYS_LANDLOCK_ADD_RULE,
        ruleset.as_raw_fd(),
        LANDLOCK_RULE_NET_PORT,
        &attr as *const NetPortAttr,
        0u32,
      )
    };
    if res != 0 {
      return Err(io::Error::last_os_error());
    }
  }
  Ok(ruleset)
}

/// Applies the confinement to `command`'s process before it starts the
/// program: `no_new_privs`, the Landlock ruleset, then the seccomp program
/// `seccomp` assembles. `ruleset` must stay open until the command has been
/// spawned.
pub fn apply_to_command(
  command: &mut tokio::process::Command,
  ruleset: &OwnedFd,
  seccomp: &SeccompSpec,
) {
  let ruleset_fd = ruleset.as_raw_fd();
  let program = seccomp.program();
  // SAFETY: only async-signal-safe syscalls run between fork and exec, on
  // memory allocated before the fork
  unsafe {
    command.pre_exec(move || {
      if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
        return Err(io::Error::last_os_error());
      }
      if libc::syscall(SYS_LANDLOCK_RESTRICT_SELF, ruleset_fd, 0u32) != 0 {
        return Err(io::Error::last_os_error());
      }
      if !program.is_empty() {
        let prog = SockFprog {
          len: (program.len() / 8) as u16,
          filter: program.as_ptr() as *const SockFilter,
        };
        if libc::prctl(
          libc::PR_SET_SECCOMP,
          libc::SECCOMP_MODE_FILTER,
          &prog as *const SockFprog,
          0,
          0,
        ) != 0
        {
          return Err(io::Error::last_os_error());
        }
      }
      Ok(())
    });
  }
}

const MOUNT_SHM_DATA: &[u8] = b"mode=1777\0";

/// Gives the process a mount namespace of its own (and a user namespace,
/// unless it already has one) and a fresh tmpfs on `/dev/shm`: what it
/// creates there no other process sees, and the host's segments are out of
/// its sight. Async-signal-safe (between fork and exec).
fn enter_private_shm(
  in_userns: bool,
  uid_map: &str,
  gid_map: &str,
) -> io::Result<()> {
  let flags = if in_userns {
    libc::CLONE_NEWNS
  } else {
    libc::CLONE_NEWUSER | libc::CLONE_NEWNS
  };
  // SAFETY: plain syscalls on NUL-terminated literals
  unsafe {
    if libc::unshare(flags) != 0 {
      return Err(io::Error::last_os_error());
    }
    if !in_userns {
      write_proc_file(b"/proc/self/setgroups\0", b"deny")?;
      write_proc_file(b"/proc/self/uid_map\0", uid_map.as_bytes())?;
      write_proc_file(b"/proc/self/gid_map\0", gid_map.as_bytes())?;
    }
    // nothing mounted here propagates back to the host
    if libc::mount(
      std::ptr::null(),
      c"/".as_ptr(),
      std::ptr::null(),
      libc::MS_REC | libc::MS_PRIVATE,
      std::ptr::null(),
    ) != 0
    {
      return Err(io::Error::last_os_error());
    }
    if libc::mount(
      c"tmpfs".as_ptr(),
      c"/dev/shm".as_ptr(),
      c"tmpfs".as_ptr(),
      libc::MS_NOSUID | libc::MS_NODEV,
      MOUNT_SHM_DATA.as_ptr() as *const libc::c_void,
    ) != 0
    {
      return Err(io::Error::last_os_error());
    }
  }
  Ok(())
}

/// Whether this process can give a child a private `/dev/shm` (a user and
/// mount namespace of its own, and a tmpfs mount there), or why not.
pub fn private_shm_available() -> Result<(), String> {
  static AVAILABLE: OnceLock<Result<(), String>> = OnceLock::new();
  AVAILABLE
    .get_or_init(|| {
      // SAFETY: getuid / getgid have no failure mode
      let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
      let uid_map = format!("{uid} {uid} 1\n");
      let gid_map = format!("{gid} {gid} 1\n");
      // tried in a child, so this process keeps its namespaces
      // SAFETY: fork; the child makes only async-signal-safe calls
      let pid = unsafe { libc::fork() };
      if pid < 0 {
        return Err(format!("fork failed: {}", io::Error::last_os_error()));
      }
      if pid == 0 {
        let code = match enter_private_shm(false, &uid_map, &gid_map) {
          Ok(()) => 0,
          Err(err) => err.raw_os_error().unwrap_or(libc::EPERM),
        };
        // SAFETY: the child ends here
        unsafe { libc::_exit(code) };
      }
      let mut status = 0;
      // SAFETY: waiting for the child just forked
      unsafe { libc::waitpid(pid, &mut status, 0) };
      if !libc::WIFEXITED(status) {
        return Err("the namespace probe died".into());
      }
      match libc::WEXITSTATUS(status) {
        0 => Ok(()),
        errno => Err(format!(
          "a user and mount namespace with a tmpfs failed: {}",
          io::Error::from_raw_os_error(errno)
        )),
      }
    })
    .clone()
}

/// A private tmpfs on `/dev/shm` for a confined command (see
/// [`private_shm_available`]), granted in its Landlock ruleset: a rule is
/// tied to the directory's inode, so the new mount's root gets its own rule,
/// added from the child before the ruleset is enforced. Use it with a
/// ruleset from [`build_ruleset_opts`] without the host's `/dev/shm`.
pub struct PrivateShm {
  rights: u64,
  in_userns: bool,
}

impl PrivateShm {
  /// `reads_handled`: the ruleset handles reads (it was built with a
  /// non-empty deny list); `in_userns`: the command's process already
  /// enters a user namespace of its own first ([`NetnsHandoff`]).
  pub fn new(reads_handled: bool, in_userns: bool) -> io::Result<Self> {
    let abi = landlock_abi().map_err(io::Error::other)?;
    let mut rights = write_rights(abi);
    if reads_handled {
      rights |= ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR;
    }
    Ok(Self { rights, in_userns })
  }

  /// Adds the mount to the command's `pre_exec`: after a [`NetnsHandoff`]'s,
  /// before [`apply_to_command`]'s (which enforces the ruleset; Landlock
  /// refuses mounts after that).
  pub fn apply_to_command(
    &self,
    command: &mut tokio::process::Command,
    ruleset: &OwnedFd,
  ) {
    let ruleset_fd = ruleset.as_raw_fd();
    let rights = self.rights;
    let in_userns = self.in_userns;
    // SAFETY: getuid / getgid have no failure mode
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let uid_map = format!("{uid} {uid} 1\n");
    let gid_map = format!("{gid} {gid} 1\n");
    // SAFETY: only async-signal-safe syscalls run between fork and exec, on
    // memory allocated before the fork
    unsafe {
      command.pre_exec(move || {
        enter_private_shm(in_userns, &uid_map, &gid_map)?;
        let fd =
          libc::open(c"/dev/shm".as_ptr(), libc::O_PATH | libc::O_CLOEXEC);
        if fd < 0 {
          return Err(io::Error::last_os_error());
        }
        let attr = PathBeneathAttr {
          allowed_access: rights,
          parent_fd: fd,
        };
        let res = libc::syscall(
          SYS_LANDLOCK_ADD_RULE,
          ruleset_fd,
          LANDLOCK_RULE_PATH_BENEATH,
          &attr as *const PathBeneathAttr,
          0u32,
        );
        let err = io::Error::last_os_error();
        libc::close(fd);
        if res != 0 {
          return Err(err);
        }
        Ok(())
      });
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A fresh, canonical scratch directory for one test.
  fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
      .join(format!("deno_sandbox_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
  }

  /// The paths the read walk allows from `root`, with `root` counted at
  /// its real depth below `/` (as the ruleset builder starts it).
  fn allowed(root: &Path, deny: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let depth = root_depth(root);
    walk_read_rules(root, deny, None, depth, &mut |p| {
      out.push(p.to_path_buf());
      Ok(())
    })
    .unwrap();
    out
  }

  #[test]
  fn read_walk_allows_siblings_not_the_denied_path() {
    let root = scratch("read_walk_siblings");
    std::fs::create_dir_all(root.join("a/b")).unwrap();
    std::fs::write(root.join("a/b/secret"), "x").unwrap();
    std::fs::write(root.join("a/b/other"), "x").unwrap();
    std::fs::write(root.join("a/sibling"), "x").unwrap();
    let deny = vec![root.join("a/b/secret")];
    let allowed = allowed(&root, &deny);
    assert!(allowed.contains(&root.join("a/b/other")));
    assert!(allowed.contains(&root.join("a/sibling")));
    assert!(allowed.iter().all(|p| !deny[0].starts_with(p)));
    std::fs::remove_dir_all(&root).unwrap();
  }

  #[test]
  fn read_walk_fails_closed_past_its_depth_limit() {
    let root = scratch("read_walk_depth");
    // a denied file further below `/` than the walk goes
    let levels = READ_WALK_MAX_DEPTH + 4 - (root.components().count() - 1);
    let mut deep = root.clone();
    for _ in 0..levels {
      deep.push("d");
    }
    std::fs::create_dir_all(&deep).unwrap();
    let secret = deep.join("secret");
    std::fs::write(&secret, "x").unwrap();
    std::fs::write(deep.join("other"), "x").unwrap();
    assert!(secret.components().count() - 1 > READ_WALK_MAX_DEPTH + 1);
    let deny = vec![secret.clone()];
    let allowed = allowed(&root, &deny);
    // nothing allowed holds the denied file, at any depth
    assert!(
      allowed.iter().all(|p| !secret.starts_with(p)),
      "{allowed:?}"
    );
    std::fs::remove_dir_all(&root).unwrap();
  }

  // every read walk root starts at its own depth below `/`, so the depth
  // limit means the same for a home root as for `/`
  #[test]
  fn read_walk_roots_start_at_their_depth() {
    assert_eq!(root_depth(Path::new("/")), 0);
    assert_eq!(root_depth(Path::new("/home/u/proj")), 3);
    let deny = vec![PathBuf::from("/home/u/.ssh")];
    let home = HomeReadPolicy {
      homes: vec![PathBuf::from("/home/u")],
      allowed: vec![PathBuf::from("/home/u/proj")],
    };
    let depths: Vec<usize> = read_rule_roots(&deny, Some(&home))
      .iter()
      .map(|(root, _)| root_depth(root))
      .collect();
    assert_eq!(depths, vec![0, 3]);
  }

  // a symlink in an allowed home path is followed only where it resolves to
  // a path the home policy allows, or outside the home directory
  #[test]
  fn read_walk_follows_links_only_where_the_home_policy_allows() {
    let root = scratch("read_walk_home_links");
    let home = root.join("home");
    let proj = home.join("proj");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::create_dir_all(home.join("sibling")).unwrap();
    std::fs::create_dir_all(home.join("tools")).unwrap();
    std::fs::create_dir_all(root.join("elsewhere")).unwrap();
    std::fs::write(proj.join(".env"), "x").unwrap();
    std::os::unix::fs::symlink(home.join("sibling"), proj.join("to_sibling"))
      .unwrap();
    std::os::unix::fs::symlink(&home, proj.join("to_home")).unwrap();
    std::os::unix::fs::symlink(home.join("tools"), proj.join("to_tools"))
      .unwrap();
    std::os::unix::fs::symlink(root.join("elsewhere"), proj.join("to_else"))
      .unwrap();
    let policy = HomeReadPolicy {
      homes: vec![home.clone()],
      allowed: vec![proj.clone(), home.join("tools")],
    };
    // a denied file inside the allowed root is what makes the walk run
    let deny = vec![proj.join(".env")];
    let mut out = Vec::new();
    let depth = proj.components().count() - 1;
    walk_read_rules(&proj, &deny, Some(&policy), depth, &mut |p| {
      out.push(p.to_path_buf());
      Ok(())
    })
    .unwrap();
    assert!(out.contains(&proj.join("src")), "{out:?}");
    assert!(out.contains(&proj.join("to_tools")), "{out:?}");
    assert!(out.contains(&proj.join("to_else")), "{out:?}");
    assert!(!out.contains(&proj.join("to_sibling")), "{out:?}");
    assert!(!out.contains(&proj.join("to_home")), "{out:?}");
    assert!(!out.contains(&proj.join(".env")), "{out:?}");
    std::fs::remove_dir_all(&root).unwrap();
  }

  // DENEXT.md item 437: with a home policy the home directory is left out of
  // the rule from "/", and each allowed path in it is a rule of its own that
  // still leaves the denied paths out
  #[test]
  fn read_rule_roots_leave_the_home_directory_out() {
    let deny = vec![
      PathBuf::from("/home/u/.ssh"),
      PathBuf::from("/home/u/proj/.env"),
    ];
    assert_eq!(
      read_rule_roots(&deny, None),
      vec![(PathBuf::from("/"), deny.clone())]
    );
    let home = HomeReadPolicy {
      homes: vec![PathBuf::from("/home/u")],
      allowed: vec![
        PathBuf::from("/home/u/proj"),
        PathBuf::from("/home/u/.ssh/inside"),
      ],
    };
    let mut outer = deny.clone();
    outer.push(PathBuf::from("/home/u"));
    // every home directory of the policy is left out
    let two = HomeReadPolicy {
      homes: vec![PathBuf::from("/home/u"), PathBuf::from("/tmp/h")],
      allowed: vec![],
    };
    let mut outer_two = outer.clone();
    outer_two.push(PathBuf::from("/tmp/h"));
    assert_eq!(
      read_rule_roots(&deny, Some(&two)),
      vec![(PathBuf::from("/"), outer_two)]
    );
    assert_eq!(
      read_rule_roots(&deny, Some(&home)),
      vec![
        (PathBuf::from("/"), outer),
        // a path inside a denied path gets no rule
        (PathBuf::from("/home/u/proj"), deny.clone()),
      ]
    );
  }
}
