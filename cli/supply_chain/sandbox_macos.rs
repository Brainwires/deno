// Copyright 2018-2026 the Deno authors. MIT license.

//! The OS confinement of a jailed lifecycle script on macOS: the script's
//! process tree runs under `sandbox-exec` (Seatbelt) with a profile that
//! allows writes only beneath the jail's roots, denies reads of credential
//! files, and denies the network (IP, the DNS resolver, and every Unix socket
//! but the resolver's when the network is allowed). Seatbelt checks the
//! resolved path, so a symlink in a writable root can't lead outside it.

use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

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
  net: bool,
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
  if net {
    p.push_str(
      "(allow network-outbound (remote unix-socket (path-literal \"/private/var/run/mDNSResponder\")))\n",
    );
  } else {
    p.push_str("(deny network-outbound (remote ip))\n");
    // DNS lookups go through mDNSResponder, which would carry data out
    p.push_str(
      "(deny mach-lookup (global-name \"com.apple.dnssd.service\"))\n",
    );
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
      false,
    );
    assert!(p.contains("(subpath \"/a \\\"b\\\"/c\\\\d\")"), "{p}");
    assert!(p.contains("(deny file-read* (subpath \"/home/.ssh\"))"));
    assert!(p.contains("(deny network-outbound (remote ip))"));
    let p = profile(&[], &[], true);
    assert!(!p.contains("(remote ip)"));
    assert!(p.contains("mDNSResponder"));
  }
}
