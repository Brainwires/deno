// Copyright 2018-2026 the Deno authors. MIT license.

//! The jail's OS confinement as the shared enforcers in `deno_sandbox`
//! produce it from the jail's policy: the seccomp program and the Seatbelt
//! profile are pinned byte for byte to what the jail emitted before the
//! enforcers moved out of `cli/supply_chain` (golden tests), and the seccomp
//! program keeps its semantics (DENEXT.md items 111 and 124).
//!
//! The golden values were produced by the code at 7d5719d5d4 (the jail's own
//! `seccomp_program(OsNet)` and `sandbox_macos::profile`), built for x86_64
//! and aarch64. A deliberate change to the jail's rules changes them: then
//! regenerate them from the new program, and say why in the commit.

use std::path::PathBuf;

use super::jail::OsNet;

/// The jail's inputs the Seatbelt goldens were taken with, in order.
fn seatbelt_cases() -> Vec<(Vec<PathBuf>, Vec<PathBuf>, OsNet)> {
  vec![
    (
      vec![PathBuf::from("/nm/pkg"), PathBuf::from("/nm/.deno/.jail/x")],
      vec![
        PathBuf::from("/home/u/.ssh"),
        PathBuf::from("/home/u/.aws"),
        PathBuf::from("/p/.env"),
      ],
      OsNet::Denied,
    ),
    (
      vec![PathBuf::from("/nm/pkg")],
      vec![PathBuf::from("/home/u/.npmrc")],
      OsNet::Open,
    ),
    (
      vec![PathBuf::from("/nm/pkg"), PathBuf::from("/tmp/a \"b\"/c\\d")],
      vec![],
      OsNet::ProxyOnly(4321),
    ),
    (vec![], vec![], OsNet::Denied),
  ]
}

const SEATBELT_GOLDEN: [&str; 4] = [
  // denied
  concat!(
    "(version 1)\n",
    "(deny default)\n",
    "(allow file-read*)\n",
    "(deny file-read* (literal \"/dev/tty\") (regex #\"^/dev/ttys[0-9]+$\") (regex #\"^/dev/pty\") (subpath \"/home/u/.ssh\") (subpath \"/home/u/.aws\") (subpath \"/p/.env\"))\n",
    "(allow file-write* (subpath \"/nm/pkg\") (subpath \"/nm/.deno/.jail/x\") (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/dtracehelper\") (literal \"/dev/stdout\") (literal \"/dev/stderr\") (regex #\"^/dev/fd/\"))\n",
    "(allow file-link (subpath \"/nm/pkg\") (subpath \"/nm/.deno/.jail/x\"))\n",
    "(allow file-ioctl)\n",
    "(deny file-ioctl (literal \"/dev/tty\") (regex #\"^/dev/ttys[0-9]+$\") (regex #\"^/dev/pty\"))\n",
    "(allow process-fork)\n",
    "(allow process-exec)\n",
    "(allow signal (target same-sandbox))\n",
    "(allow process-info* (target same-sandbox))\n",
    "(allow sysctl-read)\n",
    "(deny sysctl-read (sysctl-name-prefix \"kern.proc\"))\n",
    "(allow ipc-posix-sem)\n",
    "(allow ipc-posix-shm*)\n",
    "(allow mach-lookup (global-name \"com.apple.system.opendirectoryd.libinfo\") (global-name \"com.apple.system.opendirectoryd.membership\") (global-name \"com.apple.bsd.dirhelper\") (global-name \"com.apple.system.logger\") (global-name \"com.apple.logd\") (global-name \"com.apple.system.notification_center\"))\n",
    "(deny mach-lookup (global-name \"com.apple.SecurityServer\") (global-name \"com.apple.securityd\") (global-name \"com.apple.securityd.xpc\") (global-name \"com.apple.trustd\") (global-name \"com.apple.trustd.agent\") (global-name \"com.apple.coreservices.launchservicesd\") (global-name \"com.apple.lsd.mapdb\") (global-name \"com.apple.lsd.modifydb\") (global-name \"com.apple.pasteboard.1\") (global-name \"com.apple.tccd\") (global-name \"com.apple.tccd.system\") (global-name \"com.apple.dnssd.service\"))\n",
    "(deny lsopen)\n",
    "(deny appleevent-send)\n",
    "(deny job-creation)\n",
    "(deny network*)\n",
  ),
  // open
  concat!(
    "(version 1)\n",
    "(deny default)\n",
    "(allow file-read*)\n",
    "(deny file-read* (literal \"/dev/tty\") (regex #\"^/dev/ttys[0-9]+$\") (regex #\"^/dev/pty\") (subpath \"/home/u/.npmrc\"))\n",
    "(allow file-write* (subpath \"/nm/pkg\") (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/dtracehelper\") (literal \"/dev/stdout\") (literal \"/dev/stderr\") (regex #\"^/dev/fd/\"))\n",
    "(allow file-link (subpath \"/nm/pkg\"))\n",
    "(allow file-ioctl)\n",
    "(deny file-ioctl (literal \"/dev/tty\") (regex #\"^/dev/ttys[0-9]+$\") (regex #\"^/dev/pty\"))\n",
    "(allow process-fork)\n",
    "(allow process-exec)\n",
    "(allow signal (target same-sandbox))\n",
    "(allow process-info* (target same-sandbox))\n",
    "(allow sysctl-read)\n",
    "(deny sysctl-read (sysctl-name-prefix \"kern.proc\"))\n",
    "(allow ipc-posix-sem)\n",
    "(allow ipc-posix-shm*)\n",
    "(allow mach-lookup (global-name \"com.apple.system.opendirectoryd.libinfo\") (global-name \"com.apple.system.opendirectoryd.membership\") (global-name \"com.apple.bsd.dirhelper\") (global-name \"com.apple.system.logger\") (global-name \"com.apple.logd\") (global-name \"com.apple.system.notification_center\"))\n",
    "(deny mach-lookup (global-name \"com.apple.SecurityServer\") (global-name \"com.apple.securityd\") (global-name \"com.apple.securityd.xpc\") (global-name \"com.apple.trustd\") (global-name \"com.apple.trustd.agent\") (global-name \"com.apple.coreservices.launchservicesd\") (global-name \"com.apple.lsd.mapdb\") (global-name \"com.apple.lsd.modifydb\") (global-name \"com.apple.pasteboard.1\") (global-name \"com.apple.tccd\") (global-name \"com.apple.tccd.system\") (global-name \"com.apple.dnssd.service\"))\n",
    "(deny lsopen)\n",
    "(deny appleevent-send)\n",
    "(deny job-creation)\n",
    "(allow network*)\n",
    "(deny network-outbound (remote unix-socket))\n",
    "(allow network-outbound (remote unix-socket (path-literal \"/private/var/run/mDNSResponder\")))\n",
    "(allow mach-lookup (global-name \"com.apple.dnssd.service\"))\n",
  ),
  // proxy
  concat!(
    "(version 1)\n",
    "(deny default)\n",
    "(allow file-read*)\n",
    "(deny file-read* (literal \"/dev/tty\") (regex #\"^/dev/ttys[0-9]+$\") (regex #\"^/dev/pty\"))\n",
    "(allow file-write* (subpath \"/nm/pkg\") (subpath \"/tmp/a \\\"b\\\"/c\\\\d\") (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/dtracehelper\") (literal \"/dev/stdout\") (literal \"/dev/stderr\") (regex #\"^/dev/fd/\"))\n",
    "(allow file-link (subpath \"/nm/pkg\") (subpath \"/tmp/a \\\"b\\\"/c\\\\d\"))\n",
    "(allow file-ioctl)\n",
    "(deny file-ioctl (literal \"/dev/tty\") (regex #\"^/dev/ttys[0-9]+$\") (regex #\"^/dev/pty\"))\n",
    "(allow process-fork)\n",
    "(allow process-exec)\n",
    "(allow signal (target same-sandbox))\n",
    "(allow process-info* (target same-sandbox))\n",
    "(allow sysctl-read)\n",
    "(deny sysctl-read (sysctl-name-prefix \"kern.proc\"))\n",
    "(allow ipc-posix-sem)\n",
    "(allow ipc-posix-shm*)\n",
    "(allow mach-lookup (global-name \"com.apple.system.opendirectoryd.libinfo\") (global-name \"com.apple.system.opendirectoryd.membership\") (global-name \"com.apple.bsd.dirhelper\") (global-name \"com.apple.system.logger\") (global-name \"com.apple.logd\") (global-name \"com.apple.system.notification_center\"))\n",
    "(deny mach-lookup (global-name \"com.apple.SecurityServer\") (global-name \"com.apple.securityd\") (global-name \"com.apple.securityd.xpc\") (global-name \"com.apple.trustd\") (global-name \"com.apple.trustd.agent\") (global-name \"com.apple.coreservices.launchservicesd\") (global-name \"com.apple.lsd.mapdb\") (global-name \"com.apple.lsd.modifydb\") (global-name \"com.apple.pasteboard.1\") (global-name \"com.apple.tccd\") (global-name \"com.apple.tccd.system\") (global-name \"com.apple.dnssd.service\"))\n",
    "(deny lsopen)\n",
    "(deny appleevent-send)\n",
    "(deny job-creation)\n",
    "(deny network*)\n",
    "(allow network-outbound (remote ip \"localhost:4321\"))\n",
  ),
  // empty
  concat!(
    "(version 1)\n",
    "(deny default)\n",
    "(allow file-read*)\n",
    "(deny file-read* (literal \"/dev/tty\") (regex #\"^/dev/ttys[0-9]+$\") (regex #\"^/dev/pty\"))\n",
    "(allow file-write* (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/dtracehelper\") (literal \"/dev/stdout\") (literal \"/dev/stderr\") (regex #\"^/dev/fd/\"))\n",
    "(allow file-ioctl)\n",
    "(deny file-ioctl (literal \"/dev/tty\") (regex #\"^/dev/ttys[0-9]+$\") (regex #\"^/dev/pty\"))\n",
    "(allow process-fork)\n",
    "(allow process-exec)\n",
    "(allow signal (target same-sandbox))\n",
    "(allow process-info* (target same-sandbox))\n",
    "(allow sysctl-read)\n",
    "(deny sysctl-read (sysctl-name-prefix \"kern.proc\"))\n",
    "(allow ipc-posix-sem)\n",
    "(allow ipc-posix-shm*)\n",
    "(allow mach-lookup (global-name \"com.apple.system.opendirectoryd.libinfo\") (global-name \"com.apple.system.opendirectoryd.membership\") (global-name \"com.apple.bsd.dirhelper\") (global-name \"com.apple.system.logger\") (global-name \"com.apple.logd\") (global-name \"com.apple.system.notification_center\"))\n",
    "(deny mach-lookup (global-name \"com.apple.SecurityServer\") (global-name \"com.apple.securityd\") (global-name \"com.apple.securityd.xpc\") (global-name \"com.apple.trustd\") (global-name \"com.apple.trustd.agent\") (global-name \"com.apple.coreservices.launchservicesd\") (global-name \"com.apple.lsd.mapdb\") (global-name \"com.apple.lsd.modifydb\") (global-name \"com.apple.pasteboard.1\") (global-name \"com.apple.tccd\") (global-name \"com.apple.tccd.system\") (global-name \"com.apple.dnssd.service\"))\n",
    "(deny lsopen)\n",
    "(deny appleevent-send)\n",
    "(deny job-creation)\n",
    "(deny network*)\n",
  ),
];

// the profile text is identical for the jail's inputs (built and checked on
// every platform: generating it is string building)
#[test]
fn jail_seatbelt_profile_golden() {
  for ((write, deny_read, net), golden) in
    seatbelt_cases().into_iter().zip(SEATBELT_GOLDEN)
  {
    let profile = deno_sandbox::seatbelt::profile(&write, &deny_read, net);
    assert_eq!(profile, golden, "{net:?}");
  }
}

#[cfg(target_os = "linux")]
mod linux {
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  use deno_sandbox::seccomp::*;

  use super::super::jail::OsNet;
  use super::super::jail::seccomp_spec;

  fn seccomp_program(net: OsNet) -> Vec<u8> {
    seccomp_spec(net).program()
  }

  #[cfg(target_arch = "x86_64")]
  const SECCOMP_GOLDEN: [(&str, &str); 3] = [
    (
      "denied",
      concat!(
        "2000000004000000150001003e0000c006000000000000802000000000000000",
        "3500000100000040060000000000008015000001a90100000600000026000500",
        "150000051000000020000000180000001500010012540000150000011c540000",
        "060000000100050020000000000000001500010029000000060000000000ff7f",
        "20000000100000001500000110000000060000000000ff7f060000000d000500",
      ),
    ),
    (
      "open",
      concat!(
        "2000000004000000150001003e0000c006000000000000802000000000000000",
        "3500000100000040060000000000008015000001a90100000600000026000500",
        "150000051000000020000000180000001500010012540000150000011c540000",
        "060000000100050020000000000000001500010029000000060000000000ff7f",
        "20000000100000001500000110000000060000000000ff7f1500010002000000",
        "150000010a000000060000000000ff7f060000000d000500",
      ),
    ),
    (
      "proxy",
      concat!(
        "2000000004000000150001003e0000c006000000000000802000000000000000",
        "3500000100000040060000000000008015000001a90100000600000026000500",
        "150000051000000020000000180000001500010012540000150000011c540000",
        "06000000010005002000000000000000150000052c0000002000000028000000",
        "54000000000000201500000100000000060000000000ff7f060000005f000500",
        "1500000533010000200000002800000054000000000000201500000100000000",
        "060000000000ff7f060000005f000500150000052e0000002000000020000000",
        "54000000000000201500000100000000060000000000ff7f060000005f000500",
        "1500010029000000060000000000ff7f20000000100000001500000110000000",
        "060000000000ff7f1500010002000000150000070a0000002000000018000000",
        "540000000f000000150000040100000020000000200000001500010000000000",
        "1500000106000000060000000000ff7f060000000d000500",
      ),
    ),
  ];

  #[cfg(target_arch = "aarch64")]
  const SECCOMP_GOLDEN: [(&str, &str); 3] = [
    (
      "denied",
      concat!(
        "200000000400000015000100b70000c006000000000000802000000000000000",
        "15000001a90100000600000026000500150000051d0000002000000018000000",
        "1500010012540000150000011c54000006000000010005002000000000000000",
        "15000100c6000000060000000000ff7f20000000100000001500000110000000",
        "060000000000ff7f060000000d000500",
      ),
    ),
    (
      "open",
      concat!(
        "200000000400000015000100b70000c006000000000000802000000000000000",
        "15000001a90100000600000026000500150000051d0000002000000018000000",
        "1500010012540000150000011c54000006000000010005002000000000000000",
        "15000100c6000000060000000000ff7f20000000100000001500000110000000",
        "060000000000ff7f1500010002000000150000010a000000060000000000ff7f",
        "060000000d000500",
      ),
    ),
    (
      "proxy",
      concat!(
        "200000000400000015000100b70000c006000000000000802000000000000000",
        "15000001a90100000600000026000500150000051d0000002000000018000000",
        "1500010012540000150000011c54000006000000010005002000000000000000",
        "15000005ce000000200000002800000054000000000000201500000100000000",
        "060000000000ff7f060000005f000500150000050d0100002000000028000000",
        "54000000000000201500000100000000060000000000ff7f060000005f000500",
        "15000005d3000000200000002000000054000000000000201500000100000000",
        "060000000000ff7f060000005f00050015000100c6000000060000000000ff7f",
        "20000000100000001500000110000000060000000000ff7f1500010002000000",
        "150000070a0000002000000018000000540000000f0000001500000401000000",
        "200000002000000015000100000000001500000106000000060000000000ff7f",
        "060000000d000500",
      ),
    ),
  ];

  fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
  }

  // the program the SeccompSpec builder assembles from the jail's rules is
  // the one the jail emitted before the enforcers moved, byte for byte
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  #[test]
  fn jail_seccomp_program_golden_bytes() {
    for (net, (name, golden)) in [
      OsNet::Denied,
      OsNet::Open,
      // the proxy's port has no part in the program
      OsNet::ProxyOnly(4321),
    ]
    .into_iter()
    .zip(SECCOMP_GOLDEN)
    {
      assert_eq!(hex(&seccomp_program(net)), golden, "{name}");
    }
    assert_eq!(
      seccomp_program(OsNet::ProxyOnly(1)),
      seccomp_program(OsNet::ProxyOnly(65535))
    );
  }

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

  // DENEXT.md item 124 (H2): typing into a terminal is refused, other
  // ioctls and the syscalls after the check still get their own answers
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  #[test]
  fn seccomp_refuses_tiocsti() {
    let eperm = SECCOMP_RET_ERRNO | libc::EPERM as u32;
    let eacces = SECCOMP_RET_ERRNO | libc::EACCES as u32;
    for net in [OsNet::Denied, OsNet::Open, OsNet::ProxyOnly(1)] {
      let program = seccomp_program(net);
      assert_eq!(
        run_filter(&program, NR_IOCTL, [0, TIOCSTI, 0, 0, 0, 0]),
        eperm
      );
      assert_eq!(
        run_filter(&program, NR_IOCTL, [5, TIOCLINUX, 0, 0, 0, 0]),
        eperm
      );
      // TIOCGWINSZ, FIONBIO: fine
      assert_eq!(
        run_filter(&program, NR_IOCTL, [0, 0x5413, 0, 0, 0, 0]),
        SECCOMP_RET_ALLOW
      );
      assert_eq!(
        run_filter(&program, NR_IOCTL, [0, 0x5421, 0, 0, 0, 0]),
        SECCOMP_RET_ALLOW
      );
      // the check doesn't disturb what follows it
      assert_eq!(
        run_filter(&program, NR_SOCKET, [libc::AF_UNIX as u32, 1, 0, 0, 0, 0]),
        eacces
      );
    }
  }
}

// a built-in exception's hosts open their HTTPS port only: no other port,
// by CONNECT or forwarded, while a project's own `host` stays any port
// (moved here from the proxy's own tests: the built-in list is the jail's)
#[tokio::test]
async fn builtin_hosts_only_on_443() {
  use std::net::Ipv4Addr;

  use deno_sandbox::proxy::AllowList;
  use deno_sandbox::proxy::JailProxy;
  use deno_sandbox::proxy::ProxyHost;
  use deno_sandbox::proxy::Upstream;
  use tokio::io::AsyncReadExt;
  use tokio::io::AsyncWriteExt;

  use super::builtin_exceptions::builtin_exception;

  fn host(text: &str) -> ProxyHost {
    ProxyHost::parse(text).unwrap()
  }

  async fn roundtrip(proxy_port: u16, request: &str) -> String {
    let mut stream =
      tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, proxy_port))
        .await
        .unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    stream.read_to_end(&mut out).await.unwrap();
    String::from_utf8_lossy(&out).into_owned()
  }

  for name in ["better-sqlite3", "@tensorflow/tfjs-node", "chromedriver"] {
    let hosts = builtin_exception(name).unwrap().host_list();
    let allow = AllowList::parse(&hosts).unwrap();
    assert_eq!(allow.entries().len(), hosts.len());
    for (entry, text) in allow.entries().iter().zip(&hosts) {
      assert!(allow.allows(&entry.host, 443), "{text}");
      for port in [22, 80, 8443] {
        assert!(!allow.allows(&entry.host, port), "{text} {port}");
      }
    }
  }
  let allow =
    AllowList::parse(&builtin_exception("sharp").unwrap().host_list()).unwrap();
  let proxy = JailProxy::start(allow, Upstream::default()).await.unwrap();
  for request in [
    "CONNECT github.com:22 HTTP/1.1\r\nHost: github.com:22\r\n\r\n",
    "CONNECT objects.githubusercontent.com:8443 HTTP/1.1\r\n\r\n",
    "GET http://nodejs.org/x HTTP/1.1\r\nHost: nodejs.org\r\n\r\n",
  ] {
    let response = roundtrip(proxy.port(), request).await;
    assert!(
      response.starts_with("HTTP/1.1 403 "),
      "{request}: {response}"
    );
  }
  // the project's own `github.com` keeps meaning every port
  let own = AllowList::parse(&["github.com".to_string()]).unwrap();
  assert!(own.allows(&host("github.com"), 22));
}
