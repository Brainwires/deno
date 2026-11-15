// Copyright 2018-2026 the Deno authors. MIT license.

//! The OS confinement of a jailed lifecycle script on macOS: the script's
//! process tree runs under `sandbox-exec` (Seatbelt) with the deny-by-default
//! profile [`crate::seatbelt`] writes (DENEXT.md item 124).

use std::path::Path;
use std::sync::OnceLock;

pub use crate::seatbelt::profile;

pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Whether `sandbox-exec` works here (it is missing on some systems, and
/// refuses to run inside another sandbox).
#[allow(
  clippy::disallowed_methods,
  reason = "the real sandbox-exec binary, not a virtual file system"
)]
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
