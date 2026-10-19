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

use std::ffi::CString;
use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

// the same numbers on every architecture since Linux 5.13
const SYS_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
const SYS_LANDLOCK_ADD_RULE: libc::c_long = 445;
const SYS_LANDLOCK_RESTRICT_SELF: libc::c_long = 446;
const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

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

/// Builds the Landlock ruleset for a jailed script.
pub fn build_ruleset(
  write_roots: &[PathBuf],
  deny_read: &[PathBuf],
) -> io::Result<OwnedFd> {
  let abi = landlock_abi().map_err(io::Error::other)?;
  let write = write_rights(abi);
  let read = if deny_read.is_empty() {
    0
  } else {
    ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR
  };
  let attr = RulesetAttr {
    handled_access_fs: write | read,
  };
  // SAFETY: a valid attribute of the given size
  let fd = unsafe {
    libc::syscall(
      SYS_LANDLOCK_CREATE_RULESET,
      &attr as *const RulesetAttr,
      std::mem::size_of::<RulesetAttr>(),
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
const BPF_JEQ_K: u16 = 0x15;
const BPF_JGE_K: u16 = 0x35;
const BPF_RET_K: u16 = 0x06;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_DATA_NR: u32 = 0;
const SECCOMP_DATA_ARCH: u32 = 4;
const SECCOMP_DATA_ARG0: u32 = 16;
const AF_NETLINK: u32 = 16;
const AF_INET: u32 = 2;
const AF_INET6: u32 = 10;
const NR_IO_URING_SETUP: u32 = 425;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xC000_003E;
#[cfg(target_arch = "x86_64")]
const NR_SOCKET: u32 = 41;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xC000_00B7;
#[cfg(target_arch = "aarch64")]
const NR_SOCKET: u32 = 198;

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
/// `net`; `io_uring_setup` fails; another architecture's syscalls (x32, the
/// 32-bit compat ABI) kill the process.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn seccomp_program(net: bool) -> Vec<u8> {
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
  f.push(jump(BPF_JEQ_K, NR_SOCKET, 1, 0));
  f.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
  // socket(domain, ...): the low 32 bits of the first argument
  f.push(stmt(BPF_LD_W_ABS, SECCOMP_DATA_ARG0));
  f.push(jump(BPF_JEQ_K, AF_NETLINK, 0, 1));
  f.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
  if net {
    f.push(jump(BPF_JEQ_K, AF_INET, 1, 0));
    f.push(jump(BPF_JEQ_K, AF_INET6, 0, 1));
    f.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
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
pub fn seccomp_program(_net: bool) -> Vec<u8> {
  Vec::new()
}

/// Applies the confinement to `command`'s process before it starts the
/// program. `ruleset` must stay open until the command has been spawned.
pub fn apply_to_command(
  command: &mut tokio::process::Command,
  ruleset: &OwnedFd,
  net: bool,
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
    for net in [false, true] {
      let p = seccomp_program(net);
      assert_eq!(p.len() % 8, 0);
      if cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
        assert!(p.len() / 8 > 8);
      }
    }
  }
}
