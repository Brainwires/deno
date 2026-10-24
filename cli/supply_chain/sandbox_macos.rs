// Copyright 2018-2026 the Deno authors. MIT license.

//! The OS confinement of a jailed lifecycle script on macOS: the script's
//! process tree runs under `sandbox-exec` (Seatbelt) with a profile that
//! allows writes only beneath the jail's roots, denies reads of credential
//! files, and denies the network (IP, the DNS resolver, and every Unix socket
//! but the resolver's when the network is allowed). A script that may reach
//! only some hosts may connect only to its allow-listing proxy
//! (`localhost:<port>`, `jail_proxy`), with no DNS; without the whole
//! network a script can't listen for connections either. Seatbelt checks the
//! resolved path, so a symlink in a writable root can't lead outside it.

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

/// The Seatbelt profile.
pub fn profile(
  write_roots: &[PathBuf],
  deny_read: &[PathBuf],
  net: OsNet,
) -> String {
  let mut p =
    String::from("(version 1)\n(allow default)\n(deny file-write*)\n");
  p.push_str("(allow file-write*");
  for root in write_roots {
    p.push_str(&format!(" (subpath {})", quote(root)));
  }
  for dev in [
    "/dev/null",
    "/dev/zero",
    "/dev/tty",
    "/dev/dtracehelper",
    "/dev/stdout",
    "/dev/stderr",
  ] {
    p.push_str(&format!(" (literal \"{dev}\")"));
  }
  p.push_str(" (regex #\"^/dev/fd/\") (regex #\"^/dev/ttys[0-9]+$\"))\n");
  if !deny_read.is_empty() {
    p.push_str("(deny file-read*");
    for path in deny_read {
      p.push_str(&format!(" (subpath {})", quote(path)));
    }
    p.push_str(")\n");
  }
  // Unix sockets reach other daemons (a Docker socket runs anything): none,
  // whatever the network setting, but the DNS resolver's when it's allowed
  p.push_str("(deny network-outbound (remote unix-socket))\n");
  match net {
    OsNet::Open => {
      p.push_str(
        "(allow network-outbound (remote unix-socket (path-literal \"/private/var/run/mDNSResponder\")))\n",
      );
      return p;
    }
    OsNet::Denied => {
      p.push_str("(deny network-outbound (remote ip))\n");
    }
    OsNet::ProxyOnly(port) => {
      // the last matching rule wins: the proxy's port on the loopback
      // interface only (TCP and UDP to anywhere else are denied)
      p.push_str("(deny network-outbound (remote ip))\n");
      p.push_str(&format!(
        "(allow network-outbound (remote ip \"localhost:{port}\"))\n"
      ));
    }
  }
  // nor serving: a listening socket would hand data to whoever connects in
  p.push_str("(deny network-inbound (local ip))\n");
  // DNS lookups go through mDNSResponder, which would carry data out; the
  // proxy resolves names itself
  p.push_str("(deny mach-lookup (global-name \"com.apple.dnssd.service\"))\n");
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
    assert!(p.contains("(deny file-read* (subpath \"/home/.ssh\"))"));
    assert!(p.contains("(deny network-outbound (remote ip))"));
    assert!(p.contains("com.apple.dnssd.service"));
    assert!(p.contains("(deny network-inbound (local ip))"));
    let p = profile(&[], &[], OsNet::Open);
    assert!(!p.contains("network-inbound"));
    assert!(!p.contains("(remote ip)"));
    assert!(p.contains("mDNSResponder"));
    assert!(!p.contains("com.apple.dnssd.service"));
  }

  // DENEXT.md item 111: only the proxy's port, no DNS
  #[test]
  fn profile_proxy_only() {
    let p = profile(&[], &[], OsNet::ProxyOnly(4321));
    let deny = p.find("(deny network-outbound (remote ip))").unwrap();
    let allow = p
      .find("(allow network-outbound (remote ip \"localhost:4321\"))")
      .unwrap();
    // later rules win in Seatbelt
    assert!(deny < allow, "{p}");
    assert!(p.contains("(deny network-outbound (remote unix-socket))"));
    assert!(!p.contains("mDNSResponder"), "{p}");
    assert!(p.contains("(deny network-inbound (local ip))"));
    assert!(p.contains("com.apple.dnssd.service"));
  }
}
