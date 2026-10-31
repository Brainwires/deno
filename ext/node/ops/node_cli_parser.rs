// Copyright 2018-2026 the Deno authors. MIT license.

//! Node.js CLI Argument Parser - Uses node_shim crate
//!
//! This module uses the node_shim crate to parse Node.js CLI arguments
//! and translates them to Deno CLI arguments.

use deno_core::ToV8;
use deno_core::op2;

#[derive(Debug, thiserror::Error, deno_error::JsError)]
#[class(generic)]
pub enum CliParserError {
  #[error(
    "Failed to parse Node.js CLI arguments: {message}. If you believe this is a valid Node.js flag, please report it at https://github.com/denoland/deno/issues"
  )]
  ParseError { message: String },
}

pub use node_shim::DebugOptions;
pub use node_shim::EnvironmentOptions;
pub use node_shim::HostPort;
pub use node_shim::InspectPublishUid;
pub use node_shim::OptionEnvvarSettings;
pub use node_shim::OptionType;
pub use node_shim::OptionsParser;
pub use node_shim::ParseResult;
pub use node_shim::PerIsolateOptions;
pub use node_shim::PerProcessOptions;
pub use node_shim::TranslateOptions;
pub use node_shim::TranslatedArgs as NodeShimTranslatedArgs;
// Re-export types from node_shim for use elsewhere in Deno
pub use node_shim::parse_args;
pub use node_shim::parse_node_options_env_var;
pub use node_shim::translate_to_deno_args as translate_to_deno_args_impl;
pub use node_shim::wrap_eval_code;

/// Result of translating Node.js CLI args to Deno args
#[derive(Debug, Clone, ToV8)]
pub struct TranslatedArgs {
  /// The Deno CLI arguments
  pub deno_args: Vec<String>,
  /// Node options that should be added to NODE_OPTIONS env var
  pub node_options: Vec<String>,
  /// CA store configuration that should be passed to the spawned Deno process.
  pub ca_stores: Option<Vec<String>>,
  /// Whether the translated flags selected OpenSSL/system-only CA handling.
  pub use_openssl_ca: bool,
  /// Whether the child process needs npm process state
  pub needs_npm_process_state: bool,
  /// Comma-separated trace event categories from --trace-event-categories,
  /// to be propagated via DENO_NODE_TRACE_EVENT_CATEGORIES.
  pub trace_event_categories: Option<String>,
}

/// Translate parsed Node.js CLI arguments to Deno CLI arguments.
/// This is used by child_process when spawning a Deno process as Node.js.
fn translate_to_deno_args(
  parsed_args: ParseResult,
  script_in_npm_package: bool,
  wrap_eval: bool,
) -> TranslatedArgs {
  let options = if wrap_eval {
    TranslateOptions::for_child_process()
  } else {
    TranslateOptions::for_shell_command()
  };
  let use_openssl_ca = parsed_args.options.use_openssl_ca;
  let use_system_ca = parsed_args.options.use_system_ca;
  let use_bundled_ca = parsed_args.options.use_bundled_ca;
  let result = translate_to_deno_args_impl(parsed_args, &options);
  let ca_stores = if use_openssl_ca {
    Some(vec!["system".to_string()])
  } else {
    match (use_system_ca, use_bundled_ca) {
      (true, _) => Some(vec!["mozilla".to_string(), "system".to_string()]),
      (false, true) => Some(vec!["mozilla".to_string()]),
      (false, false) => None,
    }
  };

  let trace_event_categories = if result.trace_event_categories.is_empty() {
    None
  } else {
    Some(result.trace_event_categories)
  };

  TranslatedArgs {
    deno_args: result.deno_args,
    node_options: result.node_options,
    ca_stores,
    use_openssl_ca,
    needs_npm_process_state: script_in_npm_package,
    trace_event_categories,
  }
}

/// Op that parses Node.js CLI arguments and translates them to Deno CLI arguments.
/// Returns an object with deno_args, node_options, and needs_npm_process_state.
/// Throws an error if parsing fails - this helps identify unsupported flags
/// so they can be added to node_shim.
///
/// When `wrap_eval` is true, eval code is wrapped for Node.js compatibility
/// (used for direct child_process spawning). When false, eval code is passed
/// through as-is (used for shell command transformation).
#[op2]
pub fn op_node_translate_cli_args(
  #[scoped] args: Vec<String>,
  script_in_npm_package: bool,
  wrap_eval: bool,
) -> Result<TranslatedArgs, CliParserError> {
  translate_cli_args(args, script_in_npm_package, wrap_eval)
}

/// [`op_node_translate_cli_args`]'s body, callable outside the op.
fn translate_cli_args(
  args: Vec<String>,
  script_in_npm_package: bool,
  wrap_eval: bool,
) -> Result<TranslatedArgs, CliParserError> {
  // If no args, return early with run -A --ext=cjs -
  // `-` tells Deno to read from stdin, matching Node.js behavior where
  // `node` with no args reads and executes piped stdin as CommonJS.
  if args.is_empty() {
    return Ok(TranslatedArgs {
      deno_args: vec![
        "run".to_string(),
        "-A".to_string(),
        "--ext=cjs".to_string(),
        "-".to_string(),
      ],
      node_options: vec![],
      ca_stores: None,
      use_openssl_ca: false,
      needs_npm_process_state: script_in_npm_package,
      trace_event_categories: None,
    });
  }

  // Parse the args
  // denext (DENEXT.md item 189): `process.execArgv` holds the parent's Deno
  // flags, so `fork()` (whose default execArgv it is) and
  // `spawn(process.execPath, [...process.execArgv, script])` hand them back
  // here in Node's position. They are not Node flags: take them out before
  // Node's parser sees them and give them to the translated subcommand.
  let (deno_flags, args) = split_child_deno_flags(args);
  if args.is_empty() && !deno_flags.is_empty() {
    // Only Deno flags: Node reads a script from stdin.
    let mut translated =
      translate_cli_args(vec![], script_in_npm_package, wrap_eval)?;
    insert_child_deno_flags(&mut translated.deno_args, &[], deno_flags);
    return Ok(translated);
  }
  match parse_args(args.clone()) {
    Ok(parsed) => {
      let mut translated =
        translate_to_deno_args(parsed, script_in_npm_package, wrap_eval);
      insert_child_deno_flags(&mut translated.deno_args, &args, deno_flags);
      Ok(translated)
    }
    Err(unknown_flags) => Err(CliParserError::ParseError {
      message: unknown_flags.join(", "),
    }),
  }
}

/// The long Deno flags (without `--`) `process.execArgv` may carry (cli's
/// `FlagsExt::to_exec_argv`) that take no value or an optional one.
const CHILD_DENO_FLAGS: &[&str] = &[
  "allow-all",
  "allow-read",
  "allow-write",
  "allow-net",
  "allow-net-connect",
  "allow-net-listen",
  "allow-env",
  "allow-run",
  "allow-sys",
  "allow-ffi",
  "allow-import",
  "deny-read",
  "deny-write",
  "deny-net",
  "deny-net-connect",
  "deny-net-listen",
  "deny-env",
  "deny-run",
  "deny-sys",
  "deny-ffi",
  "deny-import",
  "ignore-env",
  "ignore-read",
  "permission-set",
  "unsafely-ignore-certificate-errors",
  "unstable",
  "no-detect-cjs",
  "no-config",
  "node-modules-dir",
  "no-lock",
  "frozen",
  "frozen-lockfile",
  "cached-only",
  "no-remote",
  "no-npm",
];

/// The long Deno flags `process.execArgv` carries only as `--flag=value`.
const CHILD_DENO_VALUE_FLAGS: &[&str] = &[
  "config",
  "import-map",
  "lock",
  "cert",
  "location",
  "seed",
  "v8-flags",
];

/// The permission flags among [`CHILD_DENO_FLAGS`].
fn is_child_permission_flag(arg: &str) -> bool {
  let name = arg.split('=').next().unwrap_or(arg);
  name == "-A"
    || name.starts_with("--allow-")
    || name.starts_with("--deny-")
    || name.starts_with("--ignore-")
    || name == "--permission-set"
}

/// Whether `arg` is a Deno flag of a parent's `process.execArgv` (none of
/// them is a Node flag: Node's permission flags are `--allow-fs-read` and
/// the like, and `--conditions` stays with Node's parser, which knows it).
fn is_child_deno_flag(arg: &str) -> bool {
  if arg == "-A" {
    return true;
  }
  let Some(long) = arg.strip_prefix("--") else {
    return false;
  };
  let (name, value) = match long.split_once('=') {
    Some((name, value)) => (name, Some(value)),
    None => (long, None),
  };
  if name.starts_with("unstable-") {
    return value.is_none();
  }
  CHILD_DENO_FLAGS.contains(&name)
    || (value.is_some() && CHILD_DENO_VALUE_FLAGS.contains(&name))
}

/// Splits the Deno flags of [`is_child_deno_flag`] off the flags before the
/// script (the first argument that is not a flag, or `--`); the script and
/// its arguments are never looked at. Returns `(deno flags, the rest)`.
fn split_child_deno_flags(args: Vec<String>) -> (Vec<String>, Vec<String>) {
  let mut deno_flags = Vec::new();
  let mut rest = Vec::with_capacity(args.len());
  let mut in_flags = true;
  for arg in args {
    if in_flags && (arg == "--" || !arg.starts_with('-')) {
      in_flags = false;
    }
    if in_flags && is_child_deno_flag(&arg) {
      deno_flags.push(arg);
    } else {
      rest.push(arg);
    }
  }
  (deno_flags, rest)
}

/// Puts a parent's Deno flags right after the translated subcommand (`run`,
/// `serve`, `test` or `bench`), skipping any it already has. When they
/// include a permission flag, the `-A` the translation adds for a Node
/// child is dropped (a Node child has no permission model; this one gets the
/// parent's grants, as Node's children get its flags), unless the caller
/// asked for `-A` itself (`node_args`). Any other translation (`eval`,
/// `--version`, ...) takes none of them.
fn insert_child_deno_flags(
  deno_args: &mut Vec<String>,
  node_args: &[String],
  deno_flags: Vec<String>,
) {
  if deno_flags.is_empty() {
    return;
  }
  let Some(at) = deno_args
    .iter()
    .take(2)
    .position(|a| matches!(a.as_str(), "run" | "serve" | "test" | "bench"))
  else {
    return;
  };
  let mut at = at + 1;
  if deno_flags.iter().any(|f| is_child_permission_flag(f))
    && deno_args.get(at).is_some_and(|a| a == "-A")
    && !node_args.iter().any(|a| a == "-A")
  {
    deno_args.remove(at);
  }
  for flag in deno_flags {
    if !deno_args.contains(&flag) {
      deno_args.insert(at, flag);
      at += 1;
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Macro to create a Vec<String> from string literals
  macro_rules! svec {
        ($($x:expr),* $(,)?) => {
            vec![$($x.to_string()),*]
        };
    }

  #[test]
  fn child_deno_flags_are_split_off_before_the_script() {
    let (deno, rest) = split_child_deno_flags(svec![
      "--allow-read=/data",
      "--unstable-kv",
      "--max-old-space-size=100",
      "--config=/app/deno.json",
      "--seed",
      "--cert=/c.pem",
      "--allow-fs-read=/x",
      "--v8-flags=--expose-gc",
      "-A",
      "child.js",
      "--allow-net",
      "--unstable-cron"
    ]);
    assert_eq!(
      deno,
      svec![
        "--allow-read=/data",
        "--unstable-kv",
        "--config=/app/deno.json",
        "--cert=/c.pem",
        "--v8-flags=--expose-gc",
        "-A"
      ]
    );
    // Node's own flags (Node's `--allow-fs-read` included), a value flag
    // without `=`, the script and its arguments stay.
    assert_eq!(
      rest,
      svec![
        "--max-old-space-size=100",
        "--seed",
        "--allow-fs-read=/x",
        "child.js",
        "--allow-net",
        "--unstable-cron"
      ]
    );
    let (deno, rest) =
      split_child_deno_flags(svec!["--", "--allow-read", "child.js"]);
    assert!(deno.is_empty());
    assert_eq!(rest, svec!["--", "--allow-read", "child.js"]);
  }

  #[test]
  fn child_deno_flags_reach_the_translated_run() {
    let translated = translate_cli_args(
      svec!["--allow-read=/data", "--unstable-kv", "child.js", "a"],
      false,
      true,
    )
    .unwrap();
    let args = translated.deno_args;
    assert_eq!(args[0], "run");
    // The parent's grants, not `-A`.
    assert!(!args.contains(&"-A".to_string()), "{args:?}");
    assert_eq!(args[1], "--allow-read=/data", "{args:?}");
    assert_eq!(args[2], "--unstable-kv", "{args:?}");
    assert_eq!(&args[args.len() - 2..], &svec!["child.js", "a"]);

    // Without a permission flag the child keeps `-A`, as before; Node flags
    // around the Deno ones are still translated.
    let translated = translate_cli_args(
      svec!["--unstable-kv", "--max-old-space-size=100", "child.js"],
      false,
      true,
    )
    .unwrap();
    let args = translated.deno_args;
    assert_eq!(&args[..3], &svec!["run", "--unstable-kv", "-A"], "{args:?}");
    assert!(
      args.iter().any(|a| a.starts_with("--v8-flags=")
        && a.contains("--max-old-space-size=100")),
      "{args:?}"
    );

    // A Deno flag the translation already adds is not repeated.
    let translated = translate_cli_args(
      svec!["--unstable-detect-cjs", "child.js"],
      false,
      true,
    )
    .unwrap();
    let args = translated.deno_args;
    assert_eq!(
      args
        .iter()
        .filter(|a| *a == "--unstable-detect-cjs")
        .count(),
      1,
      "{args:?}"
    );

    // An explicit Deno invocation keeps the caller's own `-A`.
    let translated = translate_cli_args(
      svec!["--deny-env", "run", "-A", "child.js"],
      false,
      true,
    )
    .unwrap();
    assert_eq!(
      translated.deno_args,
      svec!["run", "--deny-env", "-A", "child.js"]
    );

    // No Deno flags: unchanged.
    let translated =
      translate_cli_args(svec!["child.js"], false, true).unwrap();
    assert_eq!(&translated.deno_args[..2], &svec!["run", "-A"]);
  }

  #[test]
  fn test_basic_parsing() {
    let result = parse_args(svec!["--version"]).unwrap();
    assert!(result.options.print_version);
  }

  #[test]
  fn test_help_parsing() {
    let result = parse_args(svec!["--help"]).unwrap();
    assert!(result.options.print_help);
  }

  #[test]
  fn test_debug_options() {
    let result = parse_args(svec!["--inspect"]).unwrap();
    assert!(
      result
        .options
        .per_isolate
        .per_env
        .debug_options
        .inspector_enabled
    );
  }

  #[test]
  fn test_string_option() {
    let result = parse_args(svec!["--title", "myapp"]).unwrap();
    assert_eq!(result.options.title, "myapp");
  }

  #[test]
  fn test_boolean_negation() {
    let result = parse_args(svec!["--no-warnings"]).unwrap();
    assert!(!result.options.per_isolate.per_env.warnings);
  }

  #[test]
  fn test_alias_expansion() {
    let result = parse_args(svec!["-v"]).unwrap();
    assert!(result.options.print_version);
  }

  #[test]
  fn test_node_options_parsing() {
    let env_args =
      parse_node_options_env_var("--inspect --title \"my app\"").unwrap();
    assert_eq!(env_args, vec!["--inspect", "--title", "my app"]);
  }

  #[test]
  fn test_host_port_parsing() {
    let result = parse_args(svec!["--inspect-port", "127.0.0.1:9229"]).unwrap();
    assert_eq!(
      result
        .options
        .per_isolate
        .per_env
        .debug_options
        .host_port
        .host,
      "127.0.0.1"
    );
    assert_eq!(
      result
        .options
        .per_isolate
        .per_env
        .debug_options
        .host_port
        .port,
      9229
    );
  }

  #[test]
  fn test_translate_basic_script() {
    let parsed = parse_args(svec!["script.js"]).unwrap();
    let result = translate_to_deno_args(parsed, false, true);
    assert_eq!(
      result.deno_args,
      svec![
        "run",
        "-A",
        "--unstable-bare-node-builtins",
        "--unstable-detect-cjs",
        "script.js"
      ]
    );
    assert!(result.node_options.is_empty());
    assert!(!result.needs_npm_process_state);
  }

  #[test]
  fn test_translate_version() {
    let parsed = parse_args(svec!["--version"]).unwrap();
    let result = translate_to_deno_args(parsed, false, true);
    assert_eq!(result.deno_args, svec!["--version"]);
  }

  #[test]
  fn test_translate_help() {
    let parsed = parse_args(svec!["--help"]).unwrap();
    let result = translate_to_deno_args(parsed, false, true);
    assert_eq!(result.deno_args, svec!["--help"]);
  }

  #[test]
  fn test_translate_eval() {
    let parsed = parse_args(svec!["--eval", "console.log(42)"]).unwrap();
    let result = translate_to_deno_args(parsed, false, true);
    // Eval code should be wrapped for child_process
    assert!(result.deno_args.contains(&"eval".to_string()));
    // Note: deno eval has implicit permissions, so -A is not added
    // The wrapped code should contain process.getBuiltinModule("vm").runInThisContext
    assert!(result.deno_args.iter().any(|a| {
      a.contains(r#"process.getBuiltinModule("vm").runInThisContext"#)
    }));
  }

  #[test]
  fn test_translate_inspect() {
    let parsed = parse_args(svec!["--inspect", "script.js"]).unwrap();
    let result = translate_to_deno_args(parsed, false, true);
    assert!(
      result
        .deno_args
        .contains(&"--inspect=127.0.0.1:9229".to_string())
    );
    assert!(result.deno_args.contains(&"script.js".to_string()));
  }

  #[test]
  fn test_translate_inspect_brk() {
    let parsed = parse_args(svec!["--inspect-brk", "script.js"]).unwrap();
    let result = translate_to_deno_args(parsed, false, true);
    assert!(
      result
        .deno_args
        .contains(&"--inspect-brk=127.0.0.1:9229".to_string())
    );
  }

  #[test]
  fn test_translate_watch() {
    let parsed = parse_args(svec!["--watch", "script.js"]).unwrap();
    let result = translate_to_deno_args(parsed, false, true);
    assert!(result.deno_args.contains(&"--watch".to_string()));
  }

  #[test]
  fn test_translate_no_warnings() {
    let parsed = parse_args(svec!["--no-warnings", "script.js"]).unwrap();
    let result = translate_to_deno_args(parsed, false, true);
    assert!(result.deno_args.contains(&"--quiet".to_string()));
    assert!(result.node_options.contains(&"--no-warnings".to_string()));
  }

  #[test]
  fn test_translate_conditions() {
    let parsed =
      parse_args(svec!["--conditions", "development", "script.js"]).unwrap();
    let result = translate_to_deno_args(parsed, false, true);
    assert!(
      result
        .deno_args
        .contains(&"--conditions=development".to_string())
    );
  }

  #[test]
  fn test_translate_conditions_equals_format() {
    // Test the --conditions=custom format (with equals sign)
    let parsed = parse_args(svec!["--conditions=custom", "script.js"]).unwrap();
    let result = translate_to_deno_args(parsed, false, true);
    assert!(
      result
        .deno_args
        .contains(&"--conditions=custom".to_string()),
    );
  }

  #[test]
  fn test_translate_conditions_short_alias() {
    // Test -C custom format (short alias)
    let parsed = parse_args(svec!["-C", "custom", "script.js"]).unwrap();
    let result = translate_to_deno_args(parsed, false, true);
    assert!(
      result
        .deno_args
        .contains(&"--conditions=custom".to_string()),
    );
  }

  #[test]
  fn test_translate_v8_flags() {
    let parsed =
      parse_args(svec!["--max-old-space-size=4096", "script.js"]).unwrap();
    let result = translate_to_deno_args(parsed, false, true);
    assert!(result.deno_args.iter().any(|a| a.contains("--v8-flags=")));
  }

  #[test]
  fn test_translate_repl() {
    let parsed = parse_args(svec![]).unwrap();
    let result = translate_to_deno_args(parsed, false, true);
    // REPL should have empty deno_args (triggers Deno's REPL behavior)
    assert!(result.deno_args.is_empty());
  }

  #[test]
  fn test_translate_npm_package() {
    let parsed = parse_args(svec!["script.js"]).unwrap();
    let result = translate_to_deno_args(parsed, true, true);
    assert!(result.needs_npm_process_state);
  }

  #[test]
  fn test_translate_run_script() {
    let parsed = parse_args(svec!["--run", "build"]).unwrap();
    let result = translate_to_deno_args(parsed, false, true);
    assert_eq!(result.deno_args, svec!["task", "build"]);
  }

  #[test]
  fn test_translate_test_runner() {
    let parsed = parse_args(svec!["--test", "test.js"]).unwrap();
    let result = translate_to_deno_args(parsed, false, true);
    assert!(result.deno_args.contains(&"test".to_string()));
    assert!(result.deno_args.contains(&"-A".to_string()));
    assert!(result.deno_args.contains(&"test.js".to_string()));
  }

  #[test]
  fn test_translate_test_with_watch() {
    let parsed = parse_args(svec!["--test", "--watch", "test.js"]).unwrap();
    let result = translate_to_deno_args(parsed, false, true);
    assert!(result.deno_args.contains(&"test".to_string()));
    assert!(result.deno_args.contains(&"--watch".to_string()));
  }

  #[test]
  fn test_translate_subcommand_passthrough() {
    // Deno subcommands spawned via node:child_process should be passed through
    // unchanged rather than being treated as a script for `deno run`. See
    // #35591.
    for subcommand in ["bundle", "serve", "fmt", "compile", "lint"] {
      let parsed =
        parse_args(svec![subcommand, "jsr:@std/http/file-server"]).unwrap();
      let result = translate_to_deno_args(parsed, false, true);
      assert_eq!(result.deno_args[0], subcommand);
      assert!(!result.deno_args.contains(&"run".to_string()));
    }
  }

  #[test]
  fn test_wrap_eval_code() {
    let wrapped = wrap_eval_code("console.log(42)");
    assert!(
      wrapped.contains(r#"process.getBuiltinModule("vm").runInThisContext"#)
    );
    assert!(wrapped.contains("process.getBuiltinModule"));
    assert!(wrapped.contains("\"console.log(42)\""));
  }
}
