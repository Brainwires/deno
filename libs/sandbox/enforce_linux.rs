// Copyright 2018-2026 the Deno authors. MIT license.

//! Kernel enforcement of a process's own grant on Linux (Mode A,
//! `--kernel-sandbox=enforce`): exec, then restrict.
//!
//! The first image resolves the grant and the plan ([`crate::plan`]), loads
//! the module graph, and re-executes itself (`/proc/self/exe`, same argv, so
//! the PID is unchanged) with the plan in a sealed memfd ([`reexec`]). The new
//! image calls [`take_inherited_plan`] and [`apply_self`] first thing in
//! `main`, while it has one thread: `no_new_privs`, the Landlock ruleset
//! (`landlock_restrict_self`), and one complete seccomp program. No second
//! filter stage, no `TSYNC`, and the deno binary needs no execute right.
//!
//! The hand-over can only narrow: whatever plan arrives is applied (the
//! kernel stacks domains, so a plan can never undo one already in force), a
//! plan that is malformed or not sealed is refused (the caller exits), and
//! the CLI later requires the applied plan to equal the one it compiles
//! itself, measured as well ([`status_confined`]), before it skips its own
//! re-exec.

#![allow(
  clippy::disallowed_methods,
  reason = "Landlock rules and the hand-over act on the real file system and /proc"
)]

use std::ffi::CString;
use std::ffi::OsString;
use std::io;
use std::os::fd::AsFd;
use std::os::fd::AsRawFd;
use std::os::fd::BorrowedFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::OnceLock;

use crate::linux::ACCESS_FS_MAKE_BLOCK;
use crate::linux::ACCESS_FS_MAKE_CHAR;
use crate::linux::ACCESS_FS_MAKE_DIR;
use crate::linux::ACCESS_FS_MAKE_FIFO;
use crate::linux::ACCESS_FS_MAKE_REG;
use crate::linux::ACCESS_FS_MAKE_SOCK;
use crate::linux::ACCESS_FS_MAKE_SYM;
use crate::linux::ACCESS_FS_READ_DIR;
use crate::linux::ACCESS_FS_READ_FILE;
use crate::linux::ACCESS_FS_REFER;
use crate::linux::ACCESS_FS_REMOVE_DIR;
use crate::linux::ACCESS_FS_REMOVE_FILE;
use crate::linux::ACCESS_FS_TRUNCATE;
use crate::linux::ACCESS_FS_WRITE_FILE;
use crate::linux::ACCESS_NET_BIND_TCP;
use crate::linux::ACCESS_NET_CONNECT_TCP;
use crate::linux::LANDLOCK_NET_ABI;
use crate::linux::LANDLOCK_RULE_NET_PORT;
use crate::linux::LANDLOCK_RULE_PATH_BENEATH;
use crate::linux::NetPortAttr;
use crate::linux::PathBeneathAttr;
use crate::linux::SYS_LANDLOCK_ADD_RULE;
use crate::linux::SYS_LANDLOCK_CREATE_RULESET;
use crate::linux::SYS_LANDLOCK_RESTRICT_SELF;
use crate::linux::landlock_abi;
use crate::plan::EnforcePlan;
use crate::plan::LinuxPlan;
use crate::plan::MAX_PLAN_BYTES;
use crate::plan::NetPlan;
use crate::plan::PLAN_FD_ENV_VAR;
use crate::plan::Ports;
use crate::plan::SeccompPlan;
use crate::seccomp::InetSockets;
use crate::seccomp::NetlinkSockets;
use crate::seccomp::SeccompSpec;
use crate::seccomp::SockFilter;
use crate::seccomp::SockFprog;
use crate::seccomp::SyscallRule;

const ACCESS_FS_EXECUTE: u64 = 1 << 0;
const ACCESS_FS_IOCTL_DEV: u64 = 1 << 15;
const SCOPE_ABSTRACT_UNIX_SOCKET: u64 = 1 << 0;
const SCOPE_SIGNAL: u64 = 1 << 1;
/// The first ABI with `TRUNCATE`, `IOCTL_DEV` and scopes.
const TRUNCATE_ABI: u32 = 3;
const IOCTL_DEV_ABI: u32 = 5;
const SCOPE_ABI: u32 = 6;
const REFER_ABI: u32 = 2;

/// Rights that apply to a file (not a directory) in a rule.
const FILE_RIGHTS: u64 = ACCESS_FS_EXECUTE
  | ACCESS_FS_WRITE_FILE
  | ACCESS_FS_READ_FILE
  | ACCESS_FS_TRUNCATE
  | ACCESS_FS_IOCTL_DEV;

const READ_RIGHTS: u64 = ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR;

#[repr(C)]
struct RulesetAttr {
  handled_access_fs: u64,
  /// ABI 4+
  handled_access_net: u64,
  /// ABI 6+
  scoped: u64,
}

/// The write rights Landlock knows at `abi` (REFER from 2, TRUNCATE from 3,
/// IOCTL_DEV from 5).
pub fn write_rights(abi: u32) -> u64 {
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
  if abi >= REFER_ABI {
    rights |= ACCESS_FS_REFER;
  }
  if abi >= TRUNCATE_ABI {
    rights |= ACCESS_FS_TRUNCATE;
  }
  if abi >= IOCTL_DEV_ABI {
    rights |= ACCESS_FS_IOCTL_DEV;
  }
  rights
}

/// The ruleset attribute for `plan` at `abi`: (fs, net, scoped), or why the
/// kernel can't hold the plan.
fn handled(plan: &LinuxPlan, abi: u32) -> Result<(u64, u64, u64), String> {
  let mut fs = 0;
  if plan.handled.read {
    fs |= READ_RIGHTS;
  }
  if plan.handled.write {
    fs |= write_rights(abi);
  }
  if plan.handled.exec {
    fs |= ACCESS_FS_EXECUTE;
  }
  let net = match &plan.net {
    NetPlan::Open => 0,
    // seccomp refuses the IP families; Landlock adds "no port" where it can
    NetPlan::Denied if abi >= LANDLOCK_NET_ABI => {
      ACCESS_NET_BIND_TCP | ACCESS_NET_CONNECT_TCP
    }
    NetPlan::Denied => 0,
    NetPlan::Ports { connect, bind } => {
      if abi < LANDLOCK_NET_ABI {
        return Err(format!(
          "TCP port rules need Landlock ABI {LANDLOCK_NET_ABI}; this kernel has {abi}"
        ));
      }
      let mut net = 0;
      if matches!(connect, Ports::List { .. }) {
        net |= ACCESS_NET_CONNECT_TCP;
      }
      if matches!(bind, Ports::List { .. }) {
        net |= ACCESS_NET_BIND_TCP;
      }
      net
    }
  };
  let mut scoped = 0;
  if plan.scope_signal {
    scoped |= SCOPE_SIGNAL;
  }
  if plan.scope_abstract_unix {
    scoped |= SCOPE_ABSTRACT_UNIX_SOCKET;
  }
  if scoped != 0 && abi < SCOPE_ABI {
    return Err(format!(
      "Landlock scopes need ABI {SCOPE_ABI}; this kernel has {abi}"
    ));
  }
  Ok((fs, net, scoped))
}

fn add_path_rule(
  ruleset: &OwnedFd,
  path: &Path,
  rights: u64,
) -> Result<(), String> {
  let c_path = CString::new(path.as_os_str().as_bytes())
    .map_err(|_| format!("{}: a path with a NUL", path.display()))?;
  // SAFETY: a valid C string
  let fd =
    unsafe { libc::open(c_path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
  if fd < 0 {
    // gone or unreachable: nothing to allow (an allow that misses fails
    // closed)
    return Ok(());
  }
  // SAFETY: just opened
  let fd = unsafe { OwnedFd::from_raw_fd(fd) };
  // the object the rule attaches to, not whatever the path names now
  // SAFETY: a zeroed stat is a valid out parameter
  let mut st: libc::stat = unsafe { std::mem::zeroed() };
  // SAFETY: a valid descriptor and out parameter
  if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
    return Ok(());
  }
  let is_dir = st.st_mode & libc::S_IFMT == libc::S_IFDIR;
  let allowed_access = if is_dir { rights } else { rights & FILE_RIGHTS };
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
    return Err(format!(
      "landlock_add_rule {}: {}",
      path.display(),
      io::Error::last_os_error()
    ));
  }
  Ok(())
}

/// A read rule on a regular file only (a module file): anything else at
/// that path gets nothing.
fn add_file_read_rule(ruleset: &OwnedFd, path: &Path) -> Result<(), String> {
  add_rule_on_kind(ruleset, path, libc::S_IFREG, ACCESS_FS_READ_FILE)
}

/// A listing rule on a folder only (a folder the module graph listed to
/// expand a template import): its entries can be listed, no file read.
/// Anything else at that path gets nothing.
fn add_dir_list_rule(ruleset: &OwnedFd, path: &Path) -> Result<(), String> {
  add_rule_on_kind(ruleset, path, libc::S_IFDIR, ACCESS_FS_READ_DIR)
}

/// A rule granting `access` on the object at `path` when it is of `kind`
/// (`S_IFREG`, `S_IFDIR`); nothing when it is gone or of another kind.
fn add_rule_on_kind(
  ruleset: &OwnedFd,
  path: &Path,
  kind: libc::mode_t,
  access: u64,
) -> Result<(), String> {
  let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
    return Ok(());
  };
  // SAFETY: a valid C string
  let fd =
    unsafe { libc::open(c_path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
  if fd < 0 {
    return Ok(());
  }
  // SAFETY: just opened
  let fd = unsafe { OwnedFd::from_raw_fd(fd) };
  // SAFETY: a zeroed stat is a valid out parameter
  let mut st: libc::stat = unsafe { std::mem::zeroed() };
  // SAFETY: a valid descriptor and out parameter
  if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0
    || st.st_mode & libc::S_IFMT != kind
  {
    return Ok(());
  }
  let attr = PathBeneathAttr {
    allowed_access: access,
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
    return Err(format!(
      "landlock_add_rule {}: {}",
      path.display(),
      io::Error::last_os_error()
    ));
  }
  Ok(())
}

/// `struct open_how` for `openat2(2)`.
#[repr(C)]
struct OpenHow {
  flags: u64,
  mode: u64,
  resolve: u64,
}
const SYS_OPENAT2: libc::c_long = 437;
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_NO_SYMLINKS: u64 = 0x04;

/// Opens `path` as an `O_PATH` descriptor with no symlink anywhere in its
/// resolution; the raw result (a descriptor or -1 with errno set).
fn open_no_symlinks(path: &std::ffi::CStr) -> libc::c_long {
  let how = OpenHow {
    flags: (libc::O_PATH | libc::O_CLOEXEC) as u64,
    mode: 0,
    resolve: RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS,
  };
  // SAFETY: a valid path and open_how of the size given
  unsafe {
    libc::syscall(
      SYS_OPENAT2,
      libc::AT_FDCWD,
      path.as_ptr(),
      &how as *const OpenHow,
      std::mem::size_of::<OpenHow>(),
    )
  }
}

/// Whether `openat2` with `errno` means the call itself is unavailable (an
/// older kernel, or an outer seccomp profile refusing it), not the path.
fn openat2_unavailable(errno: Option<i32>) -> bool {
  matches!(errno, Some(libc::ENOSYS) | Some(libc::EPERM))
}

/// Whether `openat2` works here (the read expansion applies its rules with
/// it): `Err` says why not, and the expansion can't be applied.
pub fn openat2_available() -> Result<(), String> {
  let fd = open_no_symlinks(c"/");
  if fd < 0 {
    let err = io::Error::last_os_error();
    if openat2_unavailable(err.raw_os_error()) {
      return Err(format!(
        "openat2 is refused here ({err}): an older kernel, or a container's seccomp profile"
      ));
    }
    return Ok(());
  }
  // SAFETY: just opened
  drop(unsafe { OwnedFd::from_raw_fd(fd as i32) });
  Ok(())
}

/// A read rule on an expanded entry, opened with no symlink anywhere in its
/// resolution (`openat2` with `RESOLVE_NO_SYMLINKS`): a link inside an
/// expanded folder can't carry the rule to its target. An entry that can't
/// be opened so gets nothing (fails closed); `openat2` itself refused is an
/// error (every expanded rule would be missing).
fn add_expanded_rule(
  ruleset: &OwnedFd,
  path: &Path,
  rights: u64,
) -> Result<(), String> {
  let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
    return Ok(());
  };
  let fd = open_no_symlinks(&c_path);
  if fd < 0 {
    let err = io::Error::last_os_error();
    if openat2_unavailable(err.raw_os_error()) {
      return Err(format!(
        "the read expansion can't be applied: openat2 is refused here ({err})"
      ));
    }
    return Ok(());
  }
  // SAFETY: just opened
  let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
  // SAFETY: a zeroed stat is a valid out parameter
  let mut st: libc::stat = unsafe { std::mem::zeroed() };
  // SAFETY: a valid descriptor and out parameter
  if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
    return Ok(());
  }
  let allowed_access = match st.st_mode & libc::S_IFMT {
    libc::S_IFDIR => rights,
    libc::S_IFLNK => return Ok(()),
    _ => rights & FILE_RIGHTS,
  };
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
    return Err(format!(
      "landlock_add_rule {}: {}",
      path.display(),
      io::Error::last_os_error()
    ));
  }
  Ok(())
}

fn add_port_rule(
  ruleset: &OwnedFd,
  access: u64,
  port: u16,
) -> Result<(), String> {
  let attr = NetPortAttr {
    allowed_access: access,
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
    return Err(format!(
      "landlock_add_rule port {port}: {}",
      io::Error::last_os_error()
    ));
  }
  Ok(())
}

/// Builds the Landlock ruleset for `plan` at `abi`, plus a read rule on
/// each regular file in `module_files` and on each `expanded` entry (no
/// symlinks in its resolution), and a listing rule on each folder in
/// `listed_dirs`; `None` when the plan handles nothing Landlock holds.
pub fn build_ruleset(
  plan: &LinuxPlan,
  abi: u32,
  module_files: &[std::path::PathBuf],
  listed_dirs: &[std::path::PathBuf],
  expanded: &[std::path::PathBuf],
  expanded_write: &[std::path::PathBuf],
) -> Result<Option<OwnedFd>, String> {
  let (fs, net, scoped) = handled(plan, abi)?;
  if fs == 0 && net == 0 && scoped == 0 {
    return Ok(None);
  }
  let attr = RulesetAttr {
    handled_access_fs: fs,
    handled_access_net: net,
    scoped,
  };
  // an older kernel refuses a larger attribute with fields it doesn't know
  let size = if abi >= SCOPE_ABI {
    std::mem::size_of::<RulesetAttr>()
  } else if abi >= LANDLOCK_NET_ABI {
    2 * std::mem::size_of::<u64>()
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
    return Err(format!(
      "landlock_create_ruleset: {}",
      io::Error::last_os_error()
    ));
  }
  // SAFETY: just created
  let ruleset = unsafe { OwnedFd::from_raw_fd(fd as i32) };
  for rule in &plan.rules {
    let mut rights = 0;
    if rule.read {
      rights |= READ_RIGHTS;
    }
    if rule.write {
      rights |= write_rights(abi);
    }
    if rule.exec {
      rights |= ACCESS_FS_EXECUTE;
    }
    // a rule may only name handled rights
    add_path_rule(&ruleset, &rule.path, rights & fs)?;
  }
  if fs & ACCESS_FS_READ_FILE != 0 {
    for file in module_files {
      add_file_read_rule(&ruleset, file)?;
    }
  }
  if fs & ACCESS_FS_READ_DIR != 0 {
    for dir in listed_dirs {
      add_dir_list_rule(&ruleset, dir)?;
    }
  }
  if fs & ACCESS_FS_READ_FILE != 0 {
    for entry in expanded {
      add_expanded_rule(&ruleset, entry, READ_RIGHTS & fs)?;
    }
  }
  if fs & ACCESS_FS_WRITE_FILE != 0 {
    for entry in expanded_write {
      add_expanded_rule(&ruleset, entry, write_rights(abi) & fs)?;
    }
  }
  if let NetPlan::Ports { connect, bind } = &plan.net {
    for (access, ports) in [
      (ACCESS_NET_CONNECT_TCP, connect),
      (ACCESS_NET_BIND_TCP, bind),
    ] {
      if let Ports::List { ports } = ports {
        for port in ports {
          add_port_rule(&ruleset, access, *port)?;
        }
      }
    }
  }
  Ok(Some(ruleset))
}

/// Syscall numbers the program names, per architecture.
#[cfg(target_arch = "x86_64")]
mod nr {
  pub const PRCTL: u32 = 157;
  pub const CLONE: u32 = 56;
  pub const UNSHARE: u32 = 272;
  pub const SETNS: u32 = 308;
  pub const EXECVE: u32 = 59;
  pub const EXECVEAT: u32 = 322;
  pub const IO_URING_ENTER: u32 = 426;
  pub const IO_URING_REGISTER: u32 = 427;
  /// The attack-surface list.
  pub const SURFACE: &[u32] = &[
    321, // bpf
    298, // perf_event_open
    323, // userfaultfd
    250, // keyctl
    248, // add_key
    249, // request_key
    101, // ptrace
    310, // process_vm_readv
    311, // process_vm_writev
    312, // kcmp
    165, // mount
    166, // umount2
    155, // pivot_root
    428, // open_tree
    429, // move_mount
    430, // fsopen
    431, // fsconfig
    432, // fsmount
    433, // fspick
    442, // mount_setattr
    246, // kexec_load
    320, // kexec_file_load
    175, // init_module
    313, // finit_module
    176, // delete_module
    304, // open_by_handle_at
    163, // acct
    167, // swapon
    168, // swapoff
  ];
  /// The path forms of metadata writes.
  pub const PATH_METADATA: &[u32] = &[
    90,  // chmod
    268, // fchmodat
    452, // fchmodat2
    92,  // chown
    94,  // lchown
    260, // fchownat
    132, // utime
    235, // utimes
    261, // futimesat
    188, // setxattr
    189, // lsetxattr
    197, // removexattr
    198, // lremovexattr
    463, // setxattrat
    466, // removexattrat
  ];
  pub const UTIMENSAT: u32 = 280;
  pub const TRUNCATE: u32 = 76;
  /// `(nr, flags argument)` of the open calls.
  pub const OPENS: &[(u32, u32)] = &[(2, 1), (257, 2)];
  pub const OPENAT2: u32 = 437;
  /// The signal senders whose first argument is the target process.
  pub const SIGNAL_TO_PID: &[u32] = &[
    62,  // kill
    234, // tgkill
    129, // rt_sigqueueinfo
    297, // rt_tgsigqueueinfo
  ];
  pub const TKILL: u32 = 200;
}

#[cfg(target_arch = "aarch64")]
mod nr {
  pub const PRCTL: u32 = 167;
  pub const CLONE: u32 = 220;
  pub const UNSHARE: u32 = 97;
  pub const SETNS: u32 = 268;
  pub const EXECVE: u32 = 221;
  pub const EXECVEAT: u32 = 281;
  pub const IO_URING_ENTER: u32 = 426;
  pub const IO_URING_REGISTER: u32 = 427;
  pub const SURFACE: &[u32] = &[
    280, // bpf
    241, // perf_event_open
    282, // userfaultfd
    219, // keyctl
    217, // add_key
    218, // request_key
    117, // ptrace
    270, // process_vm_readv
    271, // process_vm_writev
    272, // kcmp
    40,  // mount
    39,  // umount2
    41,  // pivot_root
    428, // open_tree
    429, // move_mount
    430, // fsopen
    431, // fsconfig
    432, // fsmount
    433, // fspick
    442, // mount_setattr
    104, // kexec_load
    294, // kexec_file_load
    105, // init_module
    273, // finit_module
    106, // delete_module
    265, // open_by_handle_at
    89,  // acct
    224, // swapon
    225, // swapoff
  ];
  pub const PATH_METADATA: &[u32] = &[
    53,  // fchmodat
    452, // fchmodat2
    54,  // fchownat
    5,   // setxattr
    6,   // lsetxattr
    14,  // removexattr
    15,  // lremovexattr
    463, // setxattrat
    466, // removexattrat
  ];
  pub const UTIMENSAT: u32 = 88;
  pub const TRUNCATE: u32 = 45;
  pub const OPENS: &[(u32, u32)] = &[(56, 2)];
  pub const OPENAT2: u32 = 437;
  pub const SIGNAL_TO_PID: &[u32] = &[
    129, // kill
    131, // tgkill
    138, // rt_sigqueueinfo
    240, // rt_tgsigqueueinfo
  ];
  pub const TKILL: u32 = 130;
}

/// `pidfd_send_signal` (the same number on both architectures).
const NR_PIDFD_SEND_SIGNAL: u32 = 424;

/// `CLONE_NEW*` flags as `clone(2)` takes them (its low byte is the exit
/// signal, so `CLONE_NEWTIME` can't be passed there).
pub const CLONE_NAMESPACE_FLAGS: u32 = 0x0002_0000 // NEWNS
  | 0x0200_0000 // NEWCGROUP
  | 0x0400_0000 // NEWUTS
  | 0x0800_0000 // NEWIPC
  | 0x1000_0000 // NEWUSER
  | 0x2000_0000 // NEWPID
  | 0x4000_0000; // NEWNET
/// `unshare(2)` also takes `CLONE_NEWTIME`.
pub const UNSHARE_NAMESPACE_FLAGS: u32 = CLONE_NAMESPACE_FLAGS | 0x80;

/// The namespaces Chromium's namespace sandbox makes for its zygote and
/// helper processes: a user, a PID and a network namespace.
pub const ENGINE_NAMESPACE_FLAGS: u32 = 0x1000_0000 // NEWUSER
  | 0x2000_0000 // NEWPID
  | 0x4000_0000; // NEWNET

/// What a packaged desktop app's process needs that a headless program's
/// plan refuses (DENEXT.md item 489): its web engine lives in the confined
/// process and starts its own helper processes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DesktopVariant {
  /// The engine is Chromium (CEF): `clone` / `unshare` may make the
  /// namespaces of [`ENGINE_NAMESPACE_FLAGS`], which its namespace sandbox
  /// puts the renderer, GPU and utility processes in. Every other namespace
  /// flag, `setns` and the rest of the attack-surface list stay refused. A
  /// process in such a namespace is still inside this Landlock domain and
  /// under this seccomp program (both are inherited, whatever the
  /// namespace), and a new network namespace holds no interface of the
  /// host's.
  pub engine_namespaces: bool,
}

/// The `prctl(2)` option the enforce seccomp program answers with
/// [`MARKER_ERRNO`]: how a process tells that a kernel sandbox of this
/// runtime holds it when no carrier was handed down
/// (`crate::inherit::kernel_reports_marked`). No kernel defines it, so
/// unconfined (or under anyone else's filter) it fails with `EINVAL`; the
/// rule grants nothing.
pub const MARKER_PRCTL_OPTION: u32 = 0x646e_786b;

/// The errno the marker rule answers [`MARKER_PRCTL_OPTION`] with (one
/// `prctl(2)` never returns).
pub const MARKER_ERRNO: i32 = libc::ENOTRECOVERABLE;

/// The seccomp program for a plan: one complete filter, `EPERM` / `ENOSYS`
/// answers (catchable errors in JS), never a kill but for another
/// architecture's syscalls (the frame).
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn seccomp_spec(seccomp: &SeccompPlan, net: &NetPlan) -> SeccompSpec {
  seccomp_spec_variant(seccomp, net, DesktopVariant::default())
}

/// [`seccomp_spec`] for a packaged desktop app's process (`variant`).
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn seccomp_spec_variant(
  seccomp: &SeccompPlan,
  net: &NetPlan,
  variant: DesktopVariant,
) -> SeccompSpec {
  use crate::seccomp::MSG_FASTOPEN;
  use crate::seccomp::NR_IO_URING_SETUP;
  use crate::seccomp::NR_IOCTL;
  use crate::seccomp::NR_LISTEN;
  use crate::seccomp::NR_SENDMMSG;
  use crate::seccomp::NR_SENDMSG;
  use crate::seccomp::NR_SENDTO;
  use crate::seccomp::TIOCLINUX;
  use crate::seccomp::TIOCSTI;
  const NR_CLONE3: u32 = 435;
  let eperm = libc::EPERM;
  let mut spec = SeccompSpec::new()
    // the marker: this runtime's sandbox holds the process
    .rule(SyscallRule::DenyArgIn {
      nr: nr::PRCTL,
      arg: 0,
      values: vec![MARKER_PRCTL_OPTION],
      errno: MARKER_ERRNO,
    })
    // the ioctl request is argument 1: typing into a terminal
    .rule(SyscallRule::DenyArgIn {
      nr: NR_IOCTL,
      arg: 1,
      values: vec![TIOCSTI, TIOCLINUX],
      errno: eperm,
    });
  if seccomp.refuse_io_uring {
    // ENOSYS: callers fall back to plain syscalls
    for nr in [NR_IO_URING_SETUP, nr::IO_URING_ENTER, nr::IO_URING_REGISTER] {
      spec = spec.rule(SyscallRule::Deny {
        nr,
        errno: libc::ENOSYS,
      });
    }
  }
  if seccomp.clone3_enosys {
    spec = spec.rule(SyscallRule::Deny {
      nr: NR_CLONE3,
      errno: libc::ENOSYS,
    });
  }
  if seccomp.attack_surface {
    // the flags are argument 0 of both; anything else passes. A desktop
    // app on Chromium keeps the namespaces its engine's sandbox makes
    let engine = if variant.engine_namespaces {
      ENGINE_NAMESPACE_FLAGS
    } else {
      0
    };
    spec = spec
      .rule(SyscallRule::DenyArgBits {
        nr: nr::CLONE,
        arg: 0,
        mask: CLONE_NAMESPACE_FLAGS & !engine,
        errno: eperm,
      })
      .rule(SyscallRule::DenyArgBits {
        nr: nr::UNSHARE,
        arg: 0,
        mask: UNSHARE_NAMESPACE_FLAGS & !engine,
        errno: eperm,
      })
      .rule(SyscallRule::Deny {
        nr: nr::SETNS,
        errno: eperm,
      });
    for nr in nr::SURFACE {
      spec = spec.rule(SyscallRule::Deny {
        nr: *nr,
        errno: eperm,
      });
    }
  }
  if seccomp.refuse_exec {
    for nr in [nr::EXECVE, nr::EXECVEAT] {
      spec = spec.rule(SyscallRule::Deny { nr, errno: eperm });
    }
  }
  if seccomp.refuse_connect {
    spec = spec.rule(SyscallRule::Deny {
      nr: crate::net_gate::NR_CONNECT,
      errno: libc::EACCES,
    });
  }
  if seccomp.signal_self {
    // this process only (the PID is the first image's: the re-exec keeps
    // it); a thread id can't be told apart from another process's, and a
    // pidfd only names a child, which nothing here may start
    let own = std::process::id();
    for nr in nr::SIGNAL_TO_PID {
      spec = spec.rule(SyscallRule::DenyArgNotEq {
        nr: *nr,
        arg: 0,
        value: own,
        errno: eperm,
      });
    }
    spec = spec
      .rule(SyscallRule::Deny {
        nr: nr::TKILL,
        errno: eperm,
      })
      .rule(SyscallRule::Deny {
        nr: NR_PIDFD_SEND_SIGNAL,
        errno: libc::ENOSYS,
      });
  }
  if seccomp.refuse_listen {
    // listen(2) on an unbound socket binds an ephemeral port by itself,
    // past Landlock's BIND_TCP (the jail's item 419 rule)
    spec = spec.rule(SyscallRule::Deny {
      nr: NR_LISTEN,
      errno: eperm,
    });
  }
  if seccomp.refuse_path_metadata {
    for nr in nr::PATH_METADATA {
      spec = spec.rule(SyscallRule::Deny {
        nr: *nr,
        errno: eperm,
      });
    }
    // utimensat(fd, NULL, ...) is futimens: the descriptor form passes
    spec = spec.rule(SyscallRule::DenyArgNonZero {
      nr: nr::UTIMENSAT,
      arg: 1,
      errno: eperm,
    });
  }
  if seccomp.refuse_path_truncate {
    spec = spec.rule(SyscallRule::Deny {
      nr: nr::TRUNCATE,
      errno: eperm,
    });
    // O_ACCMODE | O_TRUNC == O_TRUNC: a read-only open that truncates
    for (nr, arg) in nr::OPENS {
      spec = spec.rule(SyscallRule::DenyArgMasked {
        nr: *nr,
        arg: *arg,
        mask: 0o3 | libc::O_TRUNC as u32,
        value: libc::O_TRUNC as u32,
        errno: eperm,
      });
    }
    spec = spec.rule(SyscallRule::Deny {
      nr: nr::OPENAT2,
      errno: libc::ENOSYS,
    });
  }
  let inet = match net {
    NetPlan::Denied => InetSockets::Denied,
    NetPlan::Open => InetSockets::Any,
    NetPlan::Ports { .. } => {
      // TCP Fast Open connects without connect(2)
      for (nr, arg) in [(NR_SENDTO, 3), (NR_SENDMMSG, 3), (NR_SENDMSG, 2)] {
        spec = spec.rule(SyscallRule::DenyArgBits {
          nr,
          arg,
          mask: MSG_FASTOPEN,
          errno: libc::EOPNOTSUPP,
        });
      }
      InetSockets::TcpStream
    }
  };
  spec
    .inet_sockets(inet)
    .netlink_sockets(if seccomp.netlink_route {
      NetlinkSockets::RouteOnly
    } else {
      NetlinkSockets::Denied
    })
    .unix_sockets(seccomp.unix_sockets)
    .vsock_sockets(seccomp.vsock)
    .socket_errno(eperm)
}

/// Whether this process can install a seccomp filter (an outer profile, a
/// container's included, may refuse it), or why not: tried in a forked
/// child, which installs an allow-everything program and exits with the
/// errno.
pub fn seccomp_available() -> Result<(), String> {
  static AVAILABLE: OnceLock<Result<(), String>> = OnceLock::new();
  AVAILABLE
    .get_or_init(|| {
      // RET ALLOW: a program that changes nothing
      let program = [SockFilter {
        code: crate::seccomp::BPF_RET_K,
        jt: 0,
        jf: 0,
        k: crate::seccomp::SECCOMP_RET_ALLOW,
      }];
      let prog = SockFprog {
        len: 1,
        filter: program.as_ptr(),
      };
      // SAFETY: fork; the child runs only prctl and _exit
      let pid = unsafe { libc::fork() };
      if pid < 0 {
        return Err(format!("fork failed: {}", io::Error::last_os_error()));
      }
      if pid == 0 {
        // SAFETY: plain prctl calls on the child's only thread, then _exit
        unsafe {
          let code = if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
            || libc::prctl(
              libc::PR_SET_SECCOMP,
              libc::SECCOMP_MODE_FILTER,
              &prog as *const SockFprog,
              0,
              0,
            ) != 0
          {
            *libc::__errno_location()
          } else {
            0
          };
          libc::_exit(code);
        }
      }
      let mut status = 0;
      // SAFETY: waiting for the child just forked
      unsafe { libc::waitpid(pid, &mut status, 0) };
      if !libc::WIFEXITED(status) {
        return Err("the seccomp probe died".into());
      }
      match libc::WEXITSTATUS(status) {
        0 => Ok(()),
        errno => Err(format!(
          "a seccomp filter can't be installed here: {} (a container's or an outer sandbox's seccomp profile)",
          io::Error::from_raw_os_error(errno)
        )),
      }
    })
    .clone()
}

/// The number of threads this process has.
/// The threads of this process, from `/proc/self/task`; `None` when it
/// can't be listed (an outer kernel sandbox's `/proc/self` rule names its
/// own process: Landlock resolves the link when the rule is made).
fn thread_count() -> Option<usize> {
  std::fs::read_dir("/proc/self/task").ok().map(|d| d.count())
}

/// The plan applied to this process at startup, if any.
static APPLIED: OnceLock<EnforcePlan> = OnceLock::new();

pub fn applied_plan() -> Option<&'static EnforcePlan> {
  APPLIED.get()
}

/// Binds the plan's listeners while this process can still make sockets,
/// and records each for the operation that will listen there
/// (`deno_net_route`). One that can't be bound (the address is in use, the
/// family is missing) is recorded with its error, which that operation
/// then reports; the sandbox applies either way.
pub fn bind_prebound(addrs: &[std::net::SocketAddr]) {
  for addr in addrs {
    // close-on-exec, SO_REUSEADDR and listening, as std binds a listener
    match std::net::TcpListener::bind(addr) {
      Ok(listener) => deno_net_route::add_prebound_tcp(*addr, listener.into()),
      Err(err) => deno_net_route::add_prebound_failure(*addr, &err),
    }
  }
}

/// Applies `plan` to this process: `no_new_privs`, the Landlock ruleset,
/// then the seccomp program, and checks the probe. Must run while the
/// process has one thread (the top of `main` in the re-executed image):
/// without `TSYNC` other threads would stay unconfined, so more than one
/// thread is an error, not a partial confinement.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn apply_self(plan: &EnforcePlan) -> Result<(), String> {
  apply_self_variant(plan, DesktopVariant::default())
}

/// Whether this process has exactly one thread (what [`apply_self`]
/// needs); `None` when its threads can't be counted.
pub fn single_threaded() -> Option<bool> {
  thread_count().map(|threads| threads == 1)
}

/// [`apply_self`] for a packaged desktop app's process (`variant`), called
/// by the app's host before it starts its web engine or any thread.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn apply_self_variant(
  plan: &EnforcePlan,
  variant: DesktopVariant,
) -> Result<(), String> {
  let Some(linux) = &plan.linux else {
    return Err("the plan has no Linux part".to_string());
  };
  // the expanded entries must belong to the plan's own expansions
  plan.check_expanded()?;
  // the channel to a supervisor that answers exec calls, if one was left:
  // taken whether or not the plan uses it (one nobody compiled is closed)
  let gate_channel = crate::exec_gate::take_channel()?;
  match thread_count() {
    Some(1) => {}
    Some(threads) => {
      return Err(format!(
        "the process has {threads} threads; the sandbox must apply while it has one"
      ));
    }
    // inside an outer kernel sandbox that hides /proc/self (a nested
    // enforce): the count can't be measured; the outer domain holds every
    // thread whatever this layer adds, and nothing starts a thread before
    // this point
    None if status_confined() => {}
    None => {
      return Err(
        "the process's threads can't be counted (/proc/self/task)".to_string(),
      );
    }
  }
  let (fs, net, scoped) = (
    linux.handled.read || linux.handled.write || linux.handled.exec,
    !matches!(linux.net, NetPlan::Open | NetPlan::Denied),
    linux.scope_signal || linux.scope_abstract_unix,
  );
  let ruleset = match landlock_abi() {
    Ok(abi) => build_ruleset(
      linux,
      abi,
      &plan.module_files,
      &plan.listed_dirs,
      &plan.expanded,
      &plan.expanded_write,
    )?,
    Err(err) if fs || net || scoped => return Err(err),
    Err(_) => None,
  };
  let program =
    seccomp_spec_variant(&linux.seccomp, &linux.net, variant).program();
  bind_prebound(&linux.prebound);
  // SAFETY: plain prctl/syscalls on this (only) thread
  unsafe {
    if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
      return Err(format!("no_new_privs: {}", io::Error::last_os_error()));
    }
    // a plan whose exec calls a supervising process answers applies only
    // with that process listening: the filter that hands them over goes in
    // first, its listener to the supervisor
    if linux.exec_gate.is_some() {
      let Some(channel) = gate_channel else {
        return Err(
          "the plan hands exec calls to a supervising process, and this process has no channel to one"
            .to_string(),
        );
      };
      crate::exec_gate::install(channel)
        .map_err(|err| format!("the exec filter: {err}"))?;
    }
    if let Some(ruleset) = &ruleset
      && libc::syscall(SYS_LANDLOCK_RESTRICT_SELF, ruleset.as_raw_fd(), 0u32)
        != 0
    {
      return Err(format!(
        "landlock_restrict_self: {}",
        io::Error::last_os_error()
      ));
    }
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
      return Err(format!("seccomp: {}", io::Error::last_os_error()));
    }
  }
  drop(ruleset);
  if let Some(probe) = &plan.probe
    && std::fs::File::open(probe).is_ok()
  {
    return Err(format!(
      "{} still opens with the sandbox applied: it doesn't hold here",
      probe.display()
    ));
  }
  let _ = APPLIED.set(plan.clone());
  Ok(())
}

/// The steps of [`trial_engine_namespaces`]'s child, as its exit status.
mod trial {
  pub const APPLY: i32 = 10;
  pub const CLONE: i32 = 11;
  pub const SETGROUPS: i32 = 12;
  pub const UID_MAP: i32 = 13;
  pub const GID_MAP: i32 = 14;
  pub const NESTED: i32 = 15;
  pub const CHROOT: i32 = 16;
  pub const DIED: i32 = 17;
}

/// Writes `text` to `path` with plain syscalls (after a raw `clone`: no
/// allocation, nothing but system calls).
fn write_small(path: &std::ffi::CStr, text: &[u8]) -> bool {
  // SAFETY: a NUL-terminated path and a buffer of the given length
  unsafe {
    let fd = libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
    if fd < 0 {
      return false;
    }
    let n = libc::write(fd, text.as_ptr() as *const libc::c_void, text.len());
    libc::close(fd);
    n == text.len() as isize
  }
}

/// What Chromium's namespace sandbox does first, done by a process this
/// plan confines: a child in new user, PID and network namespaces
/// (`clone`, as Chromium's launcher calls it) that denies `setgroups`,
/// maps its own ids, makes a nested user namespace and changes its root.
/// Returns the step that failed.
fn engine_namespace_steps(uid_map: &[u8], gid_map: &[u8]) -> i32 {
  // SAFETY: raw clone with no stack: the child is a copy of this (single)
  // thread, runs only system calls and exits
  let pid = unsafe {
    libc::syscall(
      libc::SYS_clone,
      (ENGINE_NAMESPACE_FLAGS as libc::c_ulong)
        | libc::SIGCHLD as libc::c_ulong,
      0usize,
      0usize,
      0usize,
      0usize,
    )
  };
  if pid < 0 {
    return trial::CLONE;
  }
  if pid == 0 {
    // SAFETY: plain system calls with constant arguments, in the child
    let nested = || unsafe { libc::unshare(libc::CLONE_NEWUSER) == 0 };
    // SAFETY: as above; a NUL-terminated constant path
    let rooted = || unsafe { libc::chroot(c"/proc/self/fdinfo".as_ptr()) == 0 };
    let code = if !write_small(c"/proc/self/setgroups", b"deny") {
      trial::SETGROUPS
    } else if !write_small(c"/proc/self/uid_map", uid_map) {
      trial::UID_MAP
    } else if !write_small(c"/proc/self/gid_map", gid_map) {
      trial::GID_MAP
    } else if !nested() {
      trial::NESTED
    } else if !rooted() {
      trial::CHROOT
    } else {
      0
    };
    // SAFETY: the child ends here
    unsafe { libc::_exit(code) };
  }
  let mut status = 0;
  // SAFETY: waiting for the child just cloned
  unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
  if libc::WIFEXITED(status) {
    libc::WEXITSTATUS(status)
  } else {
    trial::DIED
  }
}

/// Whether Chromium's namespace sandbox still starts inside `plan`
/// (`variant`), measured before this process commits to the plan: a forked
/// child applies the same plan to itself and takes the engine's first
/// steps ([`engine_namespace_steps`]). A desktop app on Chromium is
/// confined only when this passes: under `no_new_privs` Chromium has no
/// other sandbox for web content (its setuid helper can't gain root), so a
/// plan its namespace sandbox can't start in would leave web content less
/// contained than without the kernel sandbox. Must run while the process
/// has one thread.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn trial_engine_namespaces(
  plan: &EnforcePlan,
  variant: DesktopVariant,
) -> Result<(), String> {
  if single_threaded() != Some(true) {
    return Err("the process doesn't have exactly one thread".to_string());
  }
  // SAFETY: getuid / getgid have no failure mode
  let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
  let uid_map = format!("{uid} {uid} 1\n").into_bytes();
  let gid_map = format!("{gid} {gid} 1\n").into_bytes();
  // SAFETY: fork of a single-threaded process; the child applies the plan
  // to itself, probes and exits
  let pid = unsafe { libc::fork() };
  if pid < 0 {
    return Err(format!("fork failed: {}", io::Error::last_os_error()));
  }
  if pid == 0 {
    let code = match apply_self_variant(plan, variant) {
      Ok(()) => engine_namespace_steps(&uid_map, &gid_map),
      Err(_) => trial::APPLY,
    };
    // SAFETY: the child ends here
    unsafe { libc::_exit(code) };
  }
  let mut status = 0;
  // SAFETY: waiting for the child just forked
  unsafe { libc::waitpid(pid, &mut status, 0) };
  let code = if libc::WIFEXITED(status) {
    libc::WEXITSTATUS(status)
  } else {
    trial::DIED
  };
  let step = match code {
    0 => return Ok(()),
    trial::APPLY => "the plan could not be applied",
    trial::CLONE => {
      "a process in new user, PID and network namespaces could not be made"
    }
    trial::SETGROUPS => "/proc/self/setgroups could not be written",
    trial::UID_MAP => "the user id map could not be written",
    trial::GID_MAP => "the group id map could not be written",
    trial::NESTED => "a nested user namespace could not be made",
    trial::CHROOT => "the root could not be changed",
    _ => "the trial process died",
  };
  Err(format!(
    "Chromium's namespace sandbox does not start inside the kernel sandbox here: {step}"
  ))
}

/// Whether the kernel reports `no_new_privs` and a seccomp filter for this
/// process (the `NoNewPrivs` / `Seccomp` fields of `/proc/<pid>/status`):
/// the measured half of "this process is confined" (the other half is the
/// plan [`apply_self`] recorded, and its probe). Asked with `prctl`, which
/// needs no file: a child of a confined process can't read its own
/// `/proc/self` (the parent's rule names the parent's entry).
pub fn status_confined() -> bool {
  // SAFETY: plain queries with no pointers
  unsafe {
    libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) == 1
      && libc::prctl(libc::PR_GET_SECCOMP, 0, 0, 0, 0) == 2
  }
}

const REQUIRED_SEALS: libc::c_int = libc::F_SEAL_SEAL
  | libc::F_SEAL_SHRINK
  | libc::F_SEAL_GROW
  | libc::F_SEAL_WRITE;

/// Writes `bytes` to a new memfd and seals it (no more writes, no resize, no
/// more seals). The descriptor is not close-on-exec: it is the one the new
/// image inherits.
pub fn seal_plan(bytes: &[u8]) -> io::Result<OwnedFd> {
  seal_bytes(c"deno-kernel-plan", bytes)
}

/// Writes `bytes` to a new memfd named `name` and seals it, as
/// [`seal_plan`] does (also the carrier of the inherited policy,
/// `crate::inherit`). Not close-on-exec.
pub fn seal_bytes(name: &std::ffi::CStr, bytes: &[u8]) -> io::Result<OwnedFd> {
  // SAFETY: a valid name; MFD_ALLOW_SEALING, no MFD_CLOEXEC on purpose
  let fd =
    unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_ALLOW_SEALING) };
  if fd < 0 {
    return Err(io::Error::last_os_error());
  }
  // SAFETY: just created
  let fd = unsafe { OwnedFd::from_raw_fd(fd) };
  let mut written = 0;
  while written < bytes.len() {
    // SAFETY: a valid descriptor and buffer
    let n = unsafe {
      libc::write(
        fd.as_raw_fd(),
        bytes[written..].as_ptr() as *const libc::c_void,
        bytes.len() - written,
      )
    };
    if n < 0 {
      let err = io::Error::last_os_error();
      if err.kind() == io::ErrorKind::Interrupted {
        continue;
      }
      return Err(err);
    }
    written += n as usize;
  }
  // SAFETY: a valid descriptor
  if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_ADD_SEALS, REQUIRED_SEALS) }
    != 0
  {
    return Err(io::Error::last_os_error());
  }
  Ok(fd)
}

/// Reads a plan from an inherited sealed memfd and closes it. Anything
/// short of a sealed memfd of a sane size is an error.
pub fn read_sealed(fd: OwnedFd) -> Result<Vec<u8>, String> {
  let bytes = read_sealed_fd(fd.as_fd())?;
  drop(fd);
  Ok(bytes)
}

/// Reads a sealed memfd without closing it ([`read_sealed`]'s checks: a
/// memfd carrying every seal, of a sane size).
pub fn read_sealed_fd(fd: BorrowedFd<'_>) -> Result<Vec<u8>, String> {
  // SAFETY: a valid descriptor
  let seals = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GET_SEALS) };
  if seals < 0 {
    return Err(format!(
      "the plan descriptor is not a memfd: {}",
      io::Error::last_os_error()
    ));
  }
  if seals & REQUIRED_SEALS != REQUIRED_SEALS {
    return Err(format!("the plan memfd is not sealed (seals {seals:#x})"));
  }
  // SAFETY: a zeroed stat is a valid out parameter
  let mut st: libc::stat = unsafe { std::mem::zeroed() };
  // SAFETY: a valid descriptor and out parameter
  if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
    return Err(format!("fstat: {}", io::Error::last_os_error()));
  }
  let size = st.st_size as usize;
  if size == 0 || size > MAX_PLAN_BYTES {
    return Err(format!("the plan memfd has {size} bytes"));
  }
  let mut buf = vec![0u8; size];
  let mut read = 0;
  while read < size {
    // SAFETY: a valid descriptor and buffer
    let n = unsafe {
      libc::pread(
        fd.as_raw_fd(),
        buf[read..].as_mut_ptr() as *mut libc::c_void,
        size - read,
        read as libc::off_t,
      )
    };
    if n < 0 {
      let err = io::Error::last_os_error();
      if err.kind() == io::ErrorKind::Interrupted {
        continue;
      }
      return Err(format!("reading the plan: {err}"));
    }
    if n == 0 {
      return Err("the plan memfd ended early".to_string());
    }
    read += n as usize;
  }
  Ok(buf)
}

/// The plan handed over by the image before this one, if the variable is
/// set: read from its sealed memfd (closed afterwards) and parsed, the
/// variable removed. Call first thing in `main`, while the process has one
/// thread. `Err`: the caller refuses to start (the variable without a valid
/// sealed plan is never ignored).
pub fn take_inherited_plan() -> Result<Option<EnforcePlan>, String> {
  let Some(value) = std::env::var_os(PLAN_FD_ENV_VAR) else {
    return Ok(None);
  };
  // SAFETY: called while the process has one thread
  unsafe { std::env::remove_var(PLAN_FD_ENV_VAR) };
  let fd: i32 = value
    .to_str()
    .and_then(|v| v.parse().ok())
    .filter(|fd| *fd > 2)
    .ok_or_else(|| {
      format!("{PLAN_FD_ENV_VAR}={value:?} is not a descriptor")
    })?;
  // SAFETY: a descriptor number checked to be open below
  if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
    return Err(format!("{PLAN_FD_ENV_VAR}: descriptor {fd} is not open"));
  }
  // SAFETY: an open descriptor this process now owns
  let fd = unsafe { OwnedFd::from_raw_fd(fd) };
  let bytes = read_sealed(fd)?;
  EnforcePlan::from_bytes(&bytes).map(Some)
}

/// The descriptors open in this process (fd 3 and up), from
/// `/proc/self/fd`; `None` when it can't be listed (inside an outer kernel
/// sandbox whose `/proc/self` rule names its own process: a nested run).
fn open_fds() -> Option<Vec<i32>> {
  std::fs::read_dir("/proc/self/fd").ok().map(|d| {
    d.flatten()
      .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
      .filter(|fd| *fd > 2)
      .collect()
  })
}

/// Every descriptor number from 3 below the descriptor limit (at most
/// 65536) that is open, probed one by one.
fn probed_fds() -> Vec<i32> {
  // SAFETY: a zeroed rlimit is a valid out parameter
  let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
  // SAFETY: a valid out parameter
  let ok = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0;
  let max = if ok {
    limit.rlim_cur.min(65_536) as i32
  } else {
    1024
  };
  (3..max)
    .filter(|&fd| {
      // SAFETY: flags of a descriptor number; one that isn't open just fails
      unsafe { libc::fcntl(fd, libc::F_GETFD) >= 0 }
    })
    .collect()
}

/// Every descriptor from 3 up close-on-exec in one call (`close_range`,
/// Linux 5.11; on an older kernel nothing happens and the caller's probe
/// covers what lies below the limit).
fn mark_all_close_on_exec() {
  const CLOSE_RANGE_CLOEXEC: libc::c_uint = 1 << 2;
  // SAFETY: close_range with CLOSE_RANGE_CLOEXEC closes nothing; it only
  // sets a flag
  unsafe {
    libc::syscall(
      libc::SYS_close_range,
      3 as libc::c_uint,
      libc::c_uint::MAX,
      CLOSE_RANGE_CLOEXEC,
    );
  }
}

/// Re-executes this program (`/proc/self/exe`, the same argv, so the PID is
/// unchanged) with `plan` in a sealed memfd and `env` as its environment.
/// Every descriptor but stdio, those `keep` names (the ones the process
/// inherited) and the memfd is made close-on-exec first. Returns only on
/// failure.
pub fn reexec(
  plan: &EnforcePlan,
  env: Vec<(OsString, OsString)>,
  keep: impl Fn(i32) -> bool,
) -> io::Error {
  let memfd = match plan.to_bytes().and_then(|bytes| seal_plan(&bytes)) {
    Ok(fd) => fd,
    Err(err) => return err,
  };
  let memfd_raw = memfd.as_raw_fd();
  // where /proc/self/fd can't be listed (a nested run) the listing never
  // fails open: every descriptor is made close-on-exec, then the open ones
  // below the limit are probed and those to keep are opened again for the
  // exec (a descriptor an ancestor's broker dial left must not survive it)
  let (fds, restore) = match open_fds() {
    Some(fds) => (fds, false),
    None => {
      mark_all_close_on_exec();
      (probed_fds(), true)
    }
  };
  for fd in fds {
    let kept = fd == memfd_raw || keep(fd);
    if kept && !restore {
      continue;
    }
    // SAFETY: setting a flag on a descriptor this process holds; a stale
    // number (the directory listing's own) just fails
    unsafe {
      let flags = libc::fcntl(fd, libc::F_GETFD);
      if flags >= 0 {
        let flags = if kept {
          flags & !libc::FD_CLOEXEC
        } else {
          flags | libc::FD_CLOEXEC
        };
        libc::fcntl(fd, libc::F_SETFD, flags);
      }
    }
  }
  let to_c = |s: &[u8]| CString::new(s).ok();
  let argv: Option<Vec<CString>> =
    std::env::args_os().map(|a| to_c(a.as_bytes())).collect();
  let mut envp: Vec<CString> = env
    .iter()
    .filter(|(k, _)| k != PLAN_FD_ENV_VAR)
    .filter_map(|(k, v)| {
      let mut kv = k.as_bytes().to_vec();
      kv.push(b'=');
      kv.extend_from_slice(v.as_bytes());
      to_c(&kv)
    })
    .collect();
  envp.push(
    CString::new(format!("{PLAN_FD_ENV_VAR}={memfd_raw}"))
      .expect("no NUL in a number"),
  );
  let Some(argv) = argv else {
    return io::Error::other("an argument holds a NUL byte");
  };
  let mut argv_p: Vec<*const libc::c_char> =
    argv.iter().map(|a| a.as_ptr()).collect();
  argv_p.push(std::ptr::null());
  let mut envp_p: Vec<*const libc::c_char> =
    envp.iter().map(|e| e.as_ptr()).collect();
  envp_p.push(std::ptr::null());
  // SAFETY: NUL-terminated arrays of valid C strings that outlive the call
  unsafe {
    libc::execve(c"/proc/self/exe".as_ptr(), argv_p.as_ptr(), envp_p.as_ptr());
  }
  let err = io::Error::last_os_error();
  drop(memfd);
  err
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::plan::FsRule;
  use crate::plan::Handled;
  use crate::plan::PLAN_FORMAT;
  use crate::seccomp::AF_INET;
  use crate::seccomp::AF_NETLINK;
  use crate::seccomp::AF_UNIX;
  use crate::seccomp::NETLINK_ROUTE;
  use crate::seccomp::NR_LISTEN;
  use crate::seccomp::NR_SOCKET;
  use crate::seccomp::SECCOMP_RET_ALLOW;
  use crate::seccomp::SOCK_STREAM;
  use crate::seccomp::run_filter;
  use crate::seccomp::run_filter64;

  fn eno(e: i32) -> u32 {
    crate::seccomp::SECCOMP_RET_ERRNO | e as u32
  }

  fn linux(seccomp: SeccompPlan, net: NetPlan) -> LinuxPlan {
    LinuxPlan {
      handled: Handled {
        read: true,
        write: true,
        exec: true,
      },
      rules: vec![FsRule {
        path: "/tmp".into(),
        read: true,
        write: true,
        exec: false,
      }],
      expand: Vec::new(),
      expand_write: Vec::new(),
      net,
      prebound: Vec::new(),
      scope_signal: false,
      scope_abstract_unix: false,
      seccomp,
      exec_gate: None,
    }
  }

  fn full() -> SeccompPlan {
    SeccompPlan {
      attack_surface: true,
      clone3_enosys: true,
      refuse_exec: true,
      refuse_listen: true,
      refuse_connect: false,
      refuse_io_uring: true,
      refuse_path_metadata: true,
      refuse_path_truncate: true,
      signal_self: true,
      unix_sockets: false,
      netlink_route: false,
      vsock: false,
    }
  }

  #[test]
  fn plan_round_trip_and_refusals() {
    let plan = EnforcePlan {
      format: PLAN_FORMAT,
      linux: Some(linux(full(), NetPlan::Denied)),
      macos: None,
      windows: None,
      env_keep: Some(vec!["PATH".into(), "LC_*".into()]),
      probe: Some("/".into()),
      module_files: vec!["/w/main.ts".into()],
      expanded: Vec::new(),
      config_env: vec![("JSR_URL".into(), "https://jsr.test/".into())],
      cache_overlay: Some("/d/sandboxed/0123456789abcdef".into()),
      broker: None,
      expanded_write: Vec::new(),
      entries: Vec::new(),
      member_configs: Vec::new(),
      listed_dirs: vec!["/w/lib".into()],
    };
    assert_eq!(plan.without_measured_lists().module_files.len(), 0);
    assert_eq!(plan.without_measured_lists().listed_dirs.len(), 0);
    let bytes = plan.to_bytes().unwrap();
    assert_eq!(EnforcePlan::from_bytes(&bytes).unwrap(), plan);
    assert!(plan.keeps_env("PATH"));
    assert!(plan.keeps_env("LC_ALL"));
    assert!(!plan.keeps_env("SECRET"));
    // malformed, another format, unknown fields: refused
    assert!(EnforcePlan::from_bytes(b"{").is_err());
    let other = String::from_utf8(bytes.clone()).unwrap().replace(
      &format!("\"format\":{PLAN_FORMAT}"),
      &format!("\"format\":{}", PLAN_FORMAT + 1),
    );
    assert_ne!(other.as_bytes(), &bytes[..]);
    assert!(EnforcePlan::from_bytes(other.as_bytes()).is_err());
    let extra =
      String::from_utf8(bytes)
        .unwrap()
        .replacen('{', "{\"widen\":true,", 1);
    assert!(EnforcePlan::from_bytes(extra.as_bytes()).is_err());
  }

  #[test]
  fn handled_per_abi() {
    let p = linux(full(), NetPlan::Denied);
    // no net handling before ABI 4 (seccomp holds "none"), scopes refused
    let (fs, net, scoped) = handled(&p, 1).unwrap();
    assert_eq!(net, 0);
    assert_eq!(scoped, 0);
    assert_eq!(fs & ACCESS_FS_REFER, 0);
    assert_eq!(fs & ACCESS_FS_TRUNCATE, 0);
    let (fs, net, _) = handled(&p, 6).unwrap();
    assert_ne!(fs & ACCESS_FS_REFER, 0);
    assert_ne!(fs & ACCESS_FS_TRUNCATE, 0);
    assert_ne!(fs & ACCESS_FS_IOCTL_DEV, 0);
    assert_ne!(fs & ACCESS_FS_EXECUTE, 0);
    assert_eq!(net, ACCESS_NET_BIND_TCP | ACCESS_NET_CONNECT_TCP);
    let ports = linux(
      full(),
      NetPlan::Ports {
        connect: Ports::List { ports: vec![443] },
        bind: Ports::Any,
      },
    );
    assert!(handled(&ports, 3).is_err());
    assert_eq!(handled(&ports, 4).unwrap().1, ACCESS_NET_CONNECT_TCP);
    let mut scoped = linux(full(), NetPlan::Open);
    scoped.scope_signal = true;
    assert!(handled(&scoped, 5).is_err());
    assert_eq!(handled(&scoped, 6).unwrap().2, SCOPE_SIGNAL);
  }

  // a desktop app on Chromium keeps the three namespaces its engine's
  // sandbox makes, and nothing else of the attack-surface list
  #[test]
  fn seccomp_program_for_a_desktop_engine() {
    let variant = DesktopVariant {
      engine_namespaces: true,
    };
    let p = seccomp_spec_variant(&full(), &NetPlan::Open, variant).program();
    let eperm = eno(libc::EPERM);
    // Chromium's launcher: user + PID + network namespaces, SIGCHLD
    assert_eq!(
      run_filter(&p, nr::CLONE, [0x7000_0011, 0, 0, 0, 0, 0]),
      SECCOMP_RET_ALLOW
    );
    assert_eq!(
      run_filter(&p, nr::UNSHARE, [0x1000_0000, 0, 0, 0, 0, 0]),
      SECCOMP_RET_ALLOW
    );
    // a mount, UTS, IPC, cgroup or time namespace is still refused, alone
    // or with the allowed ones
    for flag in [0x0002_0000u32, 0x0200_0000, 0x0400_0000, 0x0800_0000] {
      assert_eq!(run_filter(&p, nr::CLONE, [flag, 0, 0, 0, 0, 0]), eperm);
      assert_eq!(
        run_filter(&p, nr::CLONE, [flag | 0x7000_0000, 0, 0, 0, 0, 0]),
        eperm
      );
      assert_eq!(run_filter(&p, nr::UNSHARE, [flag, 0, 0, 0, 0, 0]), eperm);
    }
    assert_eq!(run_filter(&p, nr::UNSHARE, [0x80, 0, 0, 0, 0, 0]), eperm);
    assert_eq!(run_filter(&p, nr::SETNS, [0; 6]), eperm);
    for n in nr::SURFACE {
      assert_eq!(run_filter(&p, *n, [0; 6]), eperm, "syscall {n}");
    }
    assert_eq!(run_filter(&p, 435, [0; 6]), eno(libc::ENOSYS));
    // the default variant is the headless program's
    assert_eq!(
      seccomp_spec_variant(&full(), &NetPlan::Open, DesktopVariant::default())
        .program(),
      seccomp_spec(&full(), &NetPlan::Open).program()
    );
    let headless = seccomp_spec(&full(), &NetPlan::Open).program();
    assert_eq!(
      run_filter(&headless, nr::CLONE, [0x7000_0011, 0, 0, 0, 0, 0]),
      eperm
    );
  }

  #[test]
  fn seccomp_program_for_enforce() {
    let p = seccomp_spec(&full(), &NetPlan::Denied).program();
    let eperm = eno(libc::EPERM);
    assert_eq!(run_filter(&p, 435, [0; 6]), eno(libc::ENOSYS));
    // clone: threads pass, a new user namespace doesn't
    assert_eq!(
      run_filter(&p, nr::CLONE, [0x003d_0f00, 0, 0, 0, 0, 0]),
      SECCOMP_RET_ALLOW
    );
    assert_eq!(
      run_filter(&p, nr::CLONE, [0x1000_0011, 0, 0, 0, 0, 0]),
      eperm
    );
    assert_eq!(run_filter(&p, nr::UNSHARE, [0x80, 0, 0, 0, 0, 0]), eperm);
    assert_eq!(
      run_filter(&p, nr::UNSHARE, [0x200, 0, 0, 0, 0, 0]),
      SECCOMP_RET_ALLOW
    );
    for n in nr::SURFACE {
      assert_eq!(run_filter(&p, *n, [0; 6]), eperm, "syscall {n}");
    }
    assert_eq!(run_filter(&p, nr::EXECVE, [0; 6]), eperm);
    assert_eq!(run_filter(&p, nr::EXECVEAT, [0; 6]), eperm);
    assert_eq!(run_filter(&p, NR_LISTEN, [0; 6]), eperm);
    for n in nr::PATH_METADATA {
      assert_eq!(run_filter(&p, *n, [0; 6]), eperm, "syscall {n}");
    }
    assert_eq!(
      run_filter64(&p, nr::UTIMENSAT, [3, 0, 0, 0, 0, 0]),
      SECCOMP_RET_ALLOW
    );
    // before Landlock's TRUNCATE: truncate(2), read-only truncating opens,
    // openat2
    assert_eq!(run_filter(&p, nr::TRUNCATE, [0; 6]), eperm);
    for (n, arg) in nr::OPENS {
      let mut args = [0u32; 6];
      args[*arg as usize] = libc::O_TRUNC as u32;
      assert_eq!(run_filter(&p, *n, args), eperm, "open {n}");
      args[*arg as usize] = libc::O_TRUNC as u32 | 1;
      assert_eq!(run_filter(&p, *n, args), SECCOMP_RET_ALLOW, "open {n}");
    }
    assert_eq!(run_filter(&p, nr::OPENAT2, [0; 6]), eno(libc::ENOSYS));
    assert_eq!(
      run_filter64(&p, nr::UTIMENSAT, [3, 0x5555_0000, 0, 0, 0, 0]),
      eperm
    );
    // signals: this process only
    let own = std::process::id();
    for n in nr::SIGNAL_TO_PID {
      assert_eq!(
        run_filter(&p, *n, [own, 15, 0, 0, 0, 0]),
        SECCOMP_RET_ALLOW,
        "syscall {n}"
      );
      for other in [own + 1, 0, u32::MAX, 1] {
        assert_eq!(
          run_filter(&p, *n, [other, 15, 0, 0, 0, 0]),
          eperm,
          "syscall {n} to {other}"
        );
      }
    }
    assert_eq!(run_filter(&p, nr::TKILL, [own, 15, 0, 0, 0, 0]), eperm);
    assert_eq!(
      run_filter(&p, NR_PIDFD_SEND_SIGNAL, [3, 15, 0, 0, 0, 0]),
      eno(libc::ENOSYS)
    );
    // sockets: none of IP, Unix or netlink; socketpair isn't socket(2)
    for domain in [AF_INET, AF_UNIX, AF_NETLINK] {
      assert_eq!(
        run_filter(&p, NR_SOCKET, [domain, SOCK_STREAM, 0, 0, 0, 0]),
        eperm
      );
    }
    // the marker, and no other prctl option
    assert_eq!(
      run_filter(&p, nr::PRCTL, [MARKER_PRCTL_OPTION, 0, 0, 0, 0, 0]),
      eno(MARKER_ERRNO)
    );
    for option in [libc::PR_GET_NO_NEW_PRIVS, libc::PR_GET_SECCOMP] {
      assert_eq!(
        run_filter(&p, nr::PRCTL, [option as u32, 0, 0, 0, 0, 0]),
        SECCOMP_RET_ALLOW
      );
    }
    // never V8's memory syscalls (mmap, mprotect, madvise, futex, membarrier)
    #[cfg(target_arch = "x86_64")]
    let memory = [9u32, 10, 28, 202, 324, 329, 330];
    #[cfg(target_arch = "aarch64")]
    let memory = [222u32, 226, 233, 98, 283, 288, 289];
    for n in memory {
      assert_eq!(run_filter(&p, n, [0; 6]), SECCOMP_RET_ALLOW, "syscall {n}");
    }
  }

  // Under a broker that holds the network the tree makes no connection of
  // its own: connect(2) is refused whatever the socket or the address
  // (a socket the broker handed over can't be dissolved and re-pointed).
  #[test]
  fn seccomp_program_refuses_connect_under_a_net_broker() {
    let eacces = eno(libc::EACCES);
    let with = SeccompPlan {
      refuse_connect: true,
      ..Default::default()
    };
    let p = seccomp_spec(&with, &NetPlan::Denied).program();
    assert_eq!(
      run_filter(&p, crate::net_gate::NR_CONNECT, [3, 0, 16, 0, 0, 0]),
      eacces
    );
    let without =
      seccomp_spec(&SeccompPlan::default(), &NetPlan::Denied).program();
    assert_eq!(
      run_filter(&without, crate::net_gate::NR_CONNECT, [3, 0, 16, 0, 0, 0]),
      SECCOMP_RET_ALLOW
    );
  }

  #[test]
  fn seccomp_program_switches() {
    let none = SeccompPlan::default();
    let p = seccomp_spec(&none, &NetPlan::Open).program();
    // the marker is in every program
    assert_eq!(
      run_filter(&p, nr::PRCTL, [MARKER_PRCTL_OPTION, 0, 0, 0, 0, 0]),
      eno(MARKER_ERRNO)
    );
    assert_eq!(run_filter(&p, nr::EXECVE, [0; 6]), SECCOMP_RET_ALLOW);
    assert_eq!(run_filter(&p, NR_LISTEN, [0; 6]), SECCOMP_RET_ALLOW);
    assert_eq!(run_filter(&p, 435, [0; 6]), SECCOMP_RET_ALLOW);
    assert_eq!(
      run_filter(&p, NR_SOCKET, [AF_INET, SOCK_STREAM, 0, 0, 0, 0]),
      SECCOMP_RET_ALLOW
    );
    assert_eq!(
      run_filter(&p, nr::SIGNAL_TO_PID[0], [1, 9, 0, 0, 0, 0]),
      SECCOMP_RET_ALLOW
    );
    let route = SeccompPlan {
      netlink_route: true,
      unix_sockets: true,
      ..Default::default()
    };
    let p = seccomp_spec(
      &route,
      &NetPlan::Ports {
        connect: Ports::List { ports: vec![443] },
        bind: Ports::List { ports: vec![] },
      },
    )
    .program();
    let raw = libc::SOCK_RAW as u32;
    assert_eq!(
      run_filter(&p, NR_SOCKET, [AF_NETLINK, raw, NETLINK_ROUTE, 0, 0, 0]),
      SECCOMP_RET_ALLOW
    );
    assert_eq!(
      run_filter(&p, NR_SOCKET, [AF_NETLINK, raw, 15, 0, 0, 0]),
      eno(libc::EPERM)
    );
    assert_eq!(
      run_filter(&p, NR_SOCKET, [AF_UNIX, SOCK_STREAM, 0, 0, 0, 0]),
      SECCOMP_RET_ALLOW
    );
    assert_eq!(
      run_filter(
        &p,
        NR_SOCKET,
        [AF_INET, libc::SOCK_DGRAM as u32, 0, 0, 0, 0]
      ),
      eno(libc::EPERM)
    );
  }

  // the probe installs a filter in a child only: this process is unchanged
  #[test]
  fn seccomp_probe_leaves_the_process_alone() {
    let before = std::fs::read_to_string("/proc/self/status").unwrap();
    let field = |s: &str| {
      s.lines()
        .find(|l| l.starts_with("Seccomp:"))
        .map(|l| l.to_string())
    };
    let result = seccomp_available();
    let after = std::fs::read_to_string("/proc/self/status").unwrap();
    assert_eq!(field(&before), field(&after));
    // a process already under a filter that refuses seccomp(2) / prctl's
    // filter mode would be told so; one that isn't can install it
    if field(&before).as_deref() == Some("Seccomp:\t0") {
      assert_eq!(result, Ok(()));
    }
  }

  // openat2 refused by an outer seccomp profile is told apart from a path
  // that can't be opened (which only leaves its rule out)
  #[test]
  fn refused_openat2_is_reported() {
    assert!(openat2_unavailable(Some(libc::ENOSYS)));
    assert!(openat2_unavailable(Some(libc::EPERM)));
    assert!(!openat2_unavailable(Some(libc::ENOENT)));
    assert!(!openat2_unavailable(Some(libc::ELOOP)));
    assert!(!openat2_unavailable(Some(libc::EACCES)));
    // measured in a child under a filter refusing openat2 with EPERM (an
    // outer profile); the program is built before the fork
    let program = SeccompSpec::new()
      .unix_sockets(true)
      .inet_sockets(InetSockets::Any)
      .rule(SyscallRule::Deny {
        nr: SYS_OPENAT2 as u32,
        errno: libc::EPERM,
      })
      .program();
    let prog = SockFprog {
      len: (program.len() / 8) as u16,
      filter: program.as_ptr() as *const SockFilter,
    };
    // SAFETY: fork; the child runs only prctl, the raw syscall and _exit
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
      // SAFETY: the child's only thread, then _exit
      unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
          || libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            &prog as *const SockFprog,
            0,
            0,
          ) != 0
        {
          libc::_exit(2);
        }
        let fd = open_no_symlinks(c"/");
        let refused =
          fd < 0 && openat2_unavailable(Some(*libc::__errno_location()));
        libc::_exit(if refused { 0 } else { 1 });
      }
    }
    let mut status = 0;
    // SAFETY: waiting for the child just forked
    unsafe { libc::waitpid(pid, &mut status, 0) };
    assert!(libc::WIFEXITED(status));
    match libc::WEXITSTATUS(status) {
      // a host that can't install a filter can't measure this
      2 => {}
      code => assert_eq!(code, 0, "openat2 under the refusing filter"),
    }
    // unfiltered, it works (kernels from 5.6)
    assert_eq!(openat2_available(), Ok(()));
  }

  // A listener bound before the plan applies is handed out afterwards, and
  // accepts under a filter that lets the process make no IP socket.
  #[test]
  fn listeners_bound_before_the_filter_still_accept() {
    use std::os::fd::IntoRawFd;
    // an address in use is recorded with its error, not fatal
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let taken_addr = taken.local_addr().unwrap();
    bind_prebound(&[taken_addr]);
    let err = deno_net_route::prebound_tcp(&taken_addr).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
    // a free one is bound (another test may take a port between the probe
    // that finds it free and the bind: then the next one)
    let (addr, listener) = (0..50)
      .find_map(|_| {
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = free.local_addr().unwrap();
        drop(free);
        bind_prebound(&[addr]);
        let listener = deno_net_route::prebound_tcp(&addr).ok()??;
        Some((addr, listener.into_raw_fd()))
      })
      .expect("a free port to bind before");
    // a client waits in the backlog: the address listens from the start
    let _client = std::net::TcpStream::connect(addr).unwrap();

    let program = seccomp_spec(&full(), &NetPlan::Denied).program();
    let prog = SockFprog {
      len: (program.len() / 8) as u16,
      filter: program.as_ptr() as *const SockFilter,
    };
    // SAFETY: fork; the child runs only raw syscalls and _exit
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
      // SAFETY: the child's only thread, then _exit
      unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
          || libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            &prog as *const SockFprog,
            0,
            0,
          ) != 0
        {
          libc::_exit(2);
        }
        // no IP socket of its own ...
        if libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) >= 0
          || *libc::__errno_location() != libc::EPERM
        {
          libc::_exit(3);
        }
        // ... and the listener bound before still accepts
        let accepted = libc::accept4(
          listener,
          std::ptr::null_mut(),
          std::ptr::null_mut(),
          libc::SOCK_CLOEXEC,
        );
        libc::_exit(if accepted >= 0 { 0 } else { 4 });
      }
    }
    let mut status = 0;
    // SAFETY: waiting for the child just forked
    unsafe { libc::waitpid(pid, &mut status, 0) };
    assert!(libc::WIFEXITED(status));
    match libc::WEXITSTATUS(status) {
      // a host that can't install a filter can't measure this
      2 => {}
      code => assert_eq!(code, 0, "3: a socket was made; 4: no accept"),
    }
    // SAFETY: the duplicate taken above
    unsafe { libc::close(listener) };
  }

  // A folder the module graph listed (a template import's expansion) can be
  // listed under the plan, and nothing more: no file in it can be read, a
  // folder the plan doesn't name can't be listed, and a listed path that
  // isn't a folder grants nothing. (Landlock's rights cover the hierarchy
  // beneath a rule: a subfolder of a listed folder lists too, and still
  // reads no file.)
  #[test]
  fn listed_dirs_list_and_read_nothing() {
    let Ok(abi) = landlock_abi() else {
      return; // a kernel without Landlock can't measure this
    };
    let root = std::env::temp_dir()
      .join(format!("deno-sandbox-listed-dirs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("listed/sub")).unwrap();
    std::fs::create_dir_all(root.join("other")).unwrap();
    std::fs::write(root.join("listed/mod.js"), "export {};\n").unwrap();
    std::fs::write(root.join("other/mod.js"), "export {};\n").unwrap();
    let plan = LinuxPlan {
      handled: Handled {
        read: true,
        write: false,
        exec: false,
      },
      rules: Vec::new(),
      ..linux(SeccompPlan::default(), NetPlan::Open)
    };
    let ruleset = build_ruleset(
      &plan,
      abi,
      &[],
      &[root.join("listed"), root.join("other/mod.js")],
      &[],
      &[],
    )
    .unwrap()
    .expect("a read-handling plan builds a ruleset");
    let c =
      |p: &str| CString::new(root.join(p).as_os_str().as_bytes()).unwrap();
    let (listed, sub, file, other, other_file) = (
      c("listed"),
      c("listed/sub"),
      c("listed/mod.js"),
      c("other"),
      c("other/mod.js"),
    );
    // SAFETY: fork; the child runs only raw syscalls and _exit
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
      // SAFETY: the child's only thread, then _exit
      unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
          || libc::syscall(
            SYS_LANDLOCK_RESTRICT_SELF,
            ruleset.as_raw_fd(),
            0u32,
          ) != 0
        {
          libc::_exit(2);
        }
        let opens = |path: &CString, flags: i32| {
          let fd = libc::open(path.as_ptr(), flags | libc::O_CLOEXEC);
          if fd >= 0 {
            libc::close(fd);
          }
          fd >= 0
        };
        let dir = libc::O_RDONLY | libc::O_DIRECTORY;
        let code = if !opens(&listed, dir) {
          3
        } else if !opens(&sub, dir) {
          4
        } else if opens(&file, libc::O_RDONLY) {
          5
        } else if opens(&other, dir) {
          6
        } else if opens(&other_file, libc::O_RDONLY) {
          7
        } else {
          0
        };
        libc::_exit(code);
      }
    }
    let mut status = 0;
    // SAFETY: waiting for the child just forked
    unsafe { libc::waitpid(pid, &mut status, 0) };
    let _ = std::fs::remove_dir_all(&root);
    assert!(libc::WIFEXITED(status));
    match libc::WEXITSTATUS(status) {
      // a host that can't restrict itself can't measure this
      2 => {}
      code => assert_eq!(
        code, 0,
        "3/4: the listed folder (or its subfolder) didn't list; 5: a file in it read; 6: an unlisted folder listed; 7: a listed file path read"
      ),
    }
  }

  #[test]
  fn sealed_memfd_hand_over() {
    let plan = EnforcePlan {
      format: PLAN_FORMAT,
      linux: Some(linux(full(), NetPlan::Open)),
      macos: None,
      windows: None,
      env_keep: None,
      probe: None,
      module_files: Vec::new(),
      expanded: Vec::new(),
      config_env: Vec::new(),
      cache_overlay: None,
      broker: None,
      expanded_write: Vec::new(),
      entries: Vec::new(),
      member_configs: Vec::new(),
      listed_dirs: Vec::new(),
    };
    let fd = seal_plan(&plan.to_bytes().unwrap()).unwrap();
    // sealed: no more writes
    // SAFETY: a valid descriptor and buffer
    let n =
      unsafe { libc::write(fd.as_raw_fd(), b"x".as_ptr() as *const _, 1) };
    assert_eq!(n, -1);
    let bytes = read_sealed(fd).unwrap();
    assert_eq!(EnforcePlan::from_bytes(&bytes).unwrap(), plan);
    // an unsealed memfd or a plain file is refused
    // SAFETY: a valid name
    let raw =
      unsafe { libc::memfd_create(c"x".as_ptr(), libc::MFD_ALLOW_SEALING) };
    // SAFETY: just created
    let unsealed = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: a valid descriptor and buffer
    unsafe { libc::write(unsealed.as_raw_fd(), b"{}".as_ptr() as *const _, 2) };
    assert!(read_sealed(unsealed).unwrap_err().contains("not sealed"));
    let file = std::fs::File::open("/proc/self/status").unwrap();
    assert!(
      read_sealed(file.into())
        .unwrap_err()
        .contains("not a memfd")
    );
  }
}
