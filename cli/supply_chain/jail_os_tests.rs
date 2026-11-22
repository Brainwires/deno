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
//!
//! The `denied` and `proxy` seccomp goldens were regenerated on the train
//! for DENEXT.md item 419 (GHSA-9363-h8qp-jg52): a `Deny` of `listen(2)`
//! after the `ioctl` rule when the network isn't fully open. The `open` program and the Seatbelt
//! profiles are unchanged.
//!
//! All three seccomp goldens were regenerated on the denext.25 train for
//! DENEXT.md item 425: after the existing rules, one rule keeps netlink
//! sockets to `NETLINK_ROUTE`, one refuses datagram socket pairs, and `Deny`
//! rules refuse `ptrace`, `process_vm_readv`, `process_vm_writev` and
//! `pidfd_getfd`. They come from a model of the assembler that reproduces the
//! previous goldens for both architectures byte for byte. The Seatbelt
//! profiles are unchanged.
//!
//! And again for DENEXT.md item 427: two `DenyArgIn` rules after those,
//! `fcntl` `F_SETOWN` / `F_SETOWN_EX` and the `ioctl`s `FIOSETOWN`,
//! `SIOCSPGRP`, `FS_IOC_SETFLAGS`, `FS_IOC32_SETFLAGS`, `FS_IOC_FSSETXATTR`
//! (from the same model). The rules for kernels before Landlock ABI 3 are
//! not in these programs (`seccomp_spec` is the ABI 3+ one).

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

// macOS: a read deny holds whatever case a script spells the path in, and
// whatever case the deny was written in, on a case-insensitive volume
// (Seatbelt matches the file's path, not the text the script passed);
// a later macOS that matched the spelling would fail here
#[cfg(target_os = "macos")]
#[test]
#[allow(clippy::print_stderr, reason = "test code: a skip note")]
fn jail_seatbelt_deny_holds_in_any_case() {
  if super::sandbox_macos::available().is_err() {
    eprintln!("skipped: sandbox-exec doesn't work here");
    return;
  }
  let dir = tempfile::tempdir().unwrap();
  let real = crate::util::fs::canonicalize_path(dir.path()).unwrap();
  let secret = real.join("Secret");
  std::fs::create_dir(&secret).unwrap();
  std::fs::write(secret.join("key"), "x").unwrap();
  if !real.join("SECRET").exists() {
    eprintln!("skipped: a case-sensitive volume");
    return;
  }
  let read = |deny: &str, path: &str| {
    let profile =
      deno_sandbox::seatbelt::profile(&[], &[real.join(deny)], OsNet::Denied);
    std::process::Command::new(super::sandbox_macos::SANDBOX_EXEC)
      .arg("-p")
      .arg(profile)
      .arg("/bin/cat")
      .arg(real.join(path))
      .output()
      .unwrap()
      .status
      .success()
  };
  // the deny itself works, and nothing else is denied
  assert!(!read("Secret", "Secret/key"));
  assert!(read("Other", "Secret/key"));
  for path in ["SECRET/key", "secret/KEY", "sEcReT/key"] {
    assert!(!read("Secret", path), "{path}");
  }
  assert!(!read("SECRET", "Secret/key"));
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
        "060000000100050020000000000000001500000132000000060000000d000500",
        "1500000829000000200000001000000054000000ffffffff1500000410000000",
        "200000002000000054000000ffffffff1500010000000000060000000d000500",
        "200000000000000015000005350000002000000018000000540000000f000000",
        "1500000102000000060000000d00050020000000000000001500000165000000",
        "0600000001000500150000013601000006000000010005001500000137010000",
        "060000000100050015000001b601000006000000010005001500000548000000",
        "20000000180000001500010008000000150000010f0000000600000001000500",
        "2000000000000000150000081000000020000000180000001500040001890000",
        "1500030002890000150002000266084015000100026604401500000120661c40",
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
        "0600000001000500200000000000000015000008290000002000000010000000",
        "54000000ffffffff1500000410000000200000002000000054000000ffffffff",
        "1500010000000000060000000d00050020000000000000001500000535000000",
        "2000000018000000540000000f0000001500000102000000060000000d000500",
        "2000000000000000150000016500000006000000010005001500000136010000",
        "06000000010005001500000137010000060000000100050015000001b6010000",
        "0600000001000500150000054800000020000000180000001500010008000000",
        "150000010f000000060000000100050020000000000000001500000810000000",
        "2000000018000000150004000189000015000300028900001500020002660840",
        "15000100026604401500000120661c4006000000010005002000000000000000",
        "1500010029000000060000000000ff7f20000000100000001500000110000000",
        "060000000000ff7f1500010002000000150000010a000000060000000000ff7f",
        "060000000d000500",
      ),
    ),
    (
      "proxy",
      concat!(
        "2000000004000000150001003e0000c006000000000000802000000000000000",
        "3500000100000040060000000000008015000001a90100000600000026000500",
        "150000051000000020000000180000001500010012540000150000011c540000",
        "060000000100050020000000000000001500000132000000060000000d000500",
        "150000052c000000200000002800000054000000000000201500000100000000",
        "060000000000ff7f060000005f00050015000005330100002000000028000000",
        "54000000000000201500000100000000060000000000ff7f060000005f000500",
        "150000052e000000200000002000000054000000000000201500000100000000",
        "060000000000ff7f060000005f00050015000008290000002000000010000000",
        "54000000ffffffff1500000410000000200000002000000054000000ffffffff",
        "1500010000000000060000000d00050020000000000000001500000535000000",
        "2000000018000000540000000f0000001500000102000000060000000d000500",
        "2000000000000000150000016500000006000000010005001500000136010000",
        "06000000010005001500000137010000060000000100050015000001b6010000",
        "0600000001000500150000054800000020000000180000001500010008000000",
        "150000010f000000060000000100050020000000000000001500000810000000",
        "2000000018000000150004000189000015000300028900001500020002660840",
        "15000100026604401500000120661c4006000000010005002000000000000000",
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
        "15000001c9000000060000000d00050015000008c60000002000000010000000",
        "54000000ffffffff1500000410000000200000002000000054000000ffffffff",
        "1500010000000000060000000d000500200000000000000015000005c7000000",
        "2000000018000000540000000f0000001500000102000000060000000d000500",
        "200000000000000015000001750000000600000001000500150000010e010000",
        "0600000001000500150000010f010000060000000100050015000001b6010000",
        "0600000001000500150000051900000020000000180000001500010008000000",
        "150000010f00000006000000010005002000000000000000150000081d000000",
        "2000000018000000150004000189000015000300028900001500020002660840",
        "15000100026604401500000120661c4006000000010005002000000000000000",
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
        "15000008c6000000200000001000000054000000ffffffff1500000410000000",
        "200000002000000054000000ffffffff1500010000000000060000000d000500",
        "200000000000000015000005c70000002000000018000000540000000f000000",
        "1500000102000000060000000d00050020000000000000001500000175000000",
        "0600000001000500150000010e0100000600000001000500150000010f010000",
        "060000000100050015000001b601000006000000010005001500000519000000",
        "20000000180000001500010008000000150000010f0000000600000001000500",
        "2000000000000000150000081d00000020000000180000001500040001890000",
        "1500030002890000150002000266084015000100026604401500000120661c40",
        "0600000001000500200000000000000015000100c6000000060000000000ff7f",
        "20000000100000001500000110000000060000000000ff7f1500010002000000",
        "150000010a000000060000000000ff7f060000000d000500",
      ),
    ),
    (
      "proxy",
      concat!(
        "200000000400000015000100b70000c006000000000000802000000000000000",
        "15000001a90100000600000026000500150000051d0000002000000018000000",
        "1500010012540000150000011c54000006000000010005002000000000000000",
        "15000001c9000000060000000d00050015000005ce0000002000000028000000",
        "54000000000000201500000100000000060000000000ff7f060000005f000500",
        "150000050d010000200000002800000054000000000000201500000100000000",
        "060000000000ff7f060000005f00050015000005d30000002000000020000000",
        "54000000000000201500000100000000060000000000ff7f060000005f000500",
        "15000008c6000000200000001000000054000000ffffffff1500000410000000",
        "200000002000000054000000ffffffff1500010000000000060000000d000500",
        "200000000000000015000005c70000002000000018000000540000000f000000",
        "1500000102000000060000000d00050020000000000000001500000175000000",
        "0600000001000500150000010e0100000600000001000500150000010f010000",
        "060000000100050015000001b601000006000000010005001500000519000000",
        "20000000180000001500010008000000150000010f0000000600000001000500",
        "2000000000000000150000081d00000020000000180000001500040001890000",
        "1500030002890000150002000266084015000100026604401500000120661c40",
        "0600000001000500200000000000000015000100c6000000060000000000ff7f",
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
    // no listening without the whole network: listen(2) auto-binds an
    // unbound socket past Landlock's BIND_TCP
    assert_eq!(run_filter(&program, NR_LISTEN, [3, 1, 0, 0, 0, 0]), eacces);
    assert_eq!(
      run_filter(
        &seccomp_program(OsNet::Denied),
        NR_LISTEN,
        [3, 1, 0, 0, 0, 0]
      ),
      eacces
    );
    assert_eq!(
      run_filter(&seccomp_program(OsNet::Open), NR_LISTEN, [3, 1, 0, 0, 0, 0]),
      allow
    );
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

  /// Confines a forked child as a proxy-held script is (Landlock with the
  /// proxy port, then the jail's seccomp program) and reports what `bind` +
  /// `listen` and a bare `listen` (no `bind`) return there: 0, or the errno.
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  fn listen_in_confined_child(bind_first: bool) -> i32 {
    use std::os::fd::AsRawFd;
    let dir = tempfile::tempdir().unwrap();
    let ruleset = super::super::sandbox_linux::build_ruleset(
      &[dir.path().to_path_buf()],
      &[],
      Some(45_678),
    )
    .unwrap();
    let program = seccomp_program(OsNet::ProxyOnly(45_678));
    let ruleset_fd = ruleset.as_raw_fd();
    // SAFETY: the child makes only async-signal-safe syscalls on memory
    // allocated before the fork, then exits with the outcome
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
      // SAFETY: as above; every path ends in _exit
      unsafe {
        let fail = |step: i32| -> ! { libc::_exit(100 + step) };
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
          fail(1);
        }
        if libc::syscall(libc::SYS_landlock_restrict_self, ruleset_fd, 0u32)
          != 0
        {
          fail(2);
        }
        let prog = libc::sock_fprog {
          len: (program.len() / 8) as u16,
          filter: program.as_ptr() as *mut libc::sock_filter,
        };
        if libc::prctl(
          libc::PR_SET_SECCOMP,
          libc::SECCOMP_MODE_FILTER,
          &prog as *const libc::sock_fprog,
          0,
          0,
        ) != 0
        {
          fail(3);
        }
        let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
        if fd < 0 {
          fail(4);
        }
        if bind_first {
          let addr = libc::sockaddr_in {
            sin_family: libc::AF_INET as libc::sa_family_t,
            sin_port: 0,
            sin_addr: libc::in_addr {
              s_addr: u32::from(std::net::Ipv4Addr::LOCALHOST).to_be(),
            },
            sin_zero: [0; 8],
          };
          if libc::bind(
            fd,
            &addr as *const libc::sockaddr_in as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
          ) != 0
          {
            libc::_exit(*libc::__errno_location());
          }
        }
        let res = libc::listen(fd, 1);
        libc::_exit(if res == 0 {
          0
        } else {
          *libc::__errno_location()
        });
      }
    }
    let mut status = 0;
    // SAFETY: the child just forked
    unsafe { libc::waitpid(pid, &mut status, 0) };
    assert!(libc::WIFEXITED(status));
    let code = libc::WEXITSTATUS(status);
    assert!(
      code < 100,
      "the child failed to confine itself at step {}",
      code - 100
    );
    code
  }

  // A proxy-held script can't serve: neither bind-then-listen (Landlock's
  // BIND_TCP) nor a bare listen(2), whose implicit ephemeral bind Landlock
  // doesn't mediate (it would listen on 0.0.0.0, reachable from outside
  // wherever there is no network namespace; GHSA-9363-h8qp-jg52).
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  #[test]
  #[allow(clippy::print_stderr, reason = "test code: a skip note")]
  fn proxy_held_script_cannot_listen() {
    if !super::super::sandbox_linux::can_confine_to_port() {
      eprintln!("skipped: no Landlock ABI 4 here");
      return;
    }
    assert_eq!(
      listen_in_confined_child(true),
      libc::EACCES,
      "bind + listen"
    );
    assert_eq!(
      listen_in_confined_child(false),
      libc::EACCES,
      "listen, no bind"
    );
  }

  // netlink is NETLINK_ROUTE only, socket pairs are never datagram ones,
  // and ptrace / process_vm_* / pidfd_getfd are refused, in every mode
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  #[test]
  fn seccomp_program_socket_and_process_rules() {
    let eacces = SECCOMP_RET_ERRNO | libc::EACCES as u32;
    let eperm = SECCOMP_RET_ERRNO | libc::EPERM as u32;
    let allow = SECCOMP_RET_ALLOW;
    let raw = libc::SOCK_RAW as u32 | libc::SOCK_CLOEXEC as u32;
    let unix = libc::AF_UNIX as u32;
    let socketpair = libc::SYS_socketpair as u32;
    for net in [OsNet::Denied, OsNet::Open, OsNet::ProxyOnly(1)] {
      let p = seccomp_program(net);
      let socket = |domain: u32, ty: u32, proto: u32| {
        run_filter(&p, NR_SOCKET, [domain, ty, proto, 0, 0, 0])
      };
      assert_eq!(socket(AF_NETLINK, raw, 0), allow, "{net:?}");
      // NETLINK_SOCK_DIAG, NETLINK_NETFILTER, NETLINK_KOBJECT_UEVENT,
      // NETLINK_GENERIC
      for proto in [4u32, 12, 15, 16] {
        assert_eq!(socket(AF_NETLINK, raw, proto), eacces, "{net:?} {proto}");
      }
      for ty in [libc::SOCK_STREAM, libc::SOCK_SEQPACKET] {
        let ty = ty as u32 | libc::SOCK_CLOEXEC as u32;
        assert_eq!(
          run_filter(&p, socketpair, [unix, ty, 0, 0, 0, 0]),
          allow,
          "{net:?}"
        );
      }
      let dgram = libc::SOCK_DGRAM as u32 | libc::SOCK_NONBLOCK as u32;
      assert_eq!(
        run_filter(&p, socketpair, [unix, dgram, 0, 0, 0, 0]),
        eacces,
        "{net:?}"
      );
      for nr in [
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_pidfd_getfd,
      ] {
        assert_eq!(run_filter(&p, nr as u32, [0; 6]), eperm, "{net:?} {nr}");
      }
      // what the rules don't name still passes
      assert_eq!(run_filter(&p, libc::SYS_getpid as u32, [0; 6]), allow);
    }
  }

  /// Runs `probe` in a forked child confined as a jailed script is (the
  /// jail's Landlock ruleset with no deny list, then its seccomp program
  /// for `net`) and returns the probe's exit code. The probe makes only
  /// async-signal-safe calls.
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  fn in_confined_child(net: OsNet, probe: fn() -> i32) -> i32 {
    use std::os::fd::AsRawFd;
    let dir = tempfile::tempdir().unwrap();
    let ruleset = super::super::sandbox_linux::build_ruleset(
      &[dir.path().to_path_buf()],
      &[],
      None,
    )
    .unwrap();
    let program = seccomp_program(net);
    let ruleset_fd = ruleset.as_raw_fd();
    // SAFETY: the child makes only async-signal-safe syscalls on memory
    // allocated before the fork, then exits with the outcome
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
      // SAFETY: as above; every path ends in _exit
      unsafe {
        let fail = |step: i32| -> ! { libc::_exit(200 + step) };
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
          fail(1);
        }
        if libc::syscall(libc::SYS_landlock_restrict_self, ruleset_fd, 0u32)
          != 0
        {
          fail(2);
        }
        let prog = libc::sock_fprog {
          len: (program.len() / 8) as u16,
          filter: program.as_ptr() as *mut libc::sock_filter,
        };
        if libc::prctl(
          libc::PR_SET_SECCOMP,
          libc::SECCOMP_MODE_FILTER,
          &prog as *const libc::sock_fprog,
          0,
          0,
        ) != 0
        {
          fail(3);
        }
        libc::_exit(probe());
      }
    }
    let mut status = 0;
    // SAFETY: the child just forked
    unsafe { libc::waitpid(pid, &mut status, 0) };
    assert!(libc::WIFEXITED(status));
    let code = libc::WEXITSTATUS(status);
    assert!(
      code < 200,
      "the child failed to confine itself at step {}",
      code - 200
    );
    code
  }

  /// 0 when the call succeeded, else its errno.
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  fn outcome(res: libc::c_long) -> i32 {
    if res >= 0 {
      0
    } else {
      // SAFETY: errno of this thread
      unsafe { *libc::__errno_location() }
    }
  }

  // A confined script gets a NETLINK_ROUTE socket and stream pairs, but no
  // other netlink family, no datagram pair and no ptrace
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  #[test]
  #[allow(clippy::print_stderr, reason = "test code: a skip note")]
  fn confined_script_socket_and_process_rules() {
    if super::super::sandbox_linux::landlock_abi().is_err() {
      eprintln!("skipped: no Landlock here");
      return;
    }
    fn route() -> i32 {
      // SAFETY: a plain syscall
      outcome(unsafe {
        libc::socket(libc::AF_NETLINK, libc::SOCK_RAW, libc::NETLINK_ROUTE)
      } as libc::c_long)
    }
    fn sock_diag() -> i32 {
      // SAFETY: a plain syscall
      outcome(unsafe {
        libc::socket(libc::AF_NETLINK, libc::SOCK_RAW, libc::NETLINK_SOCK_DIAG)
      } as libc::c_long)
    }
    fn stream_pair() -> i32 {
      let mut fds = [0; 2];
      // SAFETY: a two-element array
      outcome(unsafe {
        libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr())
      } as libc::c_long)
    }
    fn dgram_pair() -> i32 {
      let mut fds = [0; 2];
      // SAFETY: a two-element array
      outcome(unsafe {
        libc::socketpair(
          libc::AF_UNIX,
          libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
          0,
          fds.as_mut_ptr(),
        )
      } as libc::c_long)
    }
    fn ptrace_parent() -> i32 {
      // SAFETY: a plain syscall
      outcome(unsafe {
        libc::ptrace(libc::PTRACE_ATTACH, libc::getppid(), 0, 0)
      })
    }
    for net in [OsNet::Denied, OsNet::Open] {
      assert_eq!(in_confined_child(net, route), 0, "{net:?}");
      assert_eq!(in_confined_child(net, stream_pair), 0, "{net:?}");
      assert_eq!(in_confined_child(net, sock_diag), libc::EACCES, "{net:?}");
      assert_eq!(in_confined_child(net, dgram_pair), libc::EACCES, "{net:?}");
      assert_eq!(
        in_confined_child(net, ptrace_parent),
        libc::EPERM,
        "{net:?}"
      );
    }
  }

  // the SIGIO owner channels and the inode-flag ioctls are refused in every
  // mode; before Landlock ABI 3 a read-only O_TRUNC open is too, and
  // openat2 is ENOSYS
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  #[test]
  fn seccomp_program_supervised_rules() {
    use super::super::jail::seccomp_spec_for;
    let eperm = SECCOMP_RET_ERRNO | libc::EPERM as u32;
    let enosys = SECCOMP_RET_ERRNO | libc::ENOSYS as u32;
    let allow = SECCOMP_RET_ALLOW;
    let fcntl = libc::SYS_fcntl as u32;
    let openat = libc::SYS_openat as u32;
    let rdonly_trunc =
      (libc::O_RDONLY | libc::O_TRUNC | libc::O_CLOEXEC) as u32;
    let wronly_trunc = (libc::O_WRONLY | libc::O_TRUNC | libc::O_CREAT) as u32;
    for net in [OsNet::Denied, OsNet::Open, OsNet::ProxyOnly(1)] {
      let p = seccomp_program(net);
      for cmd in [libc::F_SETOWN, 15] {
        assert_eq!(
          run_filter(&p, fcntl, [3, cmd as u32, 1, 0, 0, 0]),
          eperm,
          "{net:?}"
        );
      }
      assert_eq!(
        run_filter(&p, fcntl, [3, libc::F_GETFL as u32, 0, 0, 0, 0]),
        allow
      );
      for req in [0x8901u32, 0x8902, 0x4008_6602, 0x4004_6602, 0x401c_6620] {
        assert_eq!(run_filter(&p, NR_IOCTL, [3, req, 0, 0, 0, 0]), eperm);
      }
      // ABI 3+ (and the golden programs): Landlock handles truncation
      assert_eq!(run_filter(&p, openat, [0, 0, rdonly_trunc, 0, 0, 0]), allow);
      let old = seccomp_spec_for(net, 2).program();
      assert_eq!(
        run_filter(&old, openat, [0, 0, rdonly_trunc, 0, 0, 0]),
        eperm,
        "{net:?}"
      );
      // a write-mode O_TRUNC open is Landlock's (WRITE_FILE), a plain read
      // open passes
      assert_eq!(
        run_filter(&old, openat, [0, 0, wronly_trunc, 0, 0, 0]),
        allow
      );
      assert_eq!(
        run_filter(&old, openat, [0, 0, libc::O_RDONLY as u32, 0, 0, 0]),
        allow
      );
      assert_eq!(run_filter(&old, 437, [0; 6]), enosys);
      #[cfg(target_arch = "x86_64")]
      assert_eq!(
        run_filter(&old, libc::SYS_open as u32, [0, rdonly_trunc, 0, 0, 0, 0]),
        eperm
      );
      assert_eq!(
        seccomp_spec_for(net, 3),
        super::super::jail::seccomp_spec(net)
      );
    }
  }

  /// A python3 to drive the syscalls a shell has no command for.
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  fn python3() -> Option<std::path::PathBuf> {
    ["/usr/bin/python3", "/usr/local/bin/python3", "/bin/python3"]
      .into_iter()
      .map(std::path::PathBuf::from)
      .find(|p| p.exists())
  }

  /// How a test confines its shell.
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  #[derive(Clone, Copy)]
  enum Confine {
    /// Landlock, the jail's seccomp program and the supervisor, as on this
    /// machine (a private /dev/shm where namespaces allow).
    Jail,
    /// [`Confine::Jail`] where no private /dev/shm can be had: none.
    JailNoShm,
    /// The jail's seccomp program and the supervisor for a kernel with
    /// Landlock ABI `abi`, without Landlock: what the supervisor alone
    /// holds (truncation before ABI 3).
    SupervisorOnly(u32),
  }

  /// Runs `script` under `/bin/sh` (its arguments `args`) in a session of
  /// its own, confined as `confine` says with the write root `write`, and
  /// returns its stdout. The supervisor serves it until it ends.
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  async fn jailed_shell(
    script: &str,
    args: &[String],
    write: &std::path::Path,
    confine: Confine,
  ) -> String {
    use std::os::unix::process::CommandExt as _;

    use super::super::jail::linux_os_deny_read;
    use super::super::jail::seccomp_spec_for;
    use super::super::jail::supervised_syscalls;
    let write = vec![write.to_path_buf()];
    let mut command = tokio::process::Command::new("/bin/sh");
    command
      .arg("-c")
      .arg(script)
      .arg("sh")
      .args(args)
      .stdin(std::process::Stdio::null())
      .stdout(std::process::Stdio::piped())
      .stderr(std::process::Stdio::null())
      .kill_on_drop(true);
    // SAFETY: setsid is async-signal-safe
    unsafe {
      command.as_std_mut().pre_exec(|| {
        if libc::setsid() < 0 {
          return Err(std::io::Error::last_os_error());
        }
        Ok(())
      });
    }
    let abi = match confine {
      Confine::Jail | Confine::JailNoShm => {
        super::super::sandbox_linux::landlock_abi().unwrap()
      }
      Confine::SupervisorOnly(abi) => abi,
    };
    let private_shm = matches!(confine, Confine::Jail)
      && super::super::sandbox_linux::private_shm_available().is_ok();
    let _ruleset = match confine {
      Confine::Jail | Confine::JailNoShm => {
        let ruleset = super::super::sandbox_linux::build_ruleset_opts(
          &write,
          &linux_os_deny_read(&[]),
          None,
          None,
          false,
        )
        .unwrap();
        if private_shm {
          super::super::sandbox_linux::PrivateShm::new(true, false)
            .unwrap()
            .apply_to_command(&mut command, &ruleset);
        }
        super::super::sandbox_linux::apply_to_command(
          &mut command,
          &ruleset,
          &seccomp_spec_for(OsNet::Denied, abi),
        );
        Some(ruleset)
      }
      Confine::SupervisorOnly(_) => {
        let program = seccomp_spec_for(OsNet::Denied, abi).program();
        // SAFETY: async-signal-safe prctls on memory allocated before fork
        unsafe {
          command.as_std_mut().pre_exec(move || {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
              return Err(std::io::Error::last_os_error());
            }
            let prog = libc::sock_fprog {
              len: (program.len() / 8) as u16,
              filter: program.as_ptr() as *mut libc::sock_filter,
            };
            if libc::prctl(
              libc::PR_SET_SECCOMP,
              libc::SECCOMP_MODE_FILTER,
              &prog as *const libc::sock_fprog,
              0,
              0,
            ) != 0
            {
              return Err(std::io::Error::last_os_error());
            }
            Ok(())
          });
        }
        None
      }
    };
    let handoff = deno_sandbox::notify::NotifyHandoff::new().unwrap();
    handoff.apply_to_command(
      &mut command,
      deno_sandbox::notify::notify_program(&supervised_syscalls(abi)),
    );
    let mut child = command.spawn().unwrap();
    let pid = child.id().unwrap();
    let listener = tokio::task::spawn_blocking(move || handoff.receive())
      .await
      .unwrap()
      .unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let thread_stop = stop.clone();
    let roots = write.clone();
    let thread = std::thread::spawn(move || {
      deno_sandbox::notify::Supervisor::new(roots, pid)
        .with_private_shm(private_shm)
        .serve(listener, &thread_stop)
    });
    let mut stdout = child.stdout.take().unwrap();
    let mut out = String::new();
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
      use tokio::io::AsyncReadExt;
      stdout.read_to_string(&mut out).await.unwrap();
      child.wait().await.unwrap();
    })
    .await
    .expect("the jailed shell timed out");
    // SAFETY: the shell's own process group, which it leads
    unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    thread.join().unwrap();
    out
  }

  /// A temporary directory with `inside/` (the write root) and `outside/`,
  /// each holding a file `f` (mode 644).
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  fn two_dirs() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let real = crate::util::fs::canonicalize_path(dir.path()).unwrap();
    let inside = real.join("inside");
    let outside = real.join("outside");
    for d in [&inside, &outside] {
      std::fs::create_dir(d).unwrap();
      std::fs::write(d.join("f"), "content\n").unwrap();
      std::fs::set_permissions(
        d.join("f"),
        std::fs::Permissions::from_mode(0o644),
      )
      .unwrap();
    }
    (dir, inside, outside)
  }

  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  fn mode_of(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
  }

  // A jailed script changes mode, owner and timestamps beneath its write
  // roots only: outside them chmod, chown, touch (utimensat) and a link
  // that leads out are refused, through a path and through a descriptor,
  // and nothing there changes
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  #[tokio::test]
  #[allow(clippy::print_stderr, reason = "test code: a skip note")]
  async fn jailed_script_metadata_stays_in_its_roots() {
    if super::super::sandbox_linux::landlock_abi().is_err() {
      eprintln!("skipped: no Landlock here");
      return;
    }
    let (_dir, inside, outside) = two_dirs();
    std::os::unix::fs::symlink(outside.join("f"), inside.join("link")).unwrap();
    let before = std::fs::metadata(outside.join("f")).unwrap();
    let script = r#"
      r() { if "$@" 2>/dev/null; then echo ok; else echo refused; fi; }
      echo "chmod-in $(r chmod 600 "$1/f")"
      echo "chmod-out $(r chmod 600 "$2/f")"
      echo "chmod-link $(r chmod 600 "$1/link")"
      echo "chown-out $(r chown "$(id -u)" "$2/f")"
      echo "touch-in $(r touch -m -d @0 "$1/f")"
      echo "touch-out $(r touch -m -d @0 "$2/f")"
      echo "procfd-chmod-out $(r sh -c 'exec 3<"$1"; chmod 600 /dev/fd/3' sh "$2/f")"
      if [ -n "$3" ]; then "$3" -c '
import errno, os, sys
def r(f):
    try:
        f(); return "ok"
    except OSError as e:
        return "refused" if e.errno == errno.EPERM else "error %d" % e.errno
fd = os.open(sys.argv[2] + "/f", os.O_RDONLY)
print("fchmod-out", r(lambda: os.fchmod(fd, 0o600)))
print("fchown-out", r(lambda: os.fchown(fd, os.getuid(), -1)))
print("futimens-out", r(lambda: os.utime(fd, (0, 0))))
mine = os.open(sys.argv[1] + "/f", os.O_RDONLY)
print("fchmod-in", r(lambda: os.fchmod(mine, 0o640)))
print("utime-nofollow-link", r(lambda: os.utime(sys.argv[1] + "/link", (0, 0), follow_symlinks=False)))
' "$1" "$2"; fi
    "#;
    let python = python3()
      .map(|p| p.to_string_lossy().into_owned())
      .unwrap_or_default();
    let out = jailed_shell(
      script,
      &[
        inside.to_string_lossy().into_owned(),
        outside.to_string_lossy().into_owned(),
        python.clone(),
      ],
      &inside,
      Confine::Jail,
    )
    .await;
    for line in [
      "chmod-in ok",
      "chmod-out refused",
      "chmod-link refused",
      "chown-out refused",
      "touch-in ok",
      "touch-out refused",
      "procfd-chmod-out refused",
    ] {
      assert!(out.lines().any(|l| l == line), "missing {line:?} in\n{out}");
    }
    if !python.is_empty() {
      // the link itself is beneath the root: its own times may change
      for line in [
        "fchmod-out refused",
        "fchown-out refused",
        "futimens-out refused",
        "fchmod-in ok",
        "utime-nofollow-link ok",
      ] {
        assert!(out.lines().any(|l| l == line), "missing {line:?} in\n{out}");
      }
    }
    assert_eq!(
      mode_of(&inside.join("f")),
      if python.is_empty() { 0o600 } else { 0o640 }
    );
    assert_eq!(mode_of(&outside.join("f")), 0o644);
    let after = std::fs::metadata(outside.join("f")).unwrap();
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
  }

  // A path through a procfs magic link (`/dev/fd/N`, `/proc/self/fd/N`,
  // `/proc/thread-self/fd/N`) never reaches the supervisor's own
  // descriptors: the supervisor resolves the caller's path, so `self` there
  // is the installer. Here the installer holds a pipe (no path) and a file
  // beneath the write root at descriptor numbers the script doesn't have;
  // each change is refused and neither object changes (raw syscalls from
  // python where there is one: chmod(1) stats the path first, which fails in
  // the caller). The caller's own descriptor through such a path is refused
  // too (fails closed)
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  #[tokio::test]
  #[allow(clippy::print_stderr, reason = "test code: a skip note")]
  async fn jailed_script_magic_links_never_reach_the_supervisor() {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::PermissionsExt;
    if super::super::sandbox_linux::landlock_abi().is_err() {
      eprintln!("skipped: no Landlock here");
      return;
    }
    let (_dir, inside, _outside) = two_dirs();
    std::fs::write(inside.join("g"), "content\n").unwrap();
    std::fs::set_permissions(
      inside.join("g"),
      std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let before = std::fs::metadata(inside.join("g")).unwrap();
    // close-on-exec, both: the jailed shell has neither descriptor
    let (pipe_read, _pipe_write) = std::io::pipe().unwrap();
    let pipe_mode = |fd: &std::io::PipeReader| {
      // SAFETY: a zeroed stat is a valid out parameter
      let mut st: libc::stat = unsafe { std::mem::zeroed() };
      // SAFETY: a valid descriptor and out parameter
      assert_eq!(unsafe { libc::fstat(fd.as_raw_fd(), &mut st) }, 0);
      st.st_mode & 0o7777
    };
    let pipe_before = pipe_mode(&pipe_read);
    let held = std::fs::File::open(inside.join("g")).unwrap();
    let script = r#"
      r() { if "$@" 2>/dev/null; then echo ok; else echo refused; fi; }
      echo "supervisor-file-times $(r touch -m -d @0 "/proc/thread-self/fd/$3")"
      echo "own-fd $(r sh -c 'exec 5<"$1"; chmod 600 /dev/fd/5' sh "$1/f")"
      echo "control $(r chmod 640 "$1/f")"
      if [ -n "$4" ]; then "$4" -c '
import errno, os, sys
def r(f):
    try:
        f(); return "ok"
    except OSError as e:
        return "refused" if e.errno == errno.EPERM else "error %d" % e.errno
pipe, held = sys.argv[1], sys.argv[2]
print("supervisor-pipe", r(lambda: os.chmod("/dev/fd/" + pipe, 0o606)))
print("supervisor-file", r(lambda: os.chmod("/proc/self/fd/" + held, 0o600)))
print("supervisor-file-owner", r(lambda: os.chown("/dev/fd/" + held, os.getuid(), -1)))
print("supervisor-file-size", r(lambda: os.truncate("/dev/fd/" + held, 0)))
' "$2" "$3"; fi
    "#;
    let python = python3()
      .map(|p| p.to_string_lossy().into_owned())
      .unwrap_or_default();
    let out = jailed_shell(
      script,
      &[
        inside.to_string_lossy().into_owned(),
        pipe_read.as_raw_fd().to_string(),
        held.as_raw_fd().to_string(),
        python.clone(),
      ],
      &inside,
      Confine::Jail,
    )
    .await;
    for line in [
      "supervisor-file-times refused",
      "own-fd refused",
      "control ok",
    ] {
      assert!(out.lines().any(|l| l == line), "missing {line:?} in\n{out}");
    }
    if !python.is_empty() {
      for line in ["supervisor-pipe refused", "supervisor-file-owner refused"] {
        assert!(out.lines().any(|l| l == line), "missing {line:?} in\n{out}");
      }
      // `/proc/self/fd/N` resolved by the supervisor: refused at the magic
      // link when the process resolving it holds a descriptor N, ENOENT when
      // it holds none (the supervisor needn't be the process that opened
      // `held`: run 44969626250); either way the file isn't reached
      assert!(
        out
          .lines()
          .any(|l| l == "supervisor-file refused"
            || l == "supervisor-file error 2"),
        "chmod through the supervisor's descriptor in\n{out}"
      );
      // Landlock (ABI 3+) or the supervisor holds truncate: refused, or the
      // caller's own ENOENT (it has no such descriptor), never done
      assert!(
        out.lines().any(|l| l.starts_with("supervisor-file-size ")
          && l != "supervisor-file-size ok"),
        "truncate through the supervisor's descriptor in\n{out}"
      );
    }
    assert_eq!(std::fs::read(inside.join("g")).unwrap(), b"content\n");
    assert_eq!(pipe_mode(&pipe_read), pipe_before);
    assert_eq!(mode_of(&inside.join("g")), 0o644);
    let after = std::fs::metadata(inside.join("g")).unwrap();
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    assert_eq!(mode_of(&inside.join("f")), 0o640);
    drop(held);
  }

  // A jailed script sets and removes extended attributes beneath its roots
  // only (where the file system takes user attributes at all)
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  #[tokio::test]
  #[allow(clippy::print_stderr, reason = "test code: a skip note")]
  async fn jailed_script_xattrs_stay_in_its_roots() {
    if super::super::sandbox_linux::landlock_abi().is_err() {
      eprintln!("skipped: no Landlock here");
      return;
    }
    let (_dir, inside, outside) = two_dirs();
    let set = |path: &std::path::Path| {
      let path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
      // SAFETY: NUL-terminated strings and a one-byte value
      unsafe {
        libc::setxattr(
          path.as_ptr(),
          c"user.t".as_ptr(),
          b"1".as_ptr() as _,
          1,
          0,
        )
      }
    };
    if set(&outside.join("f")) != 0 {
      eprintln!("skipped: no user xattrs on this file system");
      return;
    }
    let Some(python) = python3() else {
      eprintln!("skipped: no python3");
      return;
    };
    let script = r#"
      "$3" -c '
import errno, os, sys
def r(f):
    try:
        f(); return "ok"
    except OSError as e:
        return "refused" if e.errno == errno.EPERM else "error %d" % e.errno
print("setxattr-in", r(lambda: os.setxattr(sys.argv[1] + "/f", "user.x", b"1")))
print("setxattr-out", r(lambda: os.setxattr(sys.argv[2] + "/f", "user.x", b"1")))
print("removexattr-out", r(lambda: os.removexattr(sys.argv[2] + "/f", "user.t")))
fd = os.open(sys.argv[2] + "/f", os.O_RDONLY)
print("fsetxattr-out", r(lambda: os.setxattr(fd, "user.x", b"1")))
' "$1" "$2"
    "#;
    let out = jailed_shell(
      script,
      &[
        inside.to_string_lossy().into_owned(),
        outside.to_string_lossy().into_owned(),
        python.to_string_lossy().into_owned(),
      ],
      &inside,
      Confine::Jail,
    )
    .await;
    for line in [
      "setxattr-in ok",
      "setxattr-out refused",
      "removexattr-out refused",
      "fsetxattr-out refused",
    ] {
      assert!(out.lines().any(|l| l == line), "missing {line:?} in\n{out}");
    }
  }

  // A jailed script signals only its own processes: its child and its own
  // group, but not the installer (here, the test) nor every process
  // (kill -1); a signal to a pid that doesn't exist still says so (ESRCH)
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  #[tokio::test]
  #[allow(clippy::print_stderr, reason = "test code: a skip note")]
  async fn jailed_script_signals_only_its_own_processes() {
    if super::super::sandbox_linux::landlock_abi().is_err() {
      eprintln!("skipped: no Landlock here");
      return;
    }
    let (_dir, inside, _outside) = two_dirs();
    // SAFETY: getpid / getpgrp have no failure mode
    let (me, my_group) = unsafe { (libc::getpid(), libc::getpgrp()) };
    let script = r#"
      r() { if "$@" 2>/dev/null; then echo ok; else echo refused; fi; }
      sleep 30 &
      echo "child $(r kill -s 0 $!)"
      echo "term-child $(r kill $!)"
      echo "group $(r kill -s 0 0)"
      echo "installer $(r kill -s 0 "$1")"
      echo "everyone $(r kill -s 0 -- -1)"
      echo "installer-group $(r kill -s 0 -- -"$2")"
      echo "gone $(kill -s 0 999999999 2>&1 | grep -qi 'no such' && echo esrch || echo other)"
    "#;
    let out = jailed_shell(
      script,
      &[me.to_string(), my_group.to_string()],
      &inside,
      Confine::Jail,
    )
    .await;
    for line in [
      "child ok",
      "term-child ok",
      "group ok",
      "installer refused",
      "everyone refused",
      "installer-group refused",
      "gone esrch",
    ] {
      assert!(out.lines().any(|l| l == line), "missing {line:?} in\n{out}");
    }
    // pidfd_send_signal, on a pidfd and on a /proc/<pid> directory
    let Some(python) = python3() else {
      eprintln!("skipped the pidfd part: no python3");
      return;
    };
    let script = r#"
      "$2" -c '
import errno, os, signal, sys
def r(f):
    try:
        f(); return "ok"
    except OSError as e:
        return "refused" if e.errno == errno.EPERM else "error %d" % e.errno
pid = int(sys.argv[1])
if hasattr(os, "pidfd_open"):
    fd = os.pidfd_open(pid)
    print("pidfd-installer", r(lambda: signal.pidfd_send_signal(fd, 0)))
    own = os.pidfd_open(os.getpid())
    print("pidfd-self", r(lambda: signal.pidfd_send_signal(own, 0)))
d = os.open("/proc/%d" % pid, os.O_RDONLY | os.O_DIRECTORY)
print("procfd-installer", r(lambda: signal.pidfd_send_signal(d, 0)))
' "$1"
    "#;
    let out = jailed_shell(
      script,
      &[me.to_string(), python.to_string_lossy().into_owned()],
      &inside,
      Confine::Jail,
    )
    .await;
    assert!(
      out.lines().any(|l| l == "procfd-installer refused"),
      "{out}"
    );
    if out.contains("pidfd-") {
      assert!(out.lines().any(|l| l == "pidfd-installer refused"), "{out}");
      assert!(out.lines().any(|l| l == "pidfd-self ok"), "{out}");
    }
  }

  // Before Landlock ABI 3 the supervisor holds truncation to the write
  // roots (truncate(2)), and a read-only O_TRUNC open is refused outright;
  // shown here with the supervisor alone, as such a kernel would run it
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  #[tokio::test]
  #[allow(clippy::print_stderr, reason = "test code: a skip note")]
  async fn supervisor_file_size_rules() {
    let (_dir, inside, outside) = two_dirs();
    let Some(python) = python3() else {
      eprintln!("skipped: no python3");
      return;
    };
    let script = r#"
      "$3" -c '
import errno, os, sys
def r(f):
    try:
        f(); return "ok"
    except OSError as e:
        return "refused" if e.errno == errno.EPERM else "error %d" % e.errno
print("truncate-in", r(lambda: os.truncate(sys.argv[1] + "/f", 0)))
print("truncate-out", r(lambda: os.truncate(sys.argv[2] + "/f", 0)))
print("rdonly-trunc-out", r(lambda: os.close(os.open(sys.argv[2] + "/f", os.O_RDONLY | os.O_TRUNC))))
' "$1" "$2"
    "#;
    let out = jailed_shell(
      script,
      &[
        inside.to_string_lossy().into_owned(),
        outside.to_string_lossy().into_owned(),
        python.to_string_lossy().into_owned(),
      ],
      &inside,
      Confine::SupervisorOnly(2),
    )
    .await;
    for line in [
      "truncate-in ok",
      "truncate-out refused",
      "rdonly-trunc-out refused",
    ] {
      assert!(out.lines().any(|l| l == line), "missing {line:?} in\n{out}");
    }
    assert_eq!(std::fs::read(inside.join("f")).unwrap(), b"");
    assert_eq!(std::fs::read(outside.join("f")).unwrap(), b"content\n");
  }

  // A jailed script can't open the user's terminals (or the rest of /dev
  // but the devices a script uses) for reading
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  #[tokio::test]
  #[allow(clippy::print_stderr, reason = "test code: a skip note")]
  async fn jailed_script_cannot_read_a_terminal() {
    if super::super::sandbox_linux::landlock_abi().is_err() {
      eprintln!("skipped: no Landlock here");
      return;
    }
    // a terminal of this user's, as a shell in another window would have
    // SAFETY: plain pty calls; the name is copied before the next call
    let (master, slave) = unsafe {
      let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
      if master < 0 || libc::grantpt(master) != 0 || libc::unlockpt(master) != 0
      {
        eprintln!("skipped: no pty here");
        return;
      }
      let name = std::ffi::CStr::from_ptr(libc::ptsname(master));
      (master, name.to_string_lossy().into_owned())
    };
    let (_dir, inside, _outside) = two_dirs();
    let script = r#"
      r() { if "$@" 2>/dev/null; then echo ok; else echo refused; fi; }
      echo "tty $(r sh -c 'exec 3<"$1"' sh "$1")"
      echo "null $(r sh -c 'exec 3</dev/null' sh)"
      echo "urandom $(r sh -c 'head -c 4 /dev/urandom >/dev/null' sh)"
    "#;
    let out = jailed_shell(
      script,
      std::slice::from_ref(&slave),
      &inside,
      Confine::Jail,
    )
    .await;
    // SAFETY: opened above
    unsafe { libc::close(master) };
    for line in ["tty refused", "null ok", "urandom ok"] {
      assert!(out.lines().any(|l| l == line), "missing {line:?} in\n{out}");
    }
  }

  // A jailed script gets a /dev/shm of its own where namespaces allow:
  // POSIX shared memory and semaphores work, the host's segments are out of
  // sight and untouched, and nothing it makes there reaches the host; where
  // they don't, it gets no /dev/shm at all
  #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
  #[tokio::test]
  #[allow(clippy::print_stderr, reason = "test code: a skip note")]
  async fn jailed_script_gets_a_private_dev_shm() {
    if super::super::sandbox_linux::landlock_abi().is_err() {
      eprintln!("skipped: no Landlock here");
      return;
    }
    let (_dir, inside, _outside) = two_dirs();
    // SAFETY: getpid has no failure mode
    let tag = format!("denext-jail-test-{}", unsafe { libc::getpid() });
    let host = std::path::PathBuf::from("/dev/shm").join(&tag);
    std::fs::write(&host, "host\n").unwrap();
    let mine = format!("{tag}-mine");
    let python = python3()
      .map(|p| p.to_string_lossy().into_owned())
      .unwrap_or_default();
    let script = r#"
      r() { if "$@" 2>/dev/null; then echo ok; else echo refused; fi; }
      if [ -e "/dev/shm/$1" ]; then echo "host-seen yes"; else echo "host-seen no"; fi
      echo "host-write $(r sh -c 'echo x > "/dev/shm/$1"' sh "$1")"
      echo "own-write $(r sh -c 'echo x > "/dev/shm/$1"' sh "$2")"
      echo "own-chmod $(r chmod 600 "/dev/shm/$2")"
      if [ -n "$3" ]; then
        echo "semaphore $(r "$3" -c 'import multiprocessing; multiprocessing.Lock()')"
      fi
    "#;
    let args = [tag.clone(), mine.clone(), python.clone()];
    let shared = super::super::sandbox_linux::private_shm_available().is_ok();
    let out = jailed_shell(script, &args, &inside, Confine::Jail).await;
    let nothing =
      jailed_shell(script, &args, &inside, Confine::JailNoShm).await;
    assert_eq!(std::fs::read(&host).unwrap(), b"host\n");
    let leaked = std::path::Path::new("/dev/shm").join(&mine).exists();
    let _ = std::fs::remove_file(&host);
    let _ = std::fs::remove_file(std::path::Path::new("/dev/shm").join(&mine));
    // no namespaces: no /dev/shm, whatever the host holds
    for line in ["host-write refused", "own-write refused"] {
      assert!(
        nothing.lines().any(|l| l == line),
        "missing {line:?} in\n{nothing}"
      );
    }
    assert!(!leaked, "the script's file reached the host's /dev/shm");
    if !shared {
      eprintln!("skipped the private part: no user and mount namespaces here");
      return;
    }
    for line in ["host-seen no", "own-write ok", "own-chmod ok"] {
      assert!(out.lines().any(|l| l == line), "missing {line:?} in\n{out}");
    }
    if !python.is_empty() {
      assert!(out.lines().any(|l| l == "semaphore ok"), "{out}");
    }
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
