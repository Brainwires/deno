// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: whether the kernel sandbox enforces this process's grant
//! (`--kernel-sandbox=enforce`), set once by the CLI when it applies, read
//! where a refusal is reported: the permission-error note (deno_core's
//! `to_v8_error` hook) and the child-exit hint below.

use std::sync::Once;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

static ENFORCING: AtomicBool = AtomicBool::new(false);
static WARNING: AtomicBool = AtomicBool::new(false);
static INHERITED: AtomicBool = AtomicBool::new(false);
static GRANTS_UNKNOWN: AtomicBool = AtomicBool::new(false);
static BAKED: AtomicBool = AtomicBool::new(false);
static REFUSAL_NOTED: AtomicBool = AtomicBool::new(false);

/// Records that the kernel sandbox now enforces this process's grant.
pub fn set_enforcing() {
  ENFORCING.store(true, Ordering::SeqCst);
}

/// Whether the kernel sandbox enforces this process's grant.
pub fn is_enforcing() -> bool {
  ENFORCING.load(Ordering::SeqCst)
}

/// Records that the kernel sandbox runs in warn mode in this process
/// (`--kernel-sandbox=warn`): nothing is refused, and what `enforce` would
/// refuse is reported (`deno_sandbox::would_deny`).
pub fn set_warning() {
  WARNING.store(true, Ordering::SeqCst);
}

/// Whether the kernel sandbox runs in warn mode in this process.
pub fn is_warning() -> bool {
  WARNING.load(Ordering::Relaxed)
}

/// Warn mode: the program is about to start `program` (a path or a name).
/// Reports once per program what is known about it inside a kernel sandbox:
/// the grant it needs of its own, or that it can't work there whatever is
/// granted. Says nothing about a program with nothing known, and nothing
/// outside warn mode.
pub fn note_spawn(program: &str) {
  if !is_warning() {
    return;
  }
  if let Some(advice) = deno_sandbox::would_deny::program_advisory(program) {
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    deno_sandbox::would_deny::report(
      deno_sandbox::would_deny::WouldDeny::ungrantable(
        deno_sandbox::would_deny::AccessClass::Exec,
        name,
        advice,
      )
      .from_source(deno_sandbox::would_deny::Source::Advisory),
    );
  }
}

/// Records that this process runs inside a kernel sandbox a parent applied
/// (the inherited policy was accepted: kernel-verified), whether or not it
/// applies a layer of its own.
pub fn set_inherited() {
  INHERITED.store(true, Ordering::SeqCst);
}

/// Records that a kernel sandbox of this runtime holds the process but the
/// grants it runs under weren't handed down (a program between them closed
/// the descriptor): every permission is refused, and each refusal says why
/// ([`GRANTS_UNKNOWN_NOTE`]).
pub fn set_grants_unknown() {
  GRANTS_UNKNOWN.store(true, Ordering::SeqCst);
}

/// See [`set_grants_unknown`].
pub fn grants_unknown() -> bool {
  GRANTS_UNKNOWN.load(Ordering::SeqCst)
}

/// The note on a `NotCapable` error while the grants are unknown.
pub const GRANTS_UNKNOWN_NOTE: &str = " (confined by an ancestor's kernel sandbox; grants unknown: every permission is refused because the descriptor that hands them down was closed on the way here)";

/// Whether a kernel sandbox holds this process: its own
/// ([`is_enforcing`]) or one it inherited ([`set_inherited`]). Decides the
/// refusal note and the child hint; never whether a plan still has to be
/// applied.
pub fn is_confined() -> bool {
  is_enforcing() || INHERITED.load(Ordering::SeqCst)
}

/// Records that this process's kernel sandbox mode was baked when it was
/// built (a compiled program, a packaged desktop app): no run-time switch
/// turns it off, so the first refusal names the build-time one.
pub fn set_baked() {
  BAKED.store(true, Ordering::SeqCst);
}

/// The switch that turns the kernel sandbox off for a `deno run`-family
/// command, named by the first refusal and the child hint.
macro_rules! run_opt_out {
  () => {
    "turn it off for the run with --kernel-sandbox=off, or in deno.json with \"permissions\": { \"kernelSandbox\": \"off\" }"
  };
}

/// The same for a program whose mode was baked ([`set_baked`]).
macro_rules! baked_opt_out {
  () => {
    "rebuild the program with `deno compile --kernel-sandbox=off` to turn it off"
  };
}

/// The note appended to a permission error the kernel sandbox may explain,
/// after the first one in the process ([`refused_note`]).
pub const REFUSED_NOTE: &str =
  " (refused by the kernel sandbox; see --kernel-sandbox-report)";

/// The first refusal's note in a `deno run`-family process: the error names
/// the refused access; the note says what allows it and how to opt out.
pub const FIRST_REFUSED_NOTE: &str = concat!(
  " (refused by the kernel sandbox; the --allow-* flag that covers this access grants it, or ",
  run_opt_out!(),
  "; see --kernel-sandbox-report)"
);

/// The first refusal's note in a program whose mode was baked.
pub const FIRST_REFUSED_NOTE_BAKED: &str = concat!(
  " (refused by the kernel sandbox; the --allow-* flag that covers this access grants it, or ",
  baked_opt_out!(),
  "; see --kernel-sandbox-report)"
);

/// The note for a refusal the kernel sandbox explains: the first one in the
/// process names the grant and the opt-out ([`FIRST_REFUSED_NOTE`],
/// [`FIRST_REFUSED_NOTE_BAKED`]), every later one is [`REFUSED_NOTE`].
/// Call it only once the error is known to be the sandbox's.
pub fn refused_note() -> &'static str {
  if REFUSAL_NOTED.swap(true, Ordering::SeqCst) {
    REFUSED_NOTE
  } else if BAKED.load(Ordering::SeqCst) {
    FIRST_REFUSED_NOTE_BAKED
  } else {
    FIRST_REFUSED_NOTE
  }
}

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
    let opt_out = if BAKED.load(Ordering::SeqCst) {
      baked_opt_out!()
    } else {
      run_opt_out!()
    };
    #[allow(clippy::print_stderr, reason = "a one-line hint, once per run")]
    {
      eprintln!(
        "hint: a child process {what} inside the kernel sandbox; children inherit the file and network rules, so it may have been refused something (see --kernel-sandbox-report); grant the child's access to this program, or {opt_out}"
      );
    }
  });
}

#[cfg(test)]
mod tests {
  use super::*;

  // the notes name the refusal, the grant and the opt-out; the first one in
  // a process is the long one, every later one is short
  #[test]
  fn refused_notes() {
    for note in [FIRST_REFUSED_NOTE, FIRST_REFUSED_NOTE_BAKED, REFUSED_NOTE] {
      assert!(
        note.starts_with(" (refused by the kernel sandbox; "),
        "{note}"
      );
      assert!(note.ends_with("see --kernel-sandbox-report)"), "{note}");
    }
    assert!(FIRST_REFUSED_NOTE.contains("--allow-*"));
    assert!(FIRST_REFUSED_NOTE.contains("--kernel-sandbox=off"));
    assert!(FIRST_REFUSED_NOTE.contains(r#""kernelSandbox": "off""#));
    assert!(
      FIRST_REFUSED_NOTE_BAKED.contains("deno compile --kernel-sandbox=off")
    );
    assert_eq!(refused_note(), FIRST_REFUSED_NOTE);
    assert_eq!(refused_note(), REFUSED_NOTE);
    assert_eq!(refused_note(), REFUSED_NOTE);
  }
}
