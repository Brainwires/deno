// Copyright 2018-2026 the Deno authors. MIT license.

//! OS confinement shared by the policy producers that use it (denext): the
//! lifecycle-script jail (`cli/supply_chain/jail.rs`) today, and the kernel
//! enforcement of `--allow-*` / `--deny-*` for `deno` and `denort` later.
//! The producers decide WHAT is allowed; the enforcers here turn that into
//! what each mechanism takes:
//!
//! - [`linux`]: Landlock rulesets, the user + network namespace handoff, the
//!   child subreaper, and applying a ruleset plus a seccomp program to a
//!   command between `fork` and `exec`.
//! - [`seccomp`]: a classic-BPF seccomp program built from a [`SeccompSpec`]
//!   (per-syscall rules, then the `socket(2)` family filter).
//! - [`seatbelt`]: the deny-by-default Seatbelt (SBPL) profile text; pure
//!   string generation, so it builds (and is tested) on every platform.
//! - [`macos`]: running a profile under `sandbox-exec`.
//! - [`notify`]: a seccomp user-notification supervisor for signals, file
//!   metadata and truncation, which Landlock doesn't confine everywhere.
//! - [`proxy`] (the `proxy` feature): the allow-listing HTTP / `CONNECT`
//!   proxy a confined process with a host list reaches the network through.
//! - [`plan`]: the enforcement plan a process applies to itself under
//!   `--kernel-sandbox=enforce` (compiled by the CLI from its policy model),
//!   and [`enforce_linux`]: applying it on Linux (exec, then restrict:
//!   Landlock and one seccomp program, the plan handed over in a sealed
//!   memfd); on macOS [`macos`] re-executes too and the new image applies
//!   its Seatbelt profile first thing (`sandbox_init`).
//! - [`exec`]: the files a program list needs to execute (a strict exec
//!   scope: the programs, their `#!` interpreters, the ELF loader).
//! - [`inherit`]: the carrier that hands a confined process's policy to the
//!   `deno` processes it starts (Linux and macOS).

#[cfg(target_os = "linux")]
pub mod enforce_linux;
#[cfg(unix)]
pub mod exec;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod inherit;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "linux")]
pub mod notify;
pub mod plan;
#[cfg(feature = "proxy")]
pub mod proxy;
pub mod seatbelt;
#[cfg(target_os = "linux")]
pub mod seccomp;

#[cfg(target_os = "linux")]
pub use seccomp::SeccompSpec;

/// What the OS sandbox lets a confined process tree connect to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsNet {
  Denied,
  Open,
  /// Only the allow-listing proxy on this loopback port.
  ProxyOnly(u16),
}

/// What a confined process may read under the user's real home directories
/// (the account's home and, when it differs, `$HOME`): there, only the paths
/// in `allowed` (canonical, each inside one of `homes`); the enforcers' own
/// read denies stay in force within them too. The jail gives one to a
/// lifecycle script with network (DENEXT.md item 437).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeReadPolicy {
  pub homes: Vec<std::path::PathBuf>,
  pub allowed: Vec<std::path::PathBuf>,
}

impl HomeReadPolicy {
  /// Whether reading beneath `path` would reach something the policy keeps
  /// out: `path` is in a home directory but in none of the allowed paths, or
  /// holds a home directory.
  pub fn keeps_out(&self, path: &std::path::Path) -> bool {
    self.homes.iter().any(|home| {
      home.starts_with(path)
        || (path.starts_with(home)
          && !self.allowed.iter().any(|a| path.starts_with(a)))
    })
  }
}

#[cfg(test)]
mod tests {
  use std::path::Path;
  use std::path::PathBuf;

  use super::HomeReadPolicy;

  #[test]
  fn home_read_policy_keeps_out_the_rest_of_each_home() {
    let policy = HomeReadPolicy {
      homes: vec![PathBuf::from("/home/u"), PathBuf::from("/tmp/h")],
      allowed: vec![PathBuf::from("/home/u/proj"), PathBuf::from("/tmp/h/p")],
    };
    for kept_out in ["/home/u/.aws", "/home/u", "/home", "/", "/tmp/h/x"] {
      assert!(policy.keeps_out(Path::new(kept_out)), "{kept_out}");
    }
    for readable in ["/home/u/proj", "/home/u/proj/lib", "/tmp/h/p", "/usr"] {
      assert!(!policy.keeps_out(Path::new(readable)), "{readable}");
    }
  }
}
