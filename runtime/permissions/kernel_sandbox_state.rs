// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: whether the kernel sandbox enforces this process's grant
//! (`--kernel-sandbox=enforce`), set once by the CLI when it applies, read
//! where a refusal is reported: the permission-error note (deno_core's
//! `to_v8_error` hook) and the child-exit hint below.

use std::sync::Once;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

static ENFORCING: AtomicBool = AtomicBool::new(false);

/// Records that the kernel sandbox now enforces this process's grant.
pub fn set_enforcing() {
  ENFORCING.store(true, Ordering::SeqCst);
}

/// Whether the kernel sandbox enforces this process's grant.
pub fn is_enforcing() -> bool {
  ENFORCING.load(Ordering::SeqCst)
}

/// The note appended to a permission error the kernel sandbox may explain.
pub const REFUSED_NOTE: &str =
  " (refused by the kernel sandbox; see --kernel-sandbox-report)";

/// A child process exited unsuccessfully: under enforcement, says once per
/// run that the kernel sandbox may have refused it something (children
/// inherit the grant).
pub fn note_child_failed(code: i32) {
  if !is_enforcing() {
    return;
  }
  static HINT: Once = Once::new();
  HINT.call_once(|| {
    #[allow(clippy::print_stderr, reason = "a one-line hint, once per run")]
    {
      eprintln!(
        "hint: a child process exited with code {code} while the kernel sandbox was enforcing; children inherit the file and network rules, so it may have been refused something (see --kernel-sandbox-report)"
      );
    }
  });
}
