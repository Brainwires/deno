// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: the inherited kernel policy (DENEXT.md items 449, 450). The
//! carrier, its acceptance and the intersection live in
//! `deno_lib::kernel_inherit`, shared with `denort`; this module adds what
//! only the CLI does with them: refusing the subcommands that write the
//! project, the lockfile or the caches inside a kernel sandbox.

use deno_core::anyhow::bail;
use deno_core::error::AnyError;
pub use deno_lib::kernel_inherit::*;

use crate::args::DenoSubcommand;
use crate::args::Flags;

/// Whether `subcommand` may run inside a kernel sandbox an ancestor applied:
/// the ones that run, check or describe code, as in a jailed lifecycle
/// script (`supply_chain::jail::check_subcommand`). The rest install or
/// fetch packages, or write the project, the lockfile or the caches outside
/// the permission system (`deno install`, `add`, `remove`, `cache`, `fmt`,
/// `compile`, ...), which the sandbox would refuse half-way with a bare
/// `EACCES`.
pub fn subcommand_allowed_when_confined(subcommand: &DenoSubcommand) -> bool {
  matches!(
    subcommand,
    DenoSubcommand::Run(_)
      | DenoSubcommand::Eval(_)
      | DenoSubcommand::Task(_)
      | DenoSubcommand::Repl(_)
      | DenoSubcommand::Test(_)
      | DenoSubcommand::Bench(_)
      | DenoSubcommand::Check(_)
      | DenoSubcommand::Info(_)
      | DenoSubcommand::Types
      | DenoSubcommand::Help(_)
      | DenoSubcommand::Completions(_)
      | DenoSubcommand::Serve(_)
  )
}

/// Inside a kernel sandbox an ancestor applied (a carrier accepted, or the
/// marker with grants unknown), refuses the subcommands
/// [`subcommand_allowed_when_confined`] leaves out, before they start.
pub fn check_subcommand(flags: &Flags) -> Result<(), AnyError> {
  if inherited_layers().is_empty()
    || subcommand_allowed_when_confined(&flags.subcommand)
  {
    return Ok(());
  }
  bail!(
    "This deno subcommand is not allowed inside a kernel sandbox: it installs or fetches packages, or writes the project, the lockfile or the caches, which the sandbox's grant doesn't cover. Run it outside the sandbox (before the confined run starts)."
  );
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::args::flags_from_vec;

  fn subcommand(args: &[&str]) -> DenoSubcommand {
    let args = args.iter().map(|a| (*a).into()).collect();
    flags_from_vec(args).unwrap().subcommand
  }

  #[test]
  fn writing_subcommands_are_refused_when_confined() {
    for args in [
      &["deno", "install"][..],
      &["deno", "add", "jsr:@std/path"],
      &["deno", "remove", "@std/path"],
      &["deno", "cache", "main.ts"],
      &["deno", "fmt"],
      &["deno", "compile", "main.ts"],
      &["deno", "outdated", "--update"],
      &["deno", "clean"],
      &["deno", "init"],
    ] {
      assert!(
        !subcommand_allowed_when_confined(&subcommand(args)),
        "{args:?}"
      );
    }
    for args in [
      &["deno", "run", "main.ts"][..],
      &["deno", "eval", "1"],
      &["deno", "test"],
      &["deno", "check", "main.ts"],
      &["deno", "task", "x"],
      &["deno", "serve", "main.ts"],
    ] {
      assert!(
        subcommand_allowed_when_confined(&subcommand(args)),
        "{args:?}"
      );
    }
  }

  // outside a kernel sandbox (the unit tests run in none), nothing is refused
  #[test]
  fn nothing_is_refused_outside_a_sandbox() {
    let flags = flags_from_vec(vec!["deno".into(), "install".into()]).unwrap();
    assert!(check_subcommand(&flags).is_ok());
  }
}
