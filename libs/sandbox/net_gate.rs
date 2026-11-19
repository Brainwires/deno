// Copyright 2018-2026 the Deno authors. MIT license.

//! `connect(2)` answered by the supervisor (Linux): what makes a
//! host-scoped network grant exact for native code and child processes on
//! every kernel.
//!
//! Under a broker policy the confined process gets its sockets from the
//! broker ([`crate::net_authority`]), and its children reach the network
//! through the allow-listing proxy ([`crate::proxy`]) on a loopback port.
//! Two things would undo that if `connect(2)` were left to the kernel:
//!
//! - a socket the broker connected can be pointed somewhere else: on Linux
//!   `connect(fd, {AF_UNSPEC})` dissolves a TCP association and the socket
//!   can then connect to any address. Landlock refuses that only from ABI 4
//!   (Linux 6.7), and only by port;
//! - "only the proxy" can't be said to the kernel: Landlock's TCP rules
//!   name a port, not an address (any host on the proxy's port), and before
//!   ABI 4 there are none; a network namespace needs unprivileged user
//!   namespaces, which containers and Ubuntu's AppArmor policy refuse.
//!
//! So the confined tree's seccomp filter hands every `connect` to the
//! supervisor ([`crate::notify::notify_program`] with
//! [`connect_syscalls`]), and a [`ConnectGate`] answers it:
//!
//! - the caller's `sockaddr` is copied out of its memory ONCE; the decision
//!   and the connection use that copy, so a thread that rewrites the
//!   address afterwards changes nothing;
//! - an address in the gate's list (the proxy's `127.0.0.1:<port>`) is
//!   connected by the supervisor itself, on its own duplicate of the
//!   caller's socket (`pidfd_getfd`), and the caller gets that result
//!   (`0`, `EINPROGRESS` for a non-blocking socket, `ECONNREFUSED`, ...).
//!   The kernel never runs the caller's own `connect`
//!   (`SECCOMP_USER_NOTIF_FLAG_CONTINUE` would read the address again);
//! - everything else is refused with `EACCES`: every other IP address,
//!   `AF_UNSPEC` (the dissolve), Unix sockets (the broker connects those),
//!   and any family the gate doesn't know.
//!
//! The other ways to reach an address without `connect(2)` are closed by
//! the enforcer's own seccomp program: `MSG_FASTOPEN` sends, `io_uring`,
//! UDP sockets (`socket(2)` passes TCP stream sockets only) and `listen`.
//!
//! Fails closed like [`crate::notify`]: a request that can't be read or
//! decided is refused, and with the supervisor gone `connect` fails with
//! `ENOSYS`.

use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::Ipv6Addr;
use std::net::SocketAddr;
use std::os::fd::AsRawFd;

use crate::notify::Answer;
use crate::notify::Call;
use crate::notify::errno;
use crate::notify::read_exact;
use crate::notify::steal_fd;

#[cfg(target_arch = "x86_64")]
pub const NR_CONNECT: u32 = 42;
#[cfg(target_arch = "aarch64")]
pub const NR_CONNECT: u32 = 203;

/// The syscalls a [`ConnectGate`] answers.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn connect_syscalls() -> Vec<u32> {
  vec![NR_CONNECT]
}

/// The largest `sockaddr` the kernel takes (`sockaddr_storage`).
const MAX_SOCKADDR: usize = 128;

/// Answers the `connect` calls of a confined process tree: only to the
/// addresses it lists.
#[derive(Debug, Clone, Default)]
pub struct ConnectGate {
  allow: Vec<SocketAddr>,
}

/// What a `sockaddr` copied from the caller names.
#[derive(Debug, PartialEq, Eq)]
enum Target {
  Ip(SocketAddr),
  /// `AF_UNSPEC`: dissolves the socket's association.
  Unspecified,
  /// A Unix socket, or a family the gate doesn't know.
  Other(u16),
}

/// Parses the first `len` bytes of a `sockaddr` as the kernel would;
/// `None`: too short for its family (the kernel answers `EINVAL`).
fn parse_sockaddr(bytes: &[u8]) -> Option<Target> {
  let family = u16::from_ne_bytes(bytes.get(..2)?.try_into().ok()?);
  match family as i32 {
    libc::AF_UNSPEC => Some(Target::Unspecified),
    libc::AF_INET => {
      // sockaddr_in: family, port (network order), address
      let port = u16::from_be_bytes(bytes.get(2..4)?.try_into().ok()?);
      let ip: [u8; 4] = bytes.get(4..8)?.try_into().ok()?;
      // the kernel wants all 16 bytes
      (bytes.len() >= 16)
        .then(|| Target::Ip(SocketAddr::new(Ipv4Addr::from(ip).into(), port)))
    }
    libc::AF_INET6 => {
      // sockaddr_in6: family, port, flowinfo, address, scope id (the
      // kernel takes the 24 bytes up to the scope id as well)
      let port = u16::from_be_bytes(bytes.get(2..4)?.try_into().ok()?);
      let ip: [u8; 16] = bytes.get(8..24)?.try_into().ok()?;
      Some(Target::Ip(SocketAddr::new(Ipv6Addr::from(ip).into(), port)))
    }
    _ => Some(Target::Other(family)),
  }
}

/// An address as the gate compares it: an IPv4-mapped IPv6 address is the
/// IPv4 address it reaches.
fn canonical(addr: SocketAddr) -> SocketAddr {
  match addr.ip() {
    IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
      Some(v4) => SocketAddr::new(v4.into(), addr.port()),
      None => addr,
    },
    IpAddr::V4(_) => addr,
  }
}

impl ConnectGate {
  /// A gate that lets `connect` reach exactly `allow` (compared with
  /// IPv4-mapped addresses as their IPv4 form).
  pub fn new(allow: impl IntoIterator<Item = SocketAddr>) -> Self {
    Self {
      allow: allow.into_iter().map(canonical).collect(),
    }
  }

  /// Whether `call` is one this gate answers.
  pub fn handles(&self, call: &Call) -> bool {
    call.nr == NR_CONNECT
  }

  fn allows(&self, addr: SocketAddr) -> bool {
    self.allow.contains(&canonical(addr))
  }

  /// The answer to `connect(fd, addr, addrlen)`: never
  /// [`Answer::Continue`].
  pub fn answer(&self, call: &Call) -> Answer {
    let refuse = |errno: i32| Answer::Done(Err(errno));
    if call.nr != NR_CONNECT {
      return refuse(libc::ENOSYS);
    }
    let fd = call.args[0] as u32 as i32;
    let len = call.args[2] as u32 as usize;
    if len < 2 {
      return refuse(libc::EINVAL);
    }
    let mut copy = [0u8; MAX_SOCKADDR];
    let len = len.min(MAX_SOCKADDR);
    // the one read of the caller's address
    if let Err(errno) = read_exact(call.tid, call.args[1], &mut copy[..len]) {
      return refuse(errno);
    }
    let Some(target) = parse_sockaddr(&copy[..len]) else {
      return refuse(libc::EINVAL);
    };
    match target {
      Target::Ip(addr) if self.allows(addr) => {}
      Target::Ip(_) | Target::Unspecified | Target::Other(_) => {
        return refuse(libc::EACCES);
      }
    }
    let socket = match steal_fd(call.tid, fd) {
      Ok(socket) => socket,
      Err(errno) => return refuse(errno),
    };
    // the connection is made here, to the address that was checked
    // SAFETY: a valid socket and `len` initialized bytes of an address
    let res = unsafe {
      libc::connect(
        socket.as_raw_fd(),
        copy.as_ptr().cast::<libc::sockaddr>(),
        len as libc::socklen_t,
      )
    };
    if res == 0 {
      Answer::Done(Ok(0))
    } else {
      refuse(errno())
    }
  }
}

#[cfg(test)]
mod tests {
  use std::io::Read;
  use std::io::Write;
  use std::net::TcpListener;
  use std::os::fd::FromRawFd;
  use std::os::fd::OwnedFd;
  use std::sync::atomic::AtomicBool;
  use std::sync::atomic::Ordering;

  use super::*;
  use crate::notify::notify_program;
  use crate::seccomp::SockFilter;
  use crate::seccomp::SockFprog;

  fn sockaddr_in(addr: [u8; 4], port: u16) -> Vec<u8> {
    let mut bytes = vec![0u8; 16];
    bytes[..2].copy_from_slice(&(libc::AF_INET as u16).to_ne_bytes());
    bytes[2..4].copy_from_slice(&port.to_be_bytes());
    bytes[4..8].copy_from_slice(&addr);
    bytes
  }

  fn sockaddr_in6(addr: Ipv6Addr, port: u16) -> Vec<u8> {
    let mut bytes = vec![0u8; 28];
    bytes[..2].copy_from_slice(&(libc::AF_INET6 as u16).to_ne_bytes());
    bytes[2..4].copy_from_slice(&port.to_be_bytes());
    bytes[8..24].copy_from_slice(&addr.octets());
    bytes
  }

  #[test]
  fn sockaddrs_parse_as_the_kernel_reads_them() {
    assert_eq!(
      parse_sockaddr(&sockaddr_in([127, 0, 0, 1], 8080)),
      Some(Target::Ip("127.0.0.1:8080".parse().unwrap()))
    );
    assert_eq!(
      parse_sockaddr(&sockaddr_in6(Ipv6Addr::LOCALHOST, 443)),
      Some(Target::Ip("[::1]:443".parse().unwrap()))
    );
    // AF_UNSPEC is two bytes of zero, whatever follows
    assert_eq!(parse_sockaddr(&[0, 0]), Some(Target::Unspecified));
    assert_eq!(parse_sockaddr(&[0u8; 16]), Some(Target::Unspecified));
    // a Unix socket
    let mut unix = vec![0u8; 20];
    unix[..2].copy_from_slice(&(libc::AF_UNIX as u16).to_ne_bytes());
    assert_eq!(
      parse_sockaddr(&unix),
      Some(Target::Other(libc::AF_UNIX as u16))
    );
    // truncated addresses aren't guessed at
    assert_eq!(parse_sockaddr(&sockaddr_in([127, 0, 0, 1], 80)[..8]), None);
    assert_eq!(
      parse_sockaddr(&sockaddr_in6(Ipv6Addr::LOCALHOST, 80)[..20]),
      None
    );
    assert_eq!(parse_sockaddr(&[2]), None);
  }

  #[test]
  fn only_listed_addresses_pass() {
    let gate = ConnectGate::new(["127.0.0.1:3128".parse().unwrap()]);
    assert!(gate.allows("127.0.0.1:3128".parse().unwrap()));
    // the IPv4-mapped spelling of the same address
    assert!(gate.allows("[::ffff:127.0.0.1]:3128".parse().unwrap()));
    for other in [
      "127.0.0.1:3129",
      "127.0.0.2:3128",
      "[::1]:3128",
      "93.184.216.34:3128",
      "169.254.169.254:80",
      "0.0.0.0:3128",
    ] {
      assert!(!gate.allows(other.parse().unwrap()), "{other}");
    }
    assert!(!ConnectGate::default().allows("127.0.0.1:1".parse().unwrap()));
  }

  /// What a gated child reports for one attempt: the errno of its
  /// `connect` (0: connected).
  fn attempt(sock: libc::c_int, addr: &[u8]) -> i32 {
    // SAFETY: a socket and an initialized address of the given length
    let res = unsafe {
      libc::connect(
        sock,
        addr.as_ptr().cast::<libc::sockaddr>(),
        addr.len() as libc::socklen_t,
      )
    };
    if res == 0 { 0 } else { errno() }
  }

  /// Forks a child that installs the notifying filter for `connect`, hands
  /// its listener over, then runs `attempts` (async-signal-safe only) and
  /// writes their errnos to a pipe. Returns the errnos as the gate's
  /// supervisor saw them through.
  fn run_gated(
    gate: &ConnectGate,
    attempts: &dyn Fn(&mut dyn FnMut(i32)),
  ) -> Vec<i32> {
    let program = notify_program(&connect_syscalls());
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
          1 as libc::c_ulong,        // SECCOMP_SET_MODE_FILTER
          (1 << 3) as libc::c_ulong, // SECCOMP_FILTER_FLAG_NEW_LISTENER
          &prog as *const SockFprog,
        );
        if listener < 0 {
          libc::_exit(3);
        }
        if crate::linux::send_fd(pair[1], listener as libc::c_int).is_err() {
          libc::_exit(4);
        }
        libc::close(listener as libc::c_int);
        let mut report = |errno: i32| {
          libc::write(out[1], (&errno as *const i32).cast(), 4);
        };
        attempts(&mut report);
        libc::_exit(0);
      }
    }
    assert!(pid > 0);
    // SAFETY: our ends of the pair and the pipe
    let (parent, reader) = unsafe {
      libc::close(pair[1]);
      libc::close(out[1]);
      (OwnedFd::from_raw_fd(pair[0]), OwnedFd::from_raw_fd(out[0]))
    };
    let listener = crate::linux::recv_fd(&parent).expect("the listener");
    let stop = AtomicBool::new(false);
    let mut results = Vec::new();
    std::thread::scope(|scope| {
      scope.spawn(|| serve(gate, listener, &stop));
      let mut bytes = Vec::new();
      std::fs::File::from(reader).read_to_end(&mut bytes).unwrap();
      for chunk in bytes.chunks_exact(4) {
        results.push(i32::from_ne_bytes(chunk.try_into().unwrap()));
      }
      stop.store(true, Ordering::Relaxed);
    });
    let mut status = 0;
    // SAFETY: our own child, reaped once
    unsafe { libc::waitpid(pid, &mut status, 0) };
    assert!(
      libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
      "the gated child failed: {status:#x}"
    );
    results
  }

  /// The supervisor's loop for a gate alone (what the broker's supervisor
  /// does for the `connect` notifications among its own).
  fn serve(gate: &ConnectGate, listener: OwnedFd, stop: &AtomicBool) {
    let fd = listener.as_raw_fd();
    loop {
      let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
      };
      // SAFETY: one pollfd
      let ready = unsafe { libc::poll(&mut poll, 1, 50) };
      if ready == 0 {
        if stop.load(Ordering::Relaxed) {
          return;
        }
        continue;
      }
      if ready < 0 || poll.revents & libc::POLLIN == 0 {
        return;
      }
      // SAFETY: zeroed request and response the kernel fills / reads
      unsafe {
        let mut req: libc::seccomp_notif = std::mem::zeroed();
        if libc::ioctl(fd, libc::SECCOMP_IOCTL_NOTIF_RECV as _, &mut req) != 0 {
          continue;
        }
        let answer = gate.answer(&Call {
          tid: req.pid,
          nr: req.data.nr as u32,
          args: req.data.args,
        });
        let mut resp = libc::seccomp_notif_resp {
          id: req.id,
          val: 0,
          error: 0,
          flags: 0,
        };
        match answer {
          Answer::Continue => unreachable!("the gate never continues"),
          Answer::Done(Ok(val)) => resp.val = val,
          Answer::Done(Err(errno)) => resp.error = -errno,
        }
        libc::ioctl(fd, libc::SECCOMP_IOCTL_NOTIF_SEND as _, &resp);
      }
    }
  }

  fn tcp_socket() -> libc::c_int {
    // SAFETY: a plain socket call
    unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) }
  }

  /// Whether this host lets a process supervise a child's syscalls and
  /// duplicate its descriptors (a container's seccomp profile keeps
  /// `pidfd_getfd` for CAP_SYS_PTRACE; a ptrace policy may refuse too): a
  /// refusal and an allowed connection both come out as the gate decides.
  fn supervision_works() -> bool {
    let Ok(listener) = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) else {
      return false;
    };
    let port = listener.local_addr().unwrap().port();
    let gate =
      ConnectGate::new([SocketAddr::from((Ipv4Addr::LOCALHOST, port))]);
    let allowed = sockaddr_in([127, 0, 0, 1], port);
    let refused = sockaddr_in([127, 0, 0, 1], 9);
    let results = std::panic::catch_unwind(|| {
      run_gated(&gate, &|report| {
        report(attempt(tcp_socket(), &refused));
        report(attempt(tcp_socket(), &allowed));
      })
    });
    matches!(results.as_deref(), Ok([e, 0]) if *e == libc::EACCES)
  }

  #[test]
  fn a_gated_tree_reaches_only_the_listed_address() {
    if !supervision_works() {
      eprintln!("skipped: seccomp user notification is not usable here");
      return;
    }
    let proxy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let proxy_port = proxy.local_addr().unwrap().port();
    let other = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let other_port = other.local_addr().unwrap().port();
    let gate =
      ConnectGate::new([SocketAddr::from((Ipv4Addr::LOCALHOST, proxy_port))]);

    let to_proxy = sockaddr_in([127, 0, 0, 1], proxy_port);
    let to_other = sockaddr_in([127, 0, 0, 1], other_port);
    let to_public = sockaddr_in([93, 184, 216, 34], proxy_port);
    let to_metadata = sockaddr_in([169, 254, 169, 254], 80);
    let unspec = vec![0u8; 16];
    let results = run_gated(&gate, &|report| {
      // 0: the proxy
      let connected = tcp_socket();
      report(attempt(connected, &to_proxy));
      // 1-3: another port, another host on the proxy's port, metadata
      report(attempt(tcp_socket(), &to_other));
      report(attempt(tcp_socket(), &to_public));
      report(attempt(tcp_socket(), &to_metadata));
      // 4: dissolving the proxy connection; 5: so it can't be re-pointed
      report(attempt(connected, &unspec));
      report(attempt(connected, &to_other));
      // 6: the connection is still the proxy's
      // SAFETY: a connected socket and a one-byte buffer
      report(unsafe {
        if libc::write(connected, b"x".as_ptr().cast(), 1) == 1 {
          0
        } else {
          errno()
        }
      });
      // 7: a descriptor that is no socket
      report(attempt(-1, &to_proxy));
    });
    assert_eq!(results.len(), 8, "{results:?}");
    assert_eq!(results[0], 0, "the proxy connects");
    assert_eq!(results[1], libc::EACCES, "another port");
    assert_eq!(results[2], libc::EACCES, "another host on the proxy's port");
    assert_eq!(results[3], libc::EACCES, "the metadata address");
    assert_eq!(results[4], libc::EACCES, "AF_UNSPEC");
    // refused by the gate before the kernel could say EISCONN
    assert_eq!(results[5], libc::EACCES, "re-pointing");
    assert_eq!(results[6], 0, "still connected to the proxy");
    assert_eq!(results[7], libc::EBADF, "not a descriptor");
    // the proxy got the connection and its byte; the other listener nothing
    let (mut accepted, _) = proxy.accept().unwrap();
    let mut byte = [0u8; 1];
    accepted.read_exact(&mut byte).unwrap();
    assert_eq!(&byte, b"x");
    other.set_nonblocking(true).unwrap();
    assert!(other.accept().is_err());
    let _ = accepted.write_all(b"y");
  }

  #[test]
  fn a_non_blocking_connect_reports_in_progress_or_done() {
    if !supervision_works() {
      eprintln!("skipped: seccomp user notification is not usable here");
      return;
    }
    let proxy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = proxy.local_addr().unwrap().port();
    let gate =
      ConnectGate::new([SocketAddr::from((Ipv4Addr::LOCALHOST, port))]);
    let to_proxy = sockaddr_in([127, 0, 0, 1], port);
    let results = run_gated(&gate, &|report| {
      // SAFETY: plain socket calls
      let sock = unsafe {
        libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_NONBLOCK, 0)
      };
      report(attempt(sock, &to_proxy));
    });
    // loopback completes at once on most kernels; either is a connection
    // under way that the caller's own descriptor owns
    assert!(
      results == [0] || results == [libc::EINPROGRESS],
      "{results:?}"
    );
    proxy.accept().unwrap();
  }

  #[test]
  fn nothing_listed_nothing_reached() {
    if !supervision_works() {
      eprintln!("skipped: seccomp user notification is not usable here");
      return;
    }
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let to = sockaddr_in([127, 0, 0, 1], port);
    let to6 = sockaddr_in6(Ipv6Addr::LOCALHOST, port);
    let results = run_gated(&ConnectGate::default(), &|report| {
      report(attempt(tcp_socket(), &to));
      // SAFETY: a plain socket call
      let v6 = unsafe { libc::socket(libc::AF_INET6, libc::SOCK_STREAM, 0) };
      report(attempt(v6, &to6));
    });
    assert_eq!(results, [libc::EACCES, libc::EACCES]);
  }
}
