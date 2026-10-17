// Copyright 2018-2026 the Deno authors. MIT license.

//! `npm` as Deno: the command line of the `npm` shim that
//! `deno x --install-node-shims` writes, translated to Deno's own commands.
//!
//! | npm                                   | deno                               |
//! | ------------------------------------- | ---------------------------------- |
//! | `npm install` / `npm i`               | `deno install`                     |
//! | `npm install <pkg>...` (`-D`, `-E`)   | `deno add npm:<pkg>...`            |
//! | `npm install -g <pkg>...`             | `deno install -g -A -f npm:<pkg>`  |
//! | `npm ci`                              | `deno ci`                          |
//! | `npm uninstall <pkg>...`              | `deno remove <pkg>...`             |
//! | `npm run <script> [-- args]`          | `deno task <script> [args]`        |
//! | `npm test` / `start` / `stop` / `restart` | `deno task <name>`             |
//! | `npm exec [-y] [-p pkg] [--] <cmd>`   | `deno x [-y] [-p pkg] <cmd>`       |
//! | `npm init <initializer>` / `create`   | `deno init --npm <initializer>`    |
//! | `npm init [-y]`                       | writes a package.json (npm's)      |
//!
//! Anything else fails with a message naming what the shim supports, rather
//! than doing something npm wouldn't.

/// What the shim makes of an `npm` command line (without the program name).
#[derive(Debug, PartialEq, Eq)]
pub enum NpmTranslation {
  /// Run deno with these arguments (without the program name).
  Deno(Vec<String>),
  /// Print this to stdout and exit 0 (`npm help`).
  Print(String),
}

const SUPPORTED: &str = "install (i, add), ci, uninstall (rm, remove), run (run-script), test (t), start, stop, restart, exec (x), init (create)";

/// npm's flags that change nothing Deno does (output, audit, peer
/// resolution strategy, caching).
const IGNORED_FLAGS: &[&str] = &[
  "--no-audit",
  "--audit",
  "--no-fund",
  "--fund",
  "--legacy-peer-deps",
  "--strict-peer-deps",
  "--prefer-offline",
  "--prefer-online",
  "--no-progress",
  "--progress",
  "--silent",
  "-s",
  "--quiet",
  "-q",
  "--verbose",
  "-d",
  "--foreground-scripts",
  "--no-update-notifier",
  "--save",
  "-S",
  "--save-prod",
  "-P",
  "--color",
  "--no-color",
];

/// npm's flags that take a value (as `--flag value` or `--flag=value`) and
/// change nothing Deno does.
const IGNORED_VALUE_FLAGS: &[&str] = &["--loglevel", "--registry", "--cache"];

pub fn translate(args: &[String]) -> Result<NpmTranslation, String> {
  let Some(command) = args.first() else {
    return Ok(NpmTranslation::Print(usage()));
  };
  let rest = &args[1..];
  let deno = |v: Vec<String>| Ok(NpmTranslation::Deno(v));
  match command.as_str() {
    "-h" | "--help" | "help" => Ok(NpmTranslation::Print(usage())),
    "install" | "i" | "in" | "ins" | "inst" | "insta" | "instal" | "isnt"
    | "isnta" | "isntal" | "isntall" | "add" => deno(install(rest)?),
    "ci" | "clean-install" | "ic" | "install-clean" | "isntall-clean" => {
      deno(ci(rest)?)
    }
    "uninstall" | "unlink" | "remove" | "rm" | "r" | "un" => {
      deno(uninstall(rest)?)
    }
    "run" | "run-script" | "rum" | "urn" => deno(run(rest)?),
    "test" | "tst" | "t" => deno(run_named("test", rest)?),
    "start" | "stop" | "restart" => deno(run_named(command, rest)?),
    "exec" | "x" => deno(exec(rest)?),
    "init" | "create" | "innit" => deno(init(rest)?),
    other => Err(format!(
      "`npm {other}` is not supported by Deno's npm shim. Supported: {SUPPORTED}."
    )),
  }
}

fn usage() -> String {
  format!(
    "npm, run by Deno: npm's common commands mapped onto deno's.\n\n\
     Supported: {SUPPORTED}.\n\
     Run `deno help` for everything else.\n"
  )
}

/// Splits npm's own flags from the positional arguments, rejecting the flags
/// a command doesn't know; `--` ends the flags.
struct Parsed {
  flags: Vec<String>,
  positional: Vec<String>,
}

fn parse(
  command: &str,
  args: &[String],
  known: &[&str],
  known_with_value: &[&str],
) -> Result<Parsed, String> {
  let mut flags = Vec::new();
  let mut positional = Vec::new();
  let mut iter = args.iter().peekable();
  while let Some(arg) = iter.next() {
    if arg == "--" {
      positional.extend(iter.by_ref().cloned());
      break;
    }
    if !arg.starts_with('-') || arg == "-" {
      positional.push(arg.clone());
      continue;
    }
    let (name, inline_value) = match arg.split_once('=') {
      Some((name, value)) if name.starts_with("--") => {
        (name, Some(value.to_string()))
      }
      _ => (arg.as_str(), None),
    };
    if IGNORED_FLAGS.contains(&name) {
      continue;
    }
    if IGNORED_VALUE_FLAGS.contains(&name) {
      if inline_value.is_none() {
        iter.next();
      }
      continue;
    }
    if known.contains(&name) {
      flags.push(name.to_string());
      continue;
    }
    if known_with_value.contains(&name) {
      let value = match inline_value {
        Some(value) => value,
        None => iter
          .next()
          .cloned()
          .ok_or_else(|| format!("`npm {command} {name}` needs a value"))?,
      };
      flags.push(format!("{name}={value}"));
      continue;
    }
    return Err(format!(
      "`npm {command}`: the flag `{arg}` is not supported by Deno's npm shim"
    ));
  }
  Ok(Parsed { flags, positional })
}

fn flag_value<'a>(flag: &'a str, name: &str) -> Option<&'a str> {
  flag.strip_prefix(name)?.strip_prefix('=')
}

/// A package argument as `deno add` takes it: npm's registry specifiers
/// (`name`, `name@range`, `@scope/name@tag`) get the `npm:` prefix.
fn package_spec(spec: &str) -> Result<String, String> {
  if spec.starts_with("npm:") || spec.starts_with("jsr:") {
    return Ok(spec.to_string());
  }
  let unsupported = spec.starts_with('.')
    || spec.starts_with('/')
    || spec.starts_with('~')
    || spec.contains("://")
    || spec.starts_with("file:")
    || spec.starts_with("git")
    || spec.starts_with("github:")
    || spec.ends_with(".tgz")
    || spec.contains('\\');
  // `user/repo` is a GitHub shorthand, `@scope/name` a package
  if unsupported || (!spec.starts_with('@') && spec.contains('/')) {
    return Err(format!(
      "`{spec}`: Deno's npm shim installs registry packages only"
    ));
  }
  Ok(format!("npm:{spec}"))
}

fn install(args: &[String]) -> Result<Vec<String>, String> {
  let parsed = parse(
    "install",
    args,
    &[
      "--save-dev",
      "-D",
      "--save-optional",
      "-O",
      "--save-exact",
      "-E",
      "--no-save",
      "--global",
      "-g",
      "--production",
      "--ignore-scripts",
      "--no-package-lock",
    ],
    &["--omit", "--package-lock"],
  )?;
  let has = |names: &[&str]| parsed.flags.iter().any(|f| names.contains(&&**f));
  let global = has(&["--global", "-g"]);
  let prod = has(&["--production"])
    || parsed
      .flags
      .iter()
      .any(|f| flag_value(f, "--omit") == Some("dev"));
  let no_lock = has(&["--no-package-lock"])
    || parsed
      .flags
      .iter()
      .any(|f| flag_value(f, "--package-lock") == Some("false"));
  warn_ignore_scripts(&parsed.flags);

  if parsed.positional.is_empty() {
    if global {
      return Err("`npm install -g` needs a package".to_string());
    }
    let mut out = vec!["install".to_string()];
    if prod {
      out.push("--prod".to_string());
    }
    if no_lock {
      out.push("--no-lock".to_string());
    }
    return Ok(out);
  }

  let specs = parsed
    .positional
    .iter()
    .map(|s| package_spec(s))
    .collect::<Result<Vec<_>, _>>()?;
  if global {
    // npm's global install puts the package's bins on PATH with Node's
    // (unrestricted) permissions, replacing an earlier install
    let mut out = vec![
      "install".to_string(),
      "--global".to_string(),
      "--allow-all".to_string(),
      "--force".to_string(),
    ];
    out.extend(specs);
    return Ok(out);
  }
  let mut out = vec!["add".to_string()];
  if has(&["--save-dev", "-D"]) {
    out.push("--dev".to_string());
  }
  if has(&["--save-optional", "-O"]) {
    out.push("--save-optional".to_string());
  }
  if has(&["--save-exact", "-E"]) {
    out.push("--save-exact".to_string());
  }
  if has(&["--no-save"]) {
    out.push("--no-save".to_string());
  }
  if no_lock {
    out.push("--no-lock".to_string());
  }
  out.extend(specs);
  Ok(out)
}

fn ci(args: &[String]) -> Result<Vec<String>, String> {
  let parsed = parse(
    "ci",
    args,
    &["--production", "--ignore-scripts"],
    &["--omit"],
  )?;
  if !parsed.positional.is_empty() {
    return Err("`npm ci` takes no packages".to_string());
  }
  warn_ignore_scripts(&parsed.flags);
  let mut out = vec!["ci".to_string()];
  if parsed
    .flags
    .iter()
    .any(|f| f == "--production" || flag_value(f, "--omit") == Some("dev"))
  {
    out.push("--prod".to_string());
  }
  Ok(out)
}

fn warn_ignore_scripts(flags: &[String]) {
  if flags.iter().any(|f| f == "--ignore-scripts") {
    #[allow(clippy::print_stderr, reason = "npm shim, before logging")]
    {
      eprintln!(
        "npm shim: --ignore-scripts is not supported; dependencies' scripts run only when allowed (--allow-scripts), the project's own still run"
      );
    }
  }
}

fn uninstall(args: &[String]) -> Result<Vec<String>, String> {
  let parsed = parse(
    "uninstall",
    args,
    &["--save-dev", "-D", "--global", "-g", "--no-save"],
    &[],
  )?;
  if parsed.positional.is_empty() {
    return Err("`npm uninstall` needs a package".to_string());
  }
  let mut out = vec!["remove".to_string()];
  if parsed.flags.iter().any(|f| f == "--global" || f == "-g") {
    out.push("--global".to_string());
  }
  out.extend(parsed.positional.iter().map(|spec| {
    // `deno remove` takes the dependency's name
    spec.strip_prefix("npm:").unwrap_or(spec).to_string()
  }));
  Ok(out)
}

/// npm's run flags as `deno task`'s, with the script and its arguments.
fn task(
  script: Option<&str>,
  flags: &[String],
  script_args: &[String],
) -> Vec<String> {
  let mut out = vec!["task".to_string()];
  for flag in flags {
    match flag.as_str() {
      "--if-present" => out.push("--if-present".to_string()),
      "--workspaces" | "-ws" => out.push("--recursive".to_string()),
      _ => {
        if let Some(name) =
          flag_value(flag, "--workspace").or_else(|| flag_value(flag, "-w"))
        {
          out.push(format!("--filter={name}"));
        } else if let Some(dir) = flag_value(flag, "--prefix") {
          out.push(format!("--cwd={dir}"));
        }
      }
    }
  }
  if let Some(script) = script {
    out.push(script.to_string());
  }
  out.extend(script_args.iter().cloned());
  out
}

const RUN_FLAGS: &[&str] = &["--if-present", "--workspaces", "-ws"];
const RUN_VALUE_FLAGS: &[&str] = &["--workspace", "-w", "--prefix"];

/// npm takes its own flags before the script name; everything after the
/// script (a leading `--` dropped) is the script's.
fn run(args: &[String]) -> Result<Vec<String>, String> {
  let script_at = first_positional(args, RUN_VALUE_FLAGS);
  let (npm_args, script_and_args) = args.split_at(script_at);
  let parsed = parse("run", npm_args, RUN_FLAGS, RUN_VALUE_FLAGS)?;
  let Some((script, script_args)) = script_and_args.split_first() else {
    // `npm run` lists the scripts
    return Ok(task(None, &parsed.flags, &[]));
  };
  Ok(task(
    Some(script),
    &parsed.flags,
    strip_separator(script_args),
  ))
}

fn run_named(script: &str, args: &[String]) -> Result<Vec<String>, String> {
  // `npm test [flags] [-- args]`: args up to `--` may be npm's flags
  let (npm_args, script_args) = match args.iter().position(|a| a == "--") {
    Some(i) => (&args[..i], &args[i + 1..]),
    None => (args, &[][..]),
  };
  let parsed = parse(script, npm_args, RUN_FLAGS, RUN_VALUE_FLAGS)?;
  let mut all_args = parsed.positional;
  all_args.extend(script_args.iter().cloned());
  Ok(task(Some(script), &parsed.flags, &all_args))
}

/// The index of the first argument that is neither a flag nor a flag's value.
fn first_positional(args: &[String], value_flags: &[&str]) -> usize {
  let mut i = 0;
  while i < args.len() {
    let arg = &args[i];
    if arg == "--" || !arg.starts_with('-') {
      return i;
    }
    if (value_flags.contains(&arg.as_str())
      || IGNORED_VALUE_FLAGS.contains(&arg.as_str()))
      && !arg.contains('=')
    {
      i += 1;
    }
    i += 1;
  }
  args.len()
}

fn strip_separator(args: &[String]) -> &[String] {
  match args.first() {
    Some(first) if first == "--" => &args[1..],
    _ => args,
  }
}

fn exec(args: &[String]) -> Result<Vec<String>, String> {
  let command_at = first_positional(args, &["--package", "-p"]);
  let (npm_args, command) = args.split_at(command_at);
  let parsed = parse(
    "exec",
    npm_args,
    &["--yes", "-y", "--no"],
    &["--package", "-p"],
  )?;
  let command = strip_separator(command);
  if command.is_empty() {
    return Err("`npm exec` needs a command".to_string());
  }
  let mut out = vec!["x".to_string()];
  for flag in &parsed.flags {
    if flag == "--yes" || flag == "-y" {
      out.push("--yes".to_string());
    } else if let Some(pkg) =
      flag_value(flag, "--package").or_else(|| flag_value(flag, "-p"))
    {
      out.push(format!("--package={pkg}"));
    }
  }
  out.extend(command.iter().cloned());
  Ok(out)
}

fn init(args: &[String]) -> Result<Vec<String>, String> {
  let initializer_at = first_positional(args, &[]);
  let (npm_args, initializer) = args.split_at(initializer_at);
  let parsed = parse("init", npm_args, &["--yes", "-y", "--force", "-f"], &[])?;
  match strip_separator(initializer).split_first() {
    Some((initializer, initializer_args)) => {
      // `npm init <initializer>` runs `create-<initializer>`, as
      // `deno init --npm` does
      let mut out = vec!["init".to_string(), "--npm".to_string()];
      if parsed.flags.iter().any(|f| f == "--yes" || f == "-y") {
        out.push("--yes".to_string());
      }
      out.push(initializer.clone());
      out.extend(initializer_args.iter().cloned());
      Ok(out)
    }
    // `npm init` asks npm's questions; the shim answers them as `npm init -y`
    None => Ok(vec![
      "eval".to_string(),
      "--quiet".to_string(),
      WRITE_PACKAGE_JSON.to_string(),
    ]),
  }
}

/// `npm init -y`: a package.json with npm's defaults, keeping what an
/// existing one has.
const WRITE_PACKAGE_JSON: &str = r#"const path = "package.json";
let existing = {};
try {
  existing = JSON.parse(Deno.readTextFileSync(path));
} catch (err) {
  if (!(err instanceof Deno.errors.NotFound)) throw err;
}
const dir = Deno.cwd();
const name = dir.split(/[\\/]/).pop().toLowerCase()
  .replace(/^[._]+/, "").replace(/[^a-z0-9._~-]+/g, "-") || "package";
const pkg = {
  name,
  version: "1.0.0",
  description: "",
  main: "index.js",
  scripts: { test: 'echo "Error: no test specified" && exit 1' },
  keywords: [],
  author: "",
  license: "ISC",
  ...existing,
};
const text = JSON.stringify(pkg, null, 2) + "\n";
Deno.writeTextFileSync(path, text);
console.log(`Wrote to ${dir}${dir.includes("\\") ? "\\" : "/"}${path}:\n\n${text}`);
"#;

#[cfg(test)]
mod tests {
  use super::*;

  fn t(args: &str) -> Vec<String> {
    let args = args
      .split_whitespace()
      .map(String::from)
      .collect::<Vec<_>>();
    match translate(&args).unwrap() {
      NpmTranslation::Deno(v) => v,
      NpmTranslation::Print(_) => panic!("printed"),
    }
  }

  fn err(args: &str) -> String {
    let args = args
      .split_whitespace()
      .map(String::from)
      .collect::<Vec<_>>();
    translate(&args).unwrap_err()
  }

  #[test]
  fn install() {
    assert_eq!(t("install"), ["install"]);
    assert_eq!(t("i --no-audit --no-fund"), ["install"]);
    assert_eq!(t("ci --omit=dev"), ["ci", "--prod"]);
    assert_eq!(t("install --production"), ["install", "--prod"]);
    assert_eq!(t("i express"), ["add", "npm:express"]);
    assert_eq!(
      t("install -D -E typescript@5 @types/node@latest"),
      [
        "add",
        "--dev",
        "--save-exact",
        "npm:typescript@5",
        "npm:@types/node@latest"
      ]
    );
    assert_eq!(t("add jsr:@std/path"), ["add", "jsr:@std/path"]);
    assert_eq!(
      t("install -g cowsay"),
      [
        "install",
        "--global",
        "--allow-all",
        "--force",
        "npm:cowsay"
      ]
    );
    assert_eq!(t("i --loglevel error zod"), ["add", "npm:zod"]);
    assert!(err("i ./local").contains("registry packages only"));
    assert!(err("i user/repo").contains("registry packages only"));
    assert!(err("i --frobnicate").contains("--frobnicate"));
  }

  #[test]
  fn uninstall() {
    assert_eq!(t("uninstall lodash"), ["remove", "lodash"]);
    assert_eq!(t("rm -g cowsay"), ["remove", "--global", "cowsay"]);
  }

  #[test]
  fn run() {
    assert_eq!(t("run"), ["task"]);
    assert_eq!(t("run build"), ["task", "build"]);
    assert_eq!(
      t("run build -- --watch x"),
      ["task", "build", "--watch", "x"]
    );
    assert_eq!(t("run build a b"), ["task", "build", "a", "b"]);
    assert_eq!(t("run --if-present lint"), ["task", "--if-present", "lint"]);
    assert_eq!(t("run -w pkg-a build"), ["task", "--filter=pkg-a", "build"]);
    assert_eq!(t("run --workspaces test"), ["task", "--recursive", "test"]);
    assert_eq!(t("run --prefix app dev"), ["task", "--cwd=app", "dev"]);
    assert_eq!(t("test"), ["task", "test"]);
    assert_eq!(t("t -- --grep x"), ["task", "test", "--grep", "x"]);
    assert_eq!(t("start"), ["task", "start"]);
  }

  #[test]
  fn exec() {
    assert_eq!(t("exec cowsay hi"), ["x", "cowsay", "hi"]);
    assert_eq!(
      t("exec -y --package=typescript -- tsc --version"),
      ["x", "--yes", "--package=typescript", "tsc", "--version"]
    );
    assert_eq!(
      t("x -p cowsay cowthink hi"),
      ["x", "--package=cowsay", "cowthink", "hi"]
    );
    assert!(err("exec").contains("needs a command"));
  }

  #[test]
  fn init() {
    assert_eq!(t("init vite my-app"), ["init", "--npm", "vite", "my-app"]);
    assert_eq!(t("create -y vite"), ["init", "--npm", "--yes", "vite"]);
    let plain = t("init -y");
    assert_eq!(plain[0], "eval");
    assert_eq!(t("init")[0], "eval");
  }

  #[test]
  fn unsupported() {
    assert!(err("publish").contains("not supported"));
    assert!(matches!(
      translate(&["--help".to_string()]).unwrap(),
      NpmTranslation::Print(_)
    ));
  }
}
