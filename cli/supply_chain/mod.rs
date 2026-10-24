// Copyright 2018-2026 the Deno authors. MIT license.

//! denext's npm supply-chain gates (DENEXT.md items 83-86):
//!
//! - [`advisories`]: refuse versions listed as malicious (OSV `MAL-*`).
//! - [`downloads`]: `deno add` of a package with very low weekly downloads
//!   warns (or, with `low-download-check=prompt`, asks first): a typosquat
//!   guard.
//! - [`jail`]: approved lifecycle scripts run confined; `jail_proxy` is the
//!   allow-listing proxy a script with a host list reaches the network
//!   through.
//! - `paranoid` (resolved in `deno_npmrc::resolve_supply_chain`): all of them
//!   on, each failing closed.

pub mod advisories;
pub mod downloads;
pub mod jail;
mod jail_proxy;
#[cfg(target_os = "linux")]
mod sandbox_linux;
#[cfg(target_os = "macos")]
mod sandbox_macos;

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

static GATE_REFUSED: AtomicBool = AtomicBool::new(false);

/// Records that a supply-chain gate refused an install, so `deno add` can put
/// the configuration files back as they were.
pub fn mark_gate_refused() {
  GATE_REFUSED.store(true, Ordering::SeqCst);
}

pub fn gate_refused() -> bool {
  GATE_REFUSED.load(Ordering::SeqCst)
}

/// A test hook for the gates' web services, from the environment (the spec
/// tests point them at a local fixture server).
pub(crate) fn service_url_from_env(
  env_var: &str,
  default: &str,
) -> deno_core::url::Url {
  #[allow(clippy::disallowed_methods, reason = "a fixed service URL override")]
  let value = std::env::var(env_var).ok().filter(|v| !v.trim().is_empty());
  let text = value.as_deref().unwrap_or(default);
  let text = if text.ends_with('/') {
    text.to_string()
  } else {
    format!("{text}/")
  };
  deno_core::url::Url::parse(&text).unwrap_or_else(|_| {
    log::warn!("Ignoring {env_var}: not a URL");
    deno_core::url::Url::parse(default).unwrap()
  })
}

/// Whether `DENO_NPM_DOWNLOADS_URL` / `DENO_OSV_URL` were set (the test
/// fixtures, or a mirror).
pub(crate) fn service_url_overridden(env_var: &str) -> bool {
  #[allow(clippy::disallowed_methods, reason = "a fixed service URL override")]
  std::env::var(env_var)
    .ok()
    .is_some_and(|v| !v.trim().is_empty())
}
