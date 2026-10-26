// Copyright 2018-2026 the Deno authors. MIT license.

//! The OS confinement of a jailed lifecycle script on macOS: the script's
//! process tree runs under `sandbox-exec` (Seatbelt) with a deny-by-default
//! profile (DENEXT.md item 124) that allows only what a script, a shell, a
//! compiler or a download needs: reading files (minus the credential files),
//! writing beneath the jail's roots, forking and running programs, signals
//! within the sandbox, `sysctl` reads (minus the process table, which holds
//! every process's environment), POSIX IPC, and a handful of mach services
//! (the user database, the per-user temp directories, logging). Everything
//! else is refused: LaunchServices (`open`), launchd job submission,
//! AppleEvents (`osascript`), the pasteboard, the keychain (`securityd`,
//! `trustd`), the DNS responder, hard links from outside the roots, the
//! terminals (`/dev/tty`, `/dev/ttys*`: no `TIOCSTI`), and every other mach
//! service. A setuid program (`/bin/ps`, `sudo`) can't be started inside it,
//! as under Linux's `no_new_privs`.
//!
//! The network: none (not even Unix sockets; a Docker socket runs anything),
//! the whole of it, or only the script's allow-listing proxy
//! (`localhost:<port>`, `jail_proxy`) with no DNS; without the whole network
//! a script can't listen for connections either. Seatbelt checks the resolved
//! path, so a symlink in a writable root can't lead outside it.

use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

use super::jail::OsNet;

pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Whether `sandbox-exec` works here (it is missing on some systems, and
/// refuses to run inside another sandbox).
pub fn available() -> Result<(), String> {
  static AVAILABLE: OnceLock<Result<(), String>> = OnceLock::new();
  AVAILABLE
    .get_or_init(|| {
      if !Path::new(SANDBOX_EXEC).exists() {
        return Err(format!("{SANDBOX_EXEC} was not found"));
      }
      match std::process::Command::new(SANDBOX_EXEC)
        .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
      {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!(
          "{SANDBOX_EXEC} does not work here ({status}; is this already sandboxed?)"
        )),
        Err(err) => Err(format!("{SANDBOX_EXEC} failed to start: {err}")),
      }
    })
    .clone()
}

fn quote(path: &Path) -> String {
  let text = path.to_string_lossy();
  format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The terminal devices, which a jailed script may neither read, write nor
/// `ioctl` (a `TIOCSTI` types into the user's shell).
const TTY_FILTERS: &str = "(literal \"/dev/tty\") (regex #\"^/dev/ttys[0-9]+$\") (regex #\"^/dev/pty\")";

/// The mach services a script's programs need: the user database
/// (`getpwuid`), the per-user temp and cache directories (`confstr`, which
/// compilers ask for), and logging.
const MACH_ALLOW: &[&str] = &[
  "com.apple.system.opendirectoryd.libinfo",
  "com.apple.system.opendirectoryd.membership",
  "com.apple.bsd.dirhelper",
  "com.apple.system.logger",
  "com.apple.logd",
  "com.apple.system.notification_center",
];

/// Services refused by name as well as by the default, so a later allow
/// can't widen past them: the keychain and trust evaluation, LaunchServices,
/// the pasteboard, consent prompts, DNS.
const MACH_DENY: &[&str] = &[
  "com.apple.SecurityServer",
  "com.apple.securityd",
  "com.apple.securityd.xpc",
  "com.apple.trustd",
  "com.apple.trustd.agent",
  "com.apple.coreservices.launchservicesd",
  "com.apple.lsd.mapdb",
  "com.apple.lsd.modifydb",
  "com.apple.pasteboard.1",
  "com.apple.tccd",
  "com.apple.tccd.system",
  "com.apple.dnssd.service",
];

/// The Seatbelt profile.
pub fn profile(
  write_roots: &[PathBuf],
  deny_read: &[PathBuf],
  net: OsNet,
) -> String {
  let mut p = String::from("(version 1)\n(deny default)\n");
  // files: read everything but the credential files and the terminals;
  // write (and hard-link, which needs the source inside too) beneath the
  // roots and to the null devices; ioctl everything but the terminals
  p.push_str("(allow file-read*)\n");
  p.push_str("(deny file-read* ");
  p.push_str(TTY_FILTERS);
  for path in deny_read {
    p.push_str(&format!(" (subpath {})", quote(path)));
  }
  p.push_str(")\n");
  p.push_str("(allow file-write*");
  for root in write_roots {
    p.push_str(&format!(" (subpath {})", quote(root)));
  }
  for dev in [
    "/dev/null",
    "/dev/zero",
    "/dev/dtracehelper",
    "/dev/stdout",
    "/dev/stderr",
  ] {
    p.push_str(&format!(" (literal \"{dev}\")"));
  }
  p.push_str(" (regex #\"^/dev/fd/\"))\n");
  if !write_roots.is_empty() {
    p.push_str("(allow file-link");
    for root in write_roots {
      p.push_str(&format!(" (subpath {})", quote(root)));
    }
    p.push_str(")\n");
  }
  p.push_str("(allow file-ioctl)\n");
  p.push_str("(deny file-ioctl ");
  p.push_str(TTY_FILTERS);
  p.push_str(")\n");
  // processes: fork and run programs (a setuid one can't start under a
  // profile), signal and inspect only the sandbox's own, read sysctls but
  // not the process table (`kern.procargs2` is every process's environment)
  p.push_str("(allow process-fork)\n(allow process-exec)\n");
  p.push_str("(allow signal (target same-sandbox))\n");
  p.push_str("(allow process-info* (target same-sandbox))\n");
  p.push_str("(allow sysctl-read)\n");
  p.push_str("(deny sysctl-read (sysctl-name-prefix \"kern.proc\"))\n");
  p.push_str("(allow ipc-posix-sem)\n(allow ipc-posix-shm*)\n");
  // mach services: the short list, and the refused ones by name
  p.push_str("(allow mach-lookup");
  for name in MACH_ALLOW {
    p.push_str(&format!(" (global-name \"{name}\")"));
  }
  p.push_str(")\n");
  p.push_str("(deny mach-lookup");
  for name in MACH_DENY {
    p.push_str(&format!(" (global-name \"{name}\")"));
  }
  p.push_str(")\n");
  p.push_str("(deny lsopen)\n(deny appleevent-send)\n(deny job-creation)\n");
  // the network: Unix sockets reach other daemons (a Docker socket runs
  // anything): none, whatever the network setting, but the DNS resolver's
  // when it's allowed
  match net {
    OsNet::Open => {
      p.push_str("(allow network*)\n");
      p.push_str("(deny network-outbound (remote unix-socket))\n");
      p.push_str(
        "(allow network-outbound (remote unix-socket (path-literal \"/private/var/run/mDNSResponder\")))\n",
      );
      p.push_str(
        "(allow mach-lookup (global-name \"com.apple.dnssd.service\"))\n",
      );
    }
    OsNet::Denied => {
      p.push_str("(deny network*)\n");
    }
    OsNet::ProxyOnly(port) => {
      // the last matching rule wins: the proxy's port on the loopback
      // interface only (TCP and UDP to anywhere else are denied), and no
      // serving: a listening socket would hand data to whoever connects in
      p.push_str("(deny network*)\n");
      p.push_str(&format!(
        "(allow network-outbound (remote ip \"localhost:{port}\"))\n"
      ));
    }
  }
  p
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn profile_quotes_paths() {
    let p = profile(
      &[PathBuf::from("/a \"b\"/c\\d")],
      &[PathBuf::from("/home/.ssh")],
      OsNet::Denied,
    );
    assert!(p.contains("(subpath \"/a \\\"b\\\"/c\\\\d\")"), "{p}");
    assert!(p.contains("(subpath \"/home/.ssh\"))"), "{p}");
    assert!(p.contains("(deny network*)"));
    assert!(p.contains("com.apple.dnssd.service"));
    let p = profile(&[], &[], OsNet::Open);
    assert!(p.contains("(allow network*)"));
    assert!(p.contains("mDNSResponder"));
    assert!(p.contains(
      "(allow mach-lookup (global-name \"com.apple.dnssd.service\"))"
    ));
    // a deny of a read path list that is empty still names the terminals
    assert!(p.contains("(deny file-read* (literal \"/dev/tty\")"));
    assert!(!p.contains("(allow file-link"));
  }

  // DENEXT.md item 124: deny by default, the services a script could leave
  // the sandbox through refused by name, no terminals, hard links only
  // from the roots
  #[test]
  fn profile_denies_by_default() {
    let p = profile(
      &[PathBuf::from("/nm/pkg"), PathBuf::from("/nm/.deno/.jail/x")],
      &[PathBuf::from("/home/.ssh")],
      OsNet::Denied,
    );
    assert!(p.starts_with("(version 1)\n(deny default)\n"), "{p}");
    assert!(!p.contains("(allow default)"));
    for rule in [
      "(allow file-read*)",
      "(deny file-read* (literal \"/dev/tty\") (regex #\"^/dev/ttys[0-9]+$\") (regex #\"^/dev/pty\") (subpath \"/home/.ssh\"))",
      "(allow file-write* (subpath \"/nm/pkg\") (subpath \"/nm/.deno/.jail/x\") (literal \"/dev/null\")",
      "(allow file-link (subpath \"/nm/pkg\") (subpath \"/nm/.deno/.jail/x\"))",
      "(deny file-ioctl (literal \"/dev/tty\")",
      "(allow process-fork)",
      "(allow process-exec)",
      "(allow signal (target same-sandbox))",
      "(allow process-info* (target same-sandbox))",
      "(deny sysctl-read (sysctl-name-prefix \"kern.proc\"))",
      "(deny lsopen)",
      "(deny appleevent-send)",
      "(deny job-creation)",
      "(global-name \"com.apple.SecurityServer\")",
      "(global-name \"com.apple.coreservices.launchservicesd\")",
      "(global-name \"com.apple.pasteboard.1\")",
      "(global-name \"com.apple.dnssd.service\")",
      "(global-name \"com.apple.system.opendirectoryd.libinfo\")",
    ] {
      assert!(p.contains(rule), "missing {rule}\n{p}");
    }
    // the terminals are writable nowhere
    assert!(!p.contains("(literal \"/dev/tty\") (literal"), "{p}");
    let write = p
      .lines()
      .find(|l| l.starts_with("(allow file-write*"))
      .unwrap();
    assert!(!write.contains("tty"), "{write}");
    // the deny of the services comes after the allow list, so it wins
    let allow = p.find("(allow mach-lookup").unwrap();
    let deny = p.find("(deny mach-lookup").unwrap();
    assert!(allow < deny);
  }

  // DENEXT.md item 111: only the proxy's port, no DNS
  #[test]
  fn profile_proxy_only() {
    let p = profile(&[], &[], OsNet::ProxyOnly(4321));
    let deny = p.find("(deny network*)").unwrap();
    let allow = p
      .find("(allow network-outbound (remote ip \"localhost:4321\"))")
      .unwrap();
    // later rules win in Seatbelt
    assert!(deny < allow, "{p}");
    assert!(!p.contains("mDNSResponder"), "{p}");
    assert!(!p.contains("(allow network*)"));
    assert!(!p.contains("network-inbound"));
    assert!(
      p.contains("(deny mach-lookup")
        && p.contains("(global-name \"com.apple.dnssd.service\")")
    );
    assert!(!p.contains(
      "(allow mach-lookup (global-name \"com.apple.dnssd.service\"))"
    ));
  }
}
