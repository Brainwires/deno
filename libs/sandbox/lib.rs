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
//! - [`proxy`] (the `proxy` feature): the allow-listing HTTP / `CONNECT`
//!   proxy a confined process with a host list reaches the network through.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
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
