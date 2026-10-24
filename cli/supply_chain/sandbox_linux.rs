// Copyright 2018-2026 the Deno authors. MIT license.

//! The OS confinement of a jailed lifecycle script on Linux, applied to the
//! script's process between `fork` and `exec` and inherited by everything it
//! starts:
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
//!   through its allow-listing proxy (`jail_proxy`): Landlock ABI 4+
//!   (Linux 6.7+) allows TCP `connect` only to the proxy's port and no
//!   `bind` (no listening for connections from outside), and seccomp
//!   allows only TCP sockets (no UDP, so no DNS or other datagrams; no SCTP
//!   or MPTCP, which Landlock's port rules don't cover) and no TCP Fast Open
//!   sends (`MSG_FASTOPEN` connects without `connect(2)`). Landlock matches
//!   the port, not the address: a connection to another machine on the
//!   proxy's (random) port number isn't refused by the kernel. Without ABI 4
//!   the jail decides what to do (`jail::choose_os_net`).

use std::ffi::CString;
use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

use super::jail::OsNet;

// the same numbers on every architecture since Linux 5.13
const SYS_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
const SYS_LANDLOCK_ADD_RULE: libc::c_long = 445;
const SYS_LANDLOCK_RESTRICT_SELF: libc::c_long = 446;
const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;
const LANDLOCK_RULE_NET_PORT: u32 = 2;
const ACCESS_NET_BIND_TCP: u64 = 1 << 0;
const ACCESS_NET_CONNECT_TCP: u64 = 1 << 1;
/// The first ABI with network rules (Linux 6.7).
pub const LANDLOCK_NET_ABI: u32 = 4;

const ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
const ACCESS_FS_READ_FILE: u64 = 1 << 2;
const ACCESS_FS_READ_DIR: u64 = 1 << 3;
const ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
const ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
const ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
const ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
const ACCESS_FS_MAKE_REG: u64 = 1 << 8;
const ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
const ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
const ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
const ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
const ACCESS_FS_REFER: u64 = 1 << 13;
const ACCESS_FS_TRUNCATE: u64 = 1 << 14;

/// The rights that apply to a file (not a directory) in a rule.
const FILE_RIGHTS: u64 = 1 // execute
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
struct NetPortAttr {
  allowed_access: u64,
  port: u64,
}

#[repr(C, packed)]
struct PathBeneathAttr {
  allowed_access: u64,
  parent_fd: i32,
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

/// Allows reading everything but `deny` (canonical paths): every directory
/// entry on the way down to a denied path is allowed but that one.
fn add_read_rules(
  ruleset: &OwnedFd,
  dir: &Path,
  deny: &[PathBuf],
  depth: usize,
) -> io::Result<()> {
  let rights = ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR;
  if deny.iter().any(|d| d == dir) {
    return Ok(());
  }
  if !deny.iter().any(|d| d.starts_with(dir)) || depth > 32 {
    return add_rule(ruleset, dir, rights);
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
      let Ok(target) = crate::util::fs::canonicalize_path(&path) else {
        continue;
      };
      if deny
        .iter()
        .any(|d| target.starts_with(d) || d.starts_with(&target))
      {
        continue;
      }
      add_rule(ruleset, &path, rights)?;
    } else {
      add_read_rules(ruleset, &path, deny, depth + 1)?;
    }
  }
  Ok(())
}

/// Whether Landlock can hold TCP connections to one port (ABI 4+).
pub fn can_confine_to_port() -> bool {
  landlock_abi().is_ok_and(|abi| abi >= LANDLOCK_NET_ABI)
}

/// Builds the Landlock ruleset for a jailed script; with `proxy_port`, TCP
/// `connect` is allowed only to that port (it needs ABI 4+).
pub fn build_ruleset(
  write_roots: &[PathBuf],
  deny_read: &[PathBuf],
  proxy_port: Option<u16>,
) -> io::Result<OwnedFd> {
  let abi = landlock_abi().map_err(io::Error::other)?;
  if proxy_port.is_some() && abi < LANDLOCK_NET_ABI {
    return Err(io::Error::other(format!(
      "Landlock ABI {abi} has no network rules (ABI {LANDLOCK_NET_ABI}, Linux 6.7+, has)"
    )));
  }
  let write = write_rights(abi);
  let read = if deny_read.is_empty() {
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
  for dev in [
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/tty",
    "/dev/random",
    "/dev/urandom",
  ] {
    add_rule(&ruleset, Path::new(dev), write & FILE_RIGHTS)?;
  }
  add_rule(&ruleset, Path::new("/dev/shm"), write)?;
  if read != 0 {
    add_read_rules(&ruleset, Path::new("/"), deny_read, 0)?;
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

// seccomp
#[repr(C)]
#[derive(Clone, Copy)]
struct SockFilter {
  code: u16,
  jt: u8,
  jf: u8,
  k: u32,
}

#[repr(C)]
struct SockFprog {
  len: u16,
  filter: *const SockFilter,
}

const BPF_LD_W_ABS: u16 = 0x20;
const BPF_ALU_AND_K: u16 = 0x54;
const BPF_JEQ_K: u16 = 0x15;
const BPF_JGE_K: u16 = 0x35;
const BPF_RET_K: u16 = 0x06;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_DATA_NR: u32 = 0;
const SECCOMP_DATA_ARCH: u32 = 4;
const SECCOMP_DATA_ARG0: u32 = 16;
/// The low 32 bits of argument `n` (little-endian).
const fn seccomp_arg(n: u32) -> u32 {
  SECCOMP_DATA_ARG0 + 8 * n
}
const SOCK_TYPE_MASK: u32 = 0xf;
const SOCK_STREAM: u32 = 1;
const IPPROTO_TCP: u32 = 6;
const MSG_FASTOPEN: u32 = 0x2000_0000;
const AF_NETLINK: u32 = 16;
const AF_INET: u32 = 2;
const AF_INET6: u32 = 10;
const NR_IO_URING_SETUP: u32 = 425;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xC000_003E;
#[cfg(target_arch = "x86_64")]
const NR_SOCKET: u32 = 41;
#[cfg(target_arch = "x86_64")]
const NR_SENDTO: u32 = 44;
#[cfg(target_arch = "x86_64")]
const NR_SENDMSG: u32 = 46;
#[cfg(target_arch = "x86_64")]
const NR_SENDMMSG: u32 = 307;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xC000_00B7;
#[cfg(target_arch = "aarch64")]
const NR_SOCKET: u32 = 198;
#[cfg(target_arch = "aarch64")]
const NR_SENDTO: u32 = 206;
#[cfg(target_arch = "aarch64")]
const NR_SENDMSG: u32 = 211;
#[cfg(target_arch = "aarch64")]
const NR_SENDMMSG: u32 = 269;

const fn stmt(code: u16, k: u32) -> SockFilter {
  SockFilter {
    code,
    jt: 0,
    jf: 0,
    k,
  }
}

const fn jump(code: u16, k: u32, jt: u8, jf: u8) -> SockFilter {
  SockFilter { code, jt, jf, k }
}

/// The seccomp program: `socket(2)` only for netlink, and for IPv4/IPv6 when
/// the network is open (TCP only, and no Fast Open sends, when it goes
/// through the proxy); `io_uring_setup` fails; another architecture's
/// syscalls (x32, the 32-bit compat ABI) kill the process.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn seccomp_program(net: OsNet) -> Vec<u8> {
  let errno = |e: i32| SECCOMP_RET_ERRNO | (e as u32 & 0xffff);
  let mut f = vec![
    stmt(BPF_LD_W_ABS, SECCOMP_DATA_ARCH),
    jump(BPF_JEQ_K, AUDIT_ARCH, 1, 0),
    stmt(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
    stmt(BPF_LD_W_ABS, SECCOMP_DATA_NR),
  ];
  if cfg!(target_arch = "x86_64") {
    // x32 syscalls carry bit 30
    f.push(jump(BPF_JGE_K, 0x4000_0000, 0, 1));
    f.push(stmt(BPF_RET_K, SECCOMP_RET_KILL_PROCESS));
  }
  f.push(jump(BPF_JEQ_K, NR_IO_URING_SETUP, 0, 1));
  f.push(stmt(BPF_RET_K, errno(libc::ENOSYS)));
  if matches!(net, OsNet::ProxyOnly(_)) {
    // sendto / sendmmsg (flags: argument 3) and sendmsg (argument 2) with
    // MSG_FASTOPEN connect a TCP socket without connect(2), past Landlock
    for (nr, flags_arg) in [(NR_SENDTO, 3), (NR_SENDMMSG, 3), (NR_SENDMSG, 2)] {
      f.push(jump(BPF_JEQ_K, nr, 0, 5));
      f.push(stmt(BPF_LD_W_ABS, seccomp_arg(flags_arg)));
      f.push(stmt(BPF_ALU_AND_K, MSG_FASTOPEN));
      f.push(jump(BPF_JEQ_K, 0, 0, 1));
      f.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
      f.push(stmt(BPF_RET_K, errno(libc::EOPNOTSUPP)));
    }
  }
  f.push(jump(BPF_JEQ_K, NR_SOCKET, 1, 0));
  f.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
  // socket(domain, type, protocol): the low 32 bits of each argument
  f.push(stmt(BPF_LD_W_ABS, seccomp_arg(0)));
  f.push(jump(BPF_JEQ_K, AF_NETLINK, 0, 1));
  f.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
  match net {
    OsNet::Denied => {}
    OsNet::Open => {
      f.push(jump(BPF_JEQ_K, AF_INET, 1, 0));
      f.push(jump(BPF_JEQ_K, AF_INET6, 0, 1));
      f.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
    }
    OsNet::ProxyOnly(_) => {
      // IPv4/IPv6 stream sockets with protocol 0 or TCP; everything else
      // jumps to the EACCES below
      f.push(jump(BPF_JEQ_K, AF_INET, 1, 0));
      f.push(jump(BPF_JEQ_K, AF_INET6, 0, 7));
      f.push(stmt(BPF_LD_W_ABS, seccomp_arg(1)));
      f.push(stmt(BPF_ALU_AND_K, SOCK_TYPE_MASK));
      f.push(jump(BPF_JEQ_K, SOCK_STREAM, 0, 4));
      f.push(stmt(BPF_LD_W_ABS, seccomp_arg(2)));
      f.push(jump(BPF_JEQ_K, 0, 1, 0));
      f.push(jump(BPF_JEQ_K, IPPROTO_TCP, 0, 1));
      f.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
    }
  }
  f.push(stmt(BPF_RET_K, errno(libc::EACCES)));
  let mut bytes = Vec::with_capacity(f.len() * 8);
  for ins in f {
    bytes.extend_from_slice(&ins.code.to_ne_bytes());
    bytes.push(ins.jt);
    bytes.push(ins.jf);
    bytes.extend_from_slice(&ins.k.to_ne_bytes());
  }
  bytes
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub fn seccomp_program(_net: OsNet) -> Vec<u8> {
  Vec::new()
}

/// Applies the confinement to `command`'s process before it starts the
/// program. `ruleset` must stay open until the command has been spawned.
pub fn apply_to_command(
  command: &mut tokio::process::Command,
  ruleset: &OwnedFd,
  net: OsNet,
) {
  let ruleset_fd = ruleset.as_raw_fd();
  let program = seccomp_program(net);
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn seccomp_program_is_whole_instructions() {
    for net in [OsNet::Denied, OsNet::Open, OsNet::ProxyOnly(1)] {
      let p = seccomp_program(net);
      assert_eq!(p.len() % 8, 0);
      if cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
        assert!(p.len() / 8 > 8);
      }
    }
  }

  /// Runs a seccomp program on one syscall: the instructions it emits are
  /// loads of `seccomp_data` words, AND, JEQ / JGE and RET.
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  fn run_filter(program: &[u8], nr: u32, args: [u32; 6]) -> u32 {
    let ins = program
      .chunks(8)
      .map(|c| SockFilter {
        code: u16::from_ne_bytes([c[0], c[1]]),
        jt: c[2],
        jf: c[3],
        k: u32::from_ne_bytes([c[4], c[5], c[6], c[7]]),
      })
      .collect::<Vec<_>>();
    let word = |offset: u32| match offset {
      SECCOMP_DATA_NR => nr,
      SECCOMP_DATA_ARCH => AUDIT_ARCH,
      o if o >= SECCOMP_DATA_ARG0 && (o - SECCOMP_DATA_ARG0) % 8 == 0 => {
        args[((o - SECCOMP_DATA_ARG0) / 8) as usize]
      }
      o => panic!("an unexpected load at {o}"),
    };
    let mut acc = 0u32;
    let mut pc = 0usize;
    loop {
      let i = ins[pc];
      pc += 1;
      match i.code {
        BPF_LD_W_ABS => acc = word(i.k),
        BPF_ALU_AND_K => acc &= i.k,
        BPF_JEQ_K | BPF_JGE_K => {
          let taken = if i.code == BPF_JEQ_K {
            acc == i.k
          } else {
            acc >= i.k
          };
          let offset = if taken { i.jt } else { i.jf };
          pc += usize::from(offset);
        }
        BPF_RET_K => return i.k,
        code => panic!("an unexpected instruction {code:#x}"),
      }
    }
  }

  // DENEXT.md item 111: through the proxy, TCP sockets only and no Fast
  // Open sends
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  #[test]
  fn seccomp_program_semantics() {
    let eacces = SECCOMP_RET_ERRNO | libc::EACCES as u32;
    let fastopen = SECCOMP_RET_ERRNO | libc::EOPNOTSUPP as u32;
    let allow = SECCOMP_RET_ALLOW;
    let socket = |net: OsNet, domain: u32, ty: u32, proto: u32| {
      run_filter(
        &seccomp_program(net),
        NR_SOCKET,
        [domain, ty, proto, 0, 0, 0],
      )
    };
    let unix = libc::AF_UNIX as u32;
    let dgram = libc::SOCK_DGRAM as u32;
    let stream_flags =
      SOCK_STREAM | libc::SOCK_CLOEXEC as u32 | libc::SOCK_NONBLOCK as u32;
    let proxy = OsNet::ProxyOnly(1234);
    for net in [OsNet::Denied, OsNet::Open, proxy] {
      assert_eq!(socket(net, unix, SOCK_STREAM, 0), eacces, "{net:?}");
      assert_eq!(socket(net, AF_NETLINK, dgram, 0), allow, "{net:?}");
      // other syscalls pass
      assert_eq!(run_filter(&seccomp_program(net), 0, [0; 6]), allow);
      assert_eq!(
        run_filter(&seccomp_program(net), NR_IO_URING_SETUP, [0; 6]),
        SECCOMP_RET_ERRNO | libc::ENOSYS as u32
      );
    }
    assert_eq!(socket(OsNet::Denied, AF_INET, SOCK_STREAM, 0), eacces);
    assert_eq!(socket(OsNet::Open, AF_INET6, dgram, 0), allow);
    assert_eq!(socket(proxy, AF_INET, SOCK_STREAM, 0), allow);
    assert_eq!(socket(proxy, AF_INET6, stream_flags, IPPROTO_TCP), allow);
    assert_eq!(socket(proxy, AF_INET, dgram, 0), eacces);
    assert_eq!(socket(proxy, AF_INET6, dgram, 17), eacces);
    // SCTP and MPTCP streams: not covered by Landlock's port rules
    assert_eq!(socket(proxy, AF_INET, SOCK_STREAM, 132), eacces);
    assert_eq!(socket(proxy, AF_INET, SOCK_STREAM, 262), eacces);
    assert_eq!(socket(proxy, libc::AF_PACKET as u32, 3, 0), eacces);
    let program = seccomp_program(proxy);
    let msg_fastopen = MSG_FASTOPEN | libc::MSG_NOSIGNAL as u32;
    assert_eq!(run_filter(&program, NR_SENDTO, [3, 0, 10, 0, 0, 0]), allow);
    assert_eq!(
      run_filter(&program, NR_SENDTO, [3, 0, 10, msg_fastopen, 0, 0]),
      fastopen
    );
    assert_eq!(
      run_filter(&program, NR_SENDMMSG, [3, 0, 1, msg_fastopen, 0, 0]),
      fastopen
    );
    assert_eq!(
      run_filter(&program, NR_SENDMSG, [3, 0, msg_fastopen, 0, 0, 0]),
      fastopen
    );
    assert_eq!(run_filter(&program, NR_SENDMSG, [3, 0, 0, 0, 0, 0]), allow);
    // the open network keeps Fast Open
    assert_eq!(
      run_filter(
        &seccomp_program(OsNet::Open),
        NR_SENDTO,
        [3, 0, 10, msg_fastopen, 0, 0]
      ),
      allow
    );
  }
}
