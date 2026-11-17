// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: whether the kernel sandbox enforces this process's grant
//! (`--kernel-sandbox=enforce`), set once by the CLI when it applies, read
//! where a refusal is reported: the permission-error note (deno_core's
//! `to_v8_error` hook) and the child-exit hint below.

use std::sync::Once;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

static ENFORCING: AtomicBool = AtomicBool::new(false);
static INHERITED: AtomicBool = AtomicBool::new(false);

/// Records that the kernel sandbox now enforces this process's grant.
pub fn set_enforcing() {
  ENFORCING.store(true, Ordering::SeqCst);
}

/// Whether the kernel sandbox enforces this process's grant.
pub fn is_enforcing() -> bool {
  ENFORCING.load(Ordering::SeqCst)
}

/// Records that this process runs inside a kernel sandbox a parent applied
/// (the inherited policy was accepted: kernel-verified), whether or not it
/// applies a layer of its own.
pub fn set_inherited() {
  INHERITED.store(true, Ordering::SeqCst);
}

/// Whether a kernel sandbox holds this process: its own
/// ([`is_enforcing`]) or one it inherited ([`set_inherited`]). Decides the
/// refusal note and the child hint; never whether a plan still has to be
/// applied.
pub fn is_confined() -> bool {
  is_enforcing() || INHERITED.load(Ordering::SeqCst)
}

/// The note appended to a permission error the kernel sandbox may explain.
pub const REFUSED_NOTE: &str =
  " (refused by the kernel sandbox; see --kernel-sandbox-report)";

/// A child process exited unsuccessfully: inside a kernel sandbox, says
/// once per run that the sandbox may have refused it something (children
/// inherit the grant).
pub fn note_child_failed(code: i32) {
  child_hint(&format!("exited with code {code}"));
}

/// A child process was killed by `signal` (its name, `SIGABRT`): inside a
/// kernel sandbox, the signals a refusal can end a program with (`SIGABRT`:
/// macOS's dyld aborts a program it can't load; `SIGSYS`: a seccomp kill)
/// get the same hint; signals a parent or the user sends don't.
pub fn note_child_signaled(signal: &str) {
  if matches!(signal, "SIGABRT" | "SIGSYS") {
    child_hint(&format!("was killed by {signal}"));
  }
}

fn child_hint(what: &str) {
  if !is_confined() {
    return;
  }
  static HINT: Once = Once::new();
  HINT.call_once(|| {
    #[allow(clippy::print_stderr, reason = "a one-line hint, once per run")]
    {
      eprintln!(
        "hint: a child process {what} inside the kernel sandbox; children inherit the file and network rules, so it may have been refused something (see --kernel-sandbox-report)"
      );
    }
  });
}
