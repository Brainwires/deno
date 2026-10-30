// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: what a panic may say about the command that panicked.
//!
//! The CLI's panic banner printed the whole command line (`Args: [...]`), and
//! a command line can carry secrets (`--env` values, tokens in URLs, script
//! arguments), which then land in container and CI logs. The banner names
//! only the subcommand, when it is one of Deno's own, and how many arguments
//! there were.

use std::ffi::OsStr;
use std::ffi::OsString;

/// The subcommands a panic report may name. Anything else (a script path, a
/// script argument, a typo) is reported as unknown.
const SUBCOMMANDS: &[&str] = &[
  "add",
  "approve-builds",
  "approve-scripts",
  "audit",
  "bench",
  "bump-version",
  "bundle",
  "cache",
  "check",
  "ci",
  "clean",
  "compile",
  "completions",
  "coverage",
  "create",
  "deploy",
  "desktop",
  "doc",
  "eval",
  "fmt",
  "help",
  "i",
  "info",
  "init",
  "install",
  "jupyter",
  "link",
  "lint",
  "list",
  "lsp",
  "outdated",
  "pack",
  "publish",
  "remove",
  "repl",
  "rm",
  "run",
  "sandbox",
  "serve",
  "sync-types",
  "task",
  "test",
  "transpile",
  "types",
  "uninstall",
  "unlink",
  "update",
  "upgrade",
  "vendor",
  "why",
  "x",
];

/// The subcommand of a command line (`argv[0]` excluded): its first word that
/// is not a flag, when that word is one of Deno's subcommands; `None`
/// otherwise.
pub fn subcommand_from_args<S: AsRef<OsStr>>(
  args: &[S],
) -> Option<&'static str> {
  let word = args
    .iter()
    .map(|a| a.as_ref())
    .find(|a| !a.as_encoded_bytes().starts_with(b"-"))?;
  let word = word.to_str()?;
  SUBCOMMANDS.iter().copied().find(|s| *s == word)
}

/// The line a panic banner prints about the command line: the subcommand
/// (or `unknown`) and the argument count, never an argument itself.
pub fn command_line_summary(args: &[OsString]) -> String {
  let rest = args.get(1..).unwrap_or_default();
  let count = rest.len();
  format!(
    "{} ({count} argument{})",
    subcommand_from_args(rest).unwrap_or("unknown"),
    if count == 1 { "" } else { "s" }
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
  }

  #[test]
  fn subcommand_is_the_first_non_flag_word() {
    assert_eq!(
      subcommand_from_args(&os(&["run", "-A", "main.ts"])),
      Some("run")
    );
    assert_eq!(
      subcommand_from_args(&os(&["-q", "--unstable", "test"])),
      Some("test")
    );
    assert_eq!(subcommand_from_args(&os(&["approve-scripts"])), Some("approve-scripts"));
  }

  #[test]
  fn subcommand_is_none_for_anything_else() {
    // A script path, a typo, a flag value or nothing at all.
    assert_eq!(subcommand_from_args(&os(&["main.ts", "run"])), None);
    assert_eq!(subcommand_from_args(&os(&["rnu"])), None);
    assert_eq!(
      subcommand_from_args(&os(&["--log-level", "debug", "run"])),
      None
    );
    assert_eq!(subcommand_from_args(&os(&["--version"])), None);
    assert_eq!(subcommand_from_args::<OsString>(&[]), None);
    assert_eq!(subcommand_from_args(&os(&["https://x.test/?token=s3cret"])), None);
  }

  #[test]
  fn summary_never_carries_an_argument() {
    let args = os(&[
      "/usr/bin/deno",
      "run",
      "--env-file=.env",
      "main.ts",
      "--token=s3cret",
    ]);
    let line = command_line_summary(&args);
    assert_eq!(line, "run (4 arguments)");
    assert!(!line.contains("s3cret"));
    assert_eq!(
      command_line_summary(&os(&["deno", "s3cret.ts"])),
      "unknown (1 argument)"
    );
    assert_eq!(command_line_summary(&os(&["deno"])), "unknown (0 arguments)");
    assert_eq!(command_line_summary(&[]), "unknown (0 arguments)");
  }
}
