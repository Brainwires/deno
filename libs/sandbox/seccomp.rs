// Copyright 2018-2026 the Deno authors. MIT license.

//! A classic-BPF seccomp program built from a [`SeccompSpec`]: a producer
//! (the lifecycle-script jail today) lists per-syscall rules and says which
//! `socket(2)` families pass; [`SeccompSpec::program`] assembles them, on
//! x86_64 and aarch64, into the program `linux::apply_to_command` installs.
//!
//! Every program has the same frame:
//!
//! 1. another architecture's syscalls (x32, the 32-bit compat ABI) kill the
//!    process;
//! 2. the rules, in the order they were added; each one either answers its
//!    syscall or leaves it to the rules after it;
//! 3. `socket(2)`: netlink as [`NetlinkSockets`] says (by default every
//!    protocol), Unix and vsock sockets only when asked for, IPv4 / IPv6 as
//!    [`InetSockets`] says, every other family fails with the socket errno
//!    (`EACCES` by default); every other syscall is allowed.
//!
//! A spec with the defaults emits the program the lifecycle jail has always
//! installed (its golden bytes are tested in the jail).
//!
//! The instructions emitted are loads of `seccomp_data` words, AND, JEQ / JGE
//! and RET; [`run_filter`] interprets exactly those, for tests.

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct SockFilter {
  pub(crate) code: u16,
  pub(crate) jt: u8,
  pub(crate) jf: u8,
  pub(crate) k: u32,
}

#[repr(C)]
pub(crate) struct SockFprog {
  pub(crate) len: u16,
  pub(crate) filter: *const SockFilter,
}

pub const BPF_LD_W_ABS: u16 = 0x20;
pub const BPF_ALU_AND_K: u16 = 0x54;
pub const BPF_JEQ_K: u16 = 0x15;
pub const BPF_JGE_K: u16 = 0x35;
pub const BPF_RET_K: u16 = 0x06;
pub const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
pub const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
pub const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
pub const SECCOMP_DATA_NR: u32 = 0;
pub const SECCOMP_DATA_ARCH: u32 = 4;
pub const SECCOMP_DATA_ARG0: u32 = 16;
/// The low 32 bits of argument `n` (little-endian).
pub const fn seccomp_arg(n: u32) -> u32 {
  SECCOMP_DATA_ARG0 + 8 * n
}
/// The high 32 bits of argument `n` (little-endian).
pub const fn seccomp_arg_hi(n: u32) -> u32 {
  SECCOMP_DATA_ARG0 + 8 * n + 4
}
pub const SOCK_TYPE_MASK: u32 = 0xf;
pub const SOCK_STREAM: u32 = 1;
pub const IPPROTO_TCP: u32 = 6;
pub const MSG_FASTOPEN: u32 = 0x2000_0000;
pub const AF_NETLINK: u32 = 16;
pub const AF_INET: u32 = 2;
pub const AF_INET6: u32 = 10;
pub const AF_UNIX: u32 = 1;
pub const AF_VSOCK: u32 = 40;
pub const NETLINK_ROUTE: u32 = 0;
pub const NR_IO_URING_SETUP: u32 = 425;
/// `ioctl` requests that type into a terminal (asm-generic, the same on
/// x86_64 and aarch64).
pub const TIOCSTI: u32 = 0x5412;
pub const TIOCLINUX: u32 = 0x541C;

#[cfg(target_arch = "x86_64")]
pub const AUDIT_ARCH: u32 = 0xC000_003E;
#[cfg(target_arch = "x86_64")]
pub const NR_IOCTL: u32 = 16;
#[cfg(target_arch = "x86_64")]
pub const NR_SOCKET: u32 = 41;
#[cfg(target_arch = "x86_64")]
pub const NR_SENDTO: u32 = 44;
#[cfg(target_arch = "x86_64")]
pub const NR_SENDMSG: u32 = 46;
#[cfg(target_arch = "x86_64")]
pub const NR_SENDMMSG: u32 = 307;
#[cfg(target_arch = "x86_64")]
pub const NR_LISTEN: u32 = 50;
#[cfg(target_arch = "aarch64")]
pub const AUDIT_ARCH: u32 = 0xC000_00B7;
#[cfg(target_arch = "aarch64")]
pub const NR_IOCTL: u32 = 29;
#[cfg(target_arch = "aarch64")]
pub const NR_SOCKET: u32 = 198;
#[cfg(target_arch = "aarch64")]
pub const NR_SENDTO: u32 = 206;
#[cfg(target_arch = "aarch64")]
pub const NR_SENDMSG: u32 = 211;
#[cfg(target_arch = "aarch64")]
pub const NR_SENDMMSG: u32 = 269;
#[cfg(target_arch = "aarch64")]
pub const NR_LISTEN: u32 = 201;

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

const fn errno(e: i32) -> u32 {
  SECCOMP_RET_ERRNO | (e as u32 & 0xffff)
}

/// What one rule does to its syscall. A rule that doesn't answer leaves the
/// syscall to the rules after it (the accumulator holds the syscall number
/// again when it ends).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyscallRule {
  /// `nr` fails with `errno`.
  Deny { nr: u32, errno: i32 },
  /// `nr` fails with `errno` when the low 32 bits of argument `arg` equal
  /// one of `values` (1 to 64 of them); any other value goes on to the next
  /// rule.
  DenyArgIn {
    nr: u32,
    arg: u32,
    values: Vec<u32>,
    errno: i32,
  },
  /// `nr` fails with `errno` when argument `arg` has any bit of `mask` set,
  /// and is allowed otherwise: no later rule sees it.
  DenyArgBits {
    nr: u32,
    arg: u32,
    mask: u32,
    errno: i32,
  },
  /// `nr` fails with `errno` when argument `arg` (all 64 bits) is not zero
  /// (a path pointer: `utimensat(fd, NULL, ...)` is the descriptor form);
  /// zero goes on to the next rule.
  DenyArgNonZero { nr: u32, arg: u32, errno: i32 },
  /// `nr` fails with `errno` when the low 32 bits of argument `arg`, masked
  /// with `mask`, equal `value` (open flags: `O_RDONLY | O_TRUNC`); anything
  /// else goes on to the next rule.
  DenyArgMasked {
    nr: u32,
    arg: u32,
    mask: u32,
    value: u32,
    errno: i32,
  },
  /// `nr` fails with `errno` unless the low 32 bits of argument `arg` equal
  /// `value` (a signal's target: this process only); `value` goes on to the
  /// next rule.
  DenyArgNotEq {
    nr: u32,
    arg: u32,
    value: u32,
    errno: i32,
  },
  /// `nr` fails with `errno` when every one of `tests` (1 to 8 of them)
  /// holds; otherwise it goes on to the next rule.
  DenyArgsAll {
    nr: u32,
    tests: Vec<ArgTest>,
    errno: i32,
  },
}

/// One test of a [`SyscallRule::DenyArgsAll`]: the low 32 bits of argument
/// `arg`, ANDed with `mask`, equal `value` (or, with `equal: false`, don't).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgTest {
  pub arg: u32,
  pub mask: u32,
  pub value: u32,
  pub equal: bool,
}

/// Which IPv4 / IPv6 sockets `socket(2)` creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InetSockets {
  /// None.
  Denied,
  /// Any type and protocol.
  Any,
  /// Stream sockets with protocol 0 or TCP only (no UDP, SCTP or MPTCP).
  TcpStream,
}

/// Which netlink sockets `socket(2)` creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetlinkSockets {
  /// Every protocol (the jail's default).
  Any,
  /// `NETLINK_ROUTE` only (`getifaddrs`, `AI_ADDRCONFIG`).
  RouteOnly,
  /// None.
  Denied,
}

/// A seccomp program to assemble: the rules in order, then the `socket(2)`
/// filter. With no rules and the defaults, everything is allowed but
/// `socket(2)` outside netlink (and another architecture's syscalls).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeccompSpec {
  rules: Vec<SyscallRule>,
  inet: InetSockets,
  netlink: NetlinkSockets,
  unix: bool,
  vsock: bool,
  socket_errno: i32,
}

impl Default for SeccompSpec {
  fn default() -> Self {
    Self::new()
  }
}

impl SeccompSpec {
  pub fn new() -> Self {
    Self {
      rules: Vec::new(),
      inet: InetSockets::Denied,
      netlink: NetlinkSockets::Any,
      unix: false,
      vsock: false,
      socket_errno: libc::EACCES,
    }
  }

  /// Sets which netlink sockets pass the `socket(2)` filter.
  pub fn netlink_sockets(mut self, netlink: NetlinkSockets) -> Self {
    self.netlink = netlink;
    self
  }

  /// Lets `socket(AF_UNIX)` pass.
  pub fn unix_sockets(mut self, allow: bool) -> Self {
    self.unix = allow;
    self
  }

  /// Lets `socket(AF_VSOCK)` pass.
  pub fn vsock_sockets(mut self, allow: bool) -> Self {
    self.vsock = allow;
    self
  }

  /// The errno of a refused `socket(2)`.
  pub fn socket_errno(mut self, errno: i32) -> Self {
    self.socket_errno = errno;
    self
  }

  /// Adds a rule after the ones already added.
  pub fn rule(mut self, rule: SyscallRule) -> Self {
    self.rules.push(rule);
    self
  }

  /// Sets which IPv4 / IPv6 sockets pass the `socket(2)` filter.
  pub fn inet_sockets(mut self, inet: InetSockets) -> Self {
    self.inet = inet;
    self
  }

  pub fn rules(&self) -> &[SyscallRule] {
    &self.rules
  }

  pub fn inet(&self) -> InetSockets {
    self.inet
  }

  /// The program, as the kernel's `sock_filter` array (eight bytes an
  /// instruction, native byte order); empty on an architecture the
  /// assembler doesn't know, where nothing is installed.
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  pub fn program(&self) -> Vec<u8> {
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
    for rule in &self.rules {
      emit_rule(&mut f, rule);
    }
    f.push(jump(BPF_JEQ_K, NR_SOCKET, 1, 0));
    f.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
    // socket(domain, type, protocol): the low 32 bits of each argument
    f.push(stmt(BPF_LD_W_ABS, seccomp_arg(0)));
    let refuse = errno(self.socket_errno);
    match self.netlink {
      NetlinkSockets::Any => {
        f.push(jump(BPF_JEQ_K, AF_NETLINK, 0, 1));
        f.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
      }
      NetlinkSockets::RouteOnly => {
        // the protocol is argument 2; any other netlink protocol is refused
        // here (the accumulator no longer holds the family)
        f.push(jump(BPF_JEQ_K, AF_NETLINK, 0, 4));
        f.push(stmt(BPF_LD_W_ABS, seccomp_arg(2)));
        f.push(jump(BPF_JEQ_K, NETLINK_ROUTE, 0, 1));
        f.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
        f.push(stmt(BPF_RET_K, refuse));
      }
      // falls through to the refusal at the end
      NetlinkSockets::Denied => {}
    }
    if self.unix {
      f.push(jump(BPF_JEQ_K, AF_UNIX, 0, 1));
      f.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
    }
    if self.vsock {
      f.push(jump(BPF_JEQ_K, AF_VSOCK, 0, 1));
      f.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
    }
    match self.inet {
      InetSockets::Denied => {}
      InetSockets::Any => {
        f.push(jump(BPF_JEQ_K, AF_INET, 1, 0));
        f.push(jump(BPF_JEQ_K, AF_INET6, 0, 1));
        f.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
      }
      InetSockets::TcpStream => {
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
    f.push(stmt(BPF_RET_K, refuse));
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
  pub fn program(&self) -> Vec<u8> {
    Vec::new()
  }
}

/// Emits one rule; the accumulator holds the syscall number before and,
/// when the rule doesn't answer, after.
fn emit_rule(f: &mut Vec<SockFilter>, rule: &SyscallRule) {
  match rule {
    SyscallRule::Deny { nr, errno: e } => {
      f.push(jump(BPF_JEQ_K, *nr, 0, 1));
      f.push(stmt(BPF_RET_K, errno(*e)));
    }
    SyscallRule::DenyArgIn {
      nr,
      arg,
      values,
      errno: e,
    } => {
      assert!(
        !values.is_empty() && values.len() <= 64,
        "a DenyArgIn rule takes 1 to 64 values"
      );
      let n = values.len();
      // past the load, the comparisons, the return and the reload
      f.push(jump(BPF_JEQ_K, *nr, 0, (n + 3) as u8));
      f.push(stmt(BPF_LD_W_ABS, seccomp_arg(*arg)));
      for (i, value) in values.iter().enumerate() {
        if i + 1 < n {
          // a match jumps over the remaining comparisons to the return
          f.push(jump(BPF_JEQ_K, *value, (n - 1 - i) as u8, 0));
        } else {
          // the last: a match falls through to the return, anything else
          // jumps over it
          f.push(jump(BPF_JEQ_K, *value, 0, 1));
        }
      }
      f.push(stmt(BPF_RET_K, errno(*e)));
      f.push(stmt(BPF_LD_W_ABS, SECCOMP_DATA_NR));
    }
    SyscallRule::DenyArgBits {
      nr,
      arg,
      mask,
      errno: e,
    } => {
      f.push(jump(BPF_JEQ_K, *nr, 0, 5));
      f.push(stmt(BPF_LD_W_ABS, seccomp_arg(*arg)));
      f.push(stmt(BPF_ALU_AND_K, *mask));
      f.push(jump(BPF_JEQ_K, 0, 0, 1));
      f.push(stmt(BPF_RET_K, SECCOMP_RET_ALLOW));
      f.push(stmt(BPF_RET_K, errno(*e)));
    }
    SyscallRule::DenyArgNonZero { nr, arg, errno: e } => {
      f.push(jump(BPF_JEQ_K, *nr, 0, 6));
      f.push(stmt(BPF_LD_W_ABS, seccomp_arg(*arg)));
      // low word set: refused
      f.push(jump(BPF_JEQ_K, 0, 0, 2));
      f.push(stmt(BPF_LD_W_ABS, seccomp_arg_hi(*arg)));
      // high word clear too: on to the next rule
      f.push(jump(BPF_JEQ_K, 0, 1, 0));
      f.push(stmt(BPF_RET_K, errno(*e)));
      f.push(stmt(BPF_LD_W_ABS, SECCOMP_DATA_NR));
    }
    SyscallRule::DenyArgMasked {
      nr,
      arg,
      mask,
      value,
      errno: e,
    } => {
      f.push(jump(BPF_JEQ_K, *nr, 0, 5));
      f.push(stmt(BPF_LD_W_ABS, seccomp_arg(*arg)));
      f.push(stmt(BPF_ALU_AND_K, *mask));
      f.push(jump(BPF_JEQ_K, *value, 0, 1));
      f.push(stmt(BPF_RET_K, errno(*e)));
      f.push(stmt(BPF_LD_W_ABS, SECCOMP_DATA_NR));
    }
    SyscallRule::DenyArgNotEq {
      nr,
      arg,
      value,
      errno: e,
    } => {
      f.push(jump(BPF_JEQ_K, *nr, 0, 4));
      f.push(stmt(BPF_LD_W_ABS, seccomp_arg(*arg)));
      // equal: over the return, to the reload
      f.push(jump(BPF_JEQ_K, *value, 1, 0));
      f.push(stmt(BPF_RET_K, errno(*e)));
      f.push(stmt(BPF_LD_W_ABS, SECCOMP_DATA_NR));
    }
    SyscallRule::DenyArgsAll {
      nr,
      tests,
      errno: e,
    } => {
      assert!(
        !tests.is_empty() && tests.len() <= 8,
        "a DenyArgsAll rule takes 1 to 8 tests"
      );
      let n = tests.len();
      // another syscall: past the tests, the return and the reload
      f.push(jump(BPF_JEQ_K, *nr, 0, (3 * n + 2) as u8));
      for (i, test) in tests.iter().enumerate() {
        f.push(stmt(BPF_LD_W_ABS, seccomp_arg(test.arg)));
        f.push(stmt(BPF_ALU_AND_K, test.mask));
        // a failed test jumps to the reload, past the remaining tests and
        // the return
        let fail = (3 * (n - i) - 2) as u8;
        if test.equal {
          f.push(jump(BPF_JEQ_K, test.value, 0, fail));
        } else {
          f.push(jump(BPF_JEQ_K, test.value, fail, 0));
        }
      }
      f.push(stmt(BPF_RET_K, errno(*e)));
      f.push(stmt(BPF_LD_W_ABS, SECCOMP_DATA_NR));
    }
  }
}

/// Runs a program from [`SeccompSpec::program`] on one syscall of this
/// architecture and returns its answer (`SECCOMP_RET_*`). It interprets the
/// instructions the assembler emits (loads of `seccomp_data` words, AND,
/// JEQ / JGE and RET) and panics on anything else: a test aid, not a
/// general BPF interpreter.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn run_filter(program: &[u8], nr: u32, args: [u32; 6]) -> u32 {
  run_filter64(program, nr, args.map(u64::from))
}

/// [`run_filter`] with the full 64-bit arguments.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn run_filter64(program: &[u8], nr: u32, args: [u64; 6]) -> u32 {
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
    o if o >= SECCOMP_DATA_ARG0
      && (o - SECCOMP_DATA_ARG0).is_multiple_of(4) =>
    {
      let i = ((o - SECCOMP_DATA_ARG0) / 8) as usize;
      if (o - SECCOMP_DATA_ARG0).is_multiple_of(8) {
        args[i] as u32
      } else {
        (args[i] >> 32) as u32
      }
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

#[cfg(all(test, any(target_arch = "x86_64", target_arch = "aarch64")))]
mod tests {
  use super::*;

  const ALLOW: u32 = SECCOMP_RET_ALLOW;

  fn eno(e: i32) -> u32 {
    errno(e)
  }

  #[test]
  fn empty_spec_allows_all_but_sockets() {
    let p = SeccompSpec::new().program();
    assert_eq!(p.len() % 8, 0);
    assert_eq!(run_filter(&p, 0, [0; 6]), ALLOW);
    assert_eq!(run_filter(&p, NR_IOCTL, [0, TIOCSTI, 0, 0, 0, 0]), ALLOW);
    assert_eq!(
      run_filter(&p, NR_SOCKET, [AF_NETLINK, 3, 0, 0, 0, 0]),
      ALLOW
    );
    assert_eq!(
      run_filter(&p, NR_SOCKET, [AF_INET, SOCK_STREAM, 0, 0, 0, 0]),
      eno(libc::EACCES)
    );
    assert_eq!(
      run_filter(&p, NR_SOCKET, [libc::AF_UNIX as u32, 1, 0, 0, 0, 0]),
      eno(libc::EACCES)
    );
  }

  #[test]
  fn deny_rule() {
    let p = SeccompSpec::new()
      .rule(SyscallRule::Deny {
        nr: 7,
        errno: libc::EPERM,
      })
      .program();
    assert_eq!(run_filter(&p, 7, [0; 6]), eno(libc::EPERM));
    assert_eq!(run_filter(&p, 8, [0; 6]), ALLOW);
  }

  // every position of a value in a DenyArgIn list matches, other values and
  // syscalls go on to the rules after it with the number reloaded
  #[test]
  fn deny_arg_in_rule() {
    for n in [1usize, 2, 3, 7] {
      let values = (0..n as u32).map(|i| 100 + i).collect::<Vec<_>>();
      let p = SeccompSpec::new()
        .rule(SyscallRule::DenyArgIn {
          nr: 9,
          arg: 2,
          values: values.clone(),
          errno: libc::EPERM,
        })
        .rule(SyscallRule::Deny {
          nr: 9,
          errno: libc::ENOSYS,
        })
        .rule(SyscallRule::Deny {
          nr: 10,
          errno: libc::EBADF,
        })
        .program();
      for v in &values {
        assert_eq!(run_filter(&p, 9, [0, 0, *v, 0, 0, 0]), eno(libc::EPERM));
        // the value in another argument doesn't count
        assert_eq!(run_filter(&p, 9, [*v, 0, 0, 0, 0, 0]), eno(libc::ENOSYS));
      }
      assert_eq!(run_filter(&p, 9, [0, 0, 99, 0, 0, 0]), eno(libc::ENOSYS));
      assert_eq!(run_filter(&p, 10, [0, 0, 100, 0, 0, 0]), eno(libc::EBADF));
      assert_eq!(run_filter(&p, 11, [0, 0, 100, 0, 0, 0]), ALLOW);
    }
  }

  #[test]
  fn deny_arg_bits_rule() {
    let p = SeccompSpec::new()
      .rule(SyscallRule::DenyArgBits {
        nr: 12,
        arg: 3,
        mask: 0x10,
        errno: libc::EOPNOTSUPP,
      })
      .rule(SyscallRule::Deny {
        nr: 12,
        errno: libc::EBADF,
      })
      .program();
    assert_eq!(
      run_filter(&p, 12, [0, 0, 0, 0x11, 0, 0]),
      eno(libc::EOPNOTSUPP)
    );
    // clear: allowed, the rule after it doesn't see the call
    assert_eq!(run_filter(&p, 12, [0, 0, 0, 0x01, 0, 0]), ALLOW);
    assert_eq!(run_filter(&p, 13, [0, 0, 0, 0x10, 0, 0]), ALLOW);
  }

  // every test must hold; one failing, at any position, leaves the syscall
  // to the rules after it with the number reloaded; "not equal" tests too
  #[test]
  fn deny_args_all_rule() {
    for equal in [true, false] {
      for n in 1..=6u32 {
        // argument i, its low byte, against 10 + i
        let tests = (0..n)
          .map(|i| ArgTest {
            arg: i,
            mask: 0xff,
            value: 10 + i,
            equal,
          })
          .collect::<Vec<_>>();
        let p = SeccompSpec::new()
          .rule(SyscallRule::DenyArgsAll {
            nr: 14,
            tests,
            errno: libc::EPERM,
          })
          .rule(SyscallRule::Deny {
            nr: 14,
            errno: libc::ENOSYS,
          })
          .program();
        // every test holds (the mask drops the high bits)
        let mut held = [0u32; 6];
        for i in 0..n as usize {
          held[i] = if equal {
            0x100 | (10 + i as u32)
          } else {
            0x100
          };
        }
        assert_eq!(run_filter(&p, 14, held), eno(libc::EPERM), "{n}");
        for j in 0..n as usize {
          let mut failed = held;
          failed[j] = if equal { 0 } else { 10 + j as u32 };
          assert_eq!(
            run_filter(&p, 14, failed),
            eno(libc::ENOSYS),
            "{equal} {n} {j}"
          );
        }
        assert_eq!(run_filter(&p, 15, held), ALLOW, "{n}");
      }
    }
  }

  // a test of two arguments, as the jail's netlink rule uses it: the family
  // equal and the protocol not
  #[test]
  fn deny_args_all_two_arguments() {
    let p = SeccompSpec::new()
      .rule(SyscallRule::DenyArgsAll {
        nr: NR_SOCKET,
        tests: vec![
          ArgTest {
            arg: 0,
            mask: u32::MAX,
            value: AF_NETLINK,
            equal: true,
          },
          ArgTest {
            arg: 2,
            mask: u32::MAX,
            value: 0,
            equal: false,
          },
        ],
        errno: libc::EACCES,
      })
      .program();
    let socket =
      |a: [u32; 3]| run_filter(&p, NR_SOCKET, [a[0], a[1], a[2], 0, 0, 0]);
    assert_eq!(socket([AF_NETLINK, 3, 0]), ALLOW);
    assert_eq!(socket([AF_NETLINK, 3, 4]), eno(libc::EACCES));
    assert_eq!(socket([AF_NETLINK, 3, 12]), eno(libc::EACCES));
    // the rule is about netlink only; the family filter answers the rest
    assert_eq!(socket([AF_INET, SOCK_STREAM, 6]), eno(libc::EACCES));
    assert_eq!(socket([libc::AF_UNIX as u32, 1, 4]), eno(libc::EACCES));
  }

  #[test]
  fn inet_sockets() {
    let socket = |inet, domain, ty, proto| {
      run_filter(
        &SeccompSpec::new().inet_sockets(inet).program(),
        NR_SOCKET,
        [domain, ty, proto, 0, 0, 0],
      )
    };
    let dgram = libc::SOCK_DGRAM as u32;
    let eacces = eno(libc::EACCES);
    assert_eq!(
      socket(InetSockets::Denied, AF_INET6, SOCK_STREAM, 0),
      eacces
    );
    assert_eq!(socket(InetSockets::Any, AF_INET6, dgram, 17), ALLOW);
    assert_eq!(socket(InetSockets::Any, AF_INET, SOCK_STREAM, 132), ALLOW);
    assert_eq!(
      socket(InetSockets::TcpStream, AF_INET, SOCK_STREAM, 0),
      ALLOW
    );
    assert_eq!(
      socket(InetSockets::TcpStream, AF_INET6, SOCK_STREAM, IPPROTO_TCP),
      ALLOW
    );
    assert_eq!(socket(InetSockets::TcpStream, AF_INET, dgram, 0), eacces);
    assert_eq!(
      socket(InetSockets::TcpStream, AF_INET, SOCK_STREAM, 262),
      eacces
    );
    for inet in [
      InetSockets::Denied,
      InetSockets::Any,
      InetSockets::TcpStream,
    ] {
      assert_eq!(socket(inet, libc::AF_UNIX as u32, SOCK_STREAM, 0), eacces);
      assert_eq!(socket(inet, AF_NETLINK, dgram, 0), ALLOW);
    }
  }

  // the descriptor form (a null path) passes on, any non-null pointer is
  // refused, including one whose low word is zero
  #[test]
  fn deny_arg_non_zero_rule() {
    let p = SeccompSpec::new()
      .rule(SyscallRule::DenyArgNonZero {
        nr: 20,
        arg: 1,
        errno: libc::EPERM,
      })
      .rule(SyscallRule::Deny {
        nr: 20,
        errno: libc::EBADF,
      })
      .program();
    assert_eq!(run_filter64(&p, 20, [3, 0, 0, 0, 0, 0]), eno(libc::EBADF));
    assert_eq!(
      run_filter64(&p, 20, [3, 0x1000, 0, 0, 0, 0]),
      eno(libc::EPERM)
    );
    assert_eq!(
      run_filter64(&p, 20, [3, 0x7f00_0000_0000, 0, 0, 0, 0]),
      eno(libc::EPERM)
    );
    assert_eq!(run_filter64(&p, 21, [3, 0x1000, 0, 0, 0, 0]), ALLOW);
  }

  #[test]
  fn deny_arg_masked_rule() {
    let p = SeccompSpec::new()
      .rule(SyscallRule::DenyArgMasked {
        nr: 30,
        arg: 2,
        mask: 0x203,
        value: 0x200,
        errno: libc::EPERM,
      })
      .rule(SyscallRule::Deny {
        nr: 30,
        errno: libc::EBADF,
      })
      .program();
    // O_RDONLY | O_TRUNC (plus unrelated bits): refused
    assert_eq!(
      run_filter(&p, 30, [0, 0, 0x8200, 0, 0, 0]),
      eno(libc::EPERM)
    );
    // O_WRONLY | O_TRUNC, O_RDONLY: on to the next rule
    assert_eq!(run_filter(&p, 30, [0, 0, 0x201, 0, 0, 0]), eno(libc::EBADF));
    assert_eq!(run_filter(&p, 30, [0, 0, 0, 0, 0, 0]), eno(libc::EBADF));
    assert_eq!(run_filter(&p, 31, [0, 0, 0x200, 0, 0, 0]), ALLOW);
  }

  #[test]
  fn socket_knobs() {
    let socket = |spec: &SeccompSpec, domain, ty, proto| {
      run_filter(&spec.program(), NR_SOCKET, [domain, ty, proto, 0, 0, 0])
    };
    let raw = libc::SOCK_RAW as u32;
    let eperm = eno(libc::EPERM);
    let route_only = SeccompSpec::new()
      .netlink_sockets(NetlinkSockets::RouteOnly)
      .socket_errno(libc::EPERM);
    assert_eq!(socket(&route_only, AF_NETLINK, raw, NETLINK_ROUTE), ALLOW);
    assert_eq!(socket(&route_only, AF_NETLINK, raw, 15), eperm);
    assert_eq!(socket(&route_only, AF_UNIX, SOCK_STREAM, 0), eperm);
    assert_eq!(socket(&route_only, AF_INET, SOCK_STREAM, 0), eperm);
    let denied = SeccompSpec::new()
      .netlink_sockets(NetlinkSockets::Denied)
      .inet_sockets(InetSockets::Any)
      .socket_errno(libc::EPERM);
    assert_eq!(socket(&denied, AF_NETLINK, raw, NETLINK_ROUTE), eperm);
    assert_eq!(socket(&denied, AF_INET6, SOCK_STREAM, 0), ALLOW);
    let unix = SeccompSpec::new().unix_sockets(true).vsock_sockets(true);
    assert_eq!(socket(&unix, AF_UNIX, SOCK_STREAM, 0), ALLOW);
    assert_eq!(socket(&unix, AF_VSOCK, SOCK_STREAM, 0), ALLOW);
    assert_eq!(socket(&unix, AF_INET, SOCK_STREAM, 0), eno(libc::EACCES));
  }

  // the defaults emit the program the jail has always installed: the knobs
  // add nothing unless set
  #[test]
  fn default_knobs_add_nothing() {
    let base = SeccompSpec::new().inet_sockets(InetSockets::TcpStream);
    let explicit = base
      .clone()
      .netlink_sockets(NetlinkSockets::Any)
      .unix_sockets(false)
      .vsock_sockets(false)
      .socket_errno(libc::EACCES);
    assert_eq!(base.program(), explicit.program());
  }

  // another architecture's syscalls kill the process
  #[test]
  fn foreign_arch_kills() {
    let p = SeccompSpec::new().program();
    let ins = |i: usize| {
      u32::from_ne_bytes(p[i * 8 + 4..i * 8 + 8].try_into().unwrap())
    };
    assert_eq!(ins(1), AUDIT_ARCH);
    assert_eq!(ins(2), SECCOMP_RET_KILL_PROCESS);
    if cfg!(target_arch = "x86_64") {
      assert_eq!(
        run_filter(&p, 0x4000_0000 | 41, [0; 6]),
        SECCOMP_RET_KILL_PROCESS
      );
    }
  }
}
