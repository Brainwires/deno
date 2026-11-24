// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: the kernel sandbox (DENEXT.md item 420), an opt-in preview.
//!
//! - [`policy`] (`deno_lib::kernel_policy`): the policy model (what each OS
//!   mechanism would apply, and the gaps), pure data.
//! - The mode decision lives in `deno_lib::kernel_sandbox`, shared with
//!   `denort`.
//! - This module wires them to a CLI run: [`check_run`] at startup
//!   (`--kernel-sandbox`, `--kernel-sandbox-report`), and [`info`] for
//!   `deno info --json` (`kernelSandbox`).
//! - [`enforce`]: applying the policy under `--kernel-sandbox=enforce`
//!   (Mode A on Linux and macOS, DENEXT.md item 425).
//! - [`inherit`]: the policy a confined process hands the `deno` processes
//!   it starts, and the intersection they apply (DENEXT.md item 449).
//!
//! Opt-in only: `auto` applies nothing in this release line, `warn`
//! reports, `enforce` applies the policy (or refuses to start when a rule
//! can't be held), and `broker` is `enforce` under a supervising broker
//! (Mode B: the file operations and the network). From the stage where
//! `auto` enforces, it takes the broker by itself for a grant with a rule
//! only the broker holds, instead of falling back to `warn`
//! ([`check_run`]).

pub mod enforce;
pub mod inherit;
#[cfg(unix)]
pub mod net_broker;
pub mod warn;

use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Once;

use deno_core::anyhow::bail;
use deno_core::error::AnyError;
use deno_core::serde_json;
pub use deno_lib::kernel_apply::detect_capabilities;
use deno_lib::kernel_apply::runtime_unix_sockets;
use deno_lib::kernel_apply::std_stream_ttys;
use deno_lib::kernel_apply::tls_files;
/// The policy model, shared with `denort` (`deno_lib::kernel_policy`).
pub use deno_lib::kernel_policy as policy;
use deno_lib::kernel_sandbox::AutoPlan;
use deno_lib::kernel_sandbox::AutoStage;
use deno_lib::kernel_sandbox::EnforceCoverage;
use deno_lib::kernel_sandbox::ForcedEnforce;
use deno_lib::kernel_sandbox::KERNEL_SANDBOX_ENV_VAR;
use deno_lib::kernel_sandbox::KERNEL_SANDBOX_FORCE_ENV_VAR;
use deno_lib::kernel_sandbox::KERNEL_SANDBOX_STAGE_ENV_VAR;
use deno_lib::kernel_sandbox::KernelSandboxMode;
use deno_lib::kernel_sandbox::ModeInputs;
use deno_lib::kernel_sandbox::ModeReason;
use deno_lib::kernel_sandbox::ModeResolution;
use deno_lib::kernel_sandbox::ModeSource;
use deno_lib::kernel_sandbox::RunKind;
use deno_lib::kernel_sandbox::kernel_sandbox_mode;
use deno_runtime::deno_permissions::PermissionsOptions;
use serde::Serialize;

use self::policy::Capabilities;
use self::policy::CompiledPolicy;
use self::policy::Context;
use self::policy::DenyRule;
use self::policy::ExecScope;
use self::policy::Feature;
use self::policy::GapResolution;
use self::policy::HostScope;
use self::policy::ImplicitSet;
use self::policy::KernelPolicy;
use self::policy::NetPolicy;
use self::policy::PathScope;
use self::policy::PortScope;
use self::policy::TargetOs;
use self::policy::UnixPolicy;
use crate::args::CliOptions;
use crate::args::DenoSubcommand;

/// The subcommand's kind, for the mode decision.
pub fn run_kind(subcommand: &DenoSubcommand) -> RunKind {
  match subcommand {
    DenoSubcommand::Run(_)
    | DenoSubcommand::Eval(_)
    | DenoSubcommand::Serve(_)
    | DenoSubcommand::Test(_)
    | DenoSubcommand::Bench(_) => RunKind::Run,
    DenoSubcommand::Repl(_) => RunKind::Repl,
    DenoSubcommand::Jupyter(_) => RunKind::Jupyter,
    _ => RunKind::NotARuntime,
  }
}

fn env_override_warned() -> Option<KernelSandboxMode> {
  match deno_lib::kernel_sandbox::env_override() {
    Ok(mode) => mode,
    Err(value) => {
      static WARN: Once = Once::new();
      WARN.call_once(|| {
        log::warn!(
          "{} {KERNEL_SANDBOX_ENV_VAR}={value:?} is ignored: it accepts only \"off\" and \"warn\"",
          crate::colors::yellow("Warning"),
        );
      });
      None
    }
  }
}

/// The harnesses' rollout stage (`DENO_INTERNAL_KERNEL_SANDBOX_STAGE`, item
/// 957); a value that isn't one is ignored with a warning.
fn stage_override_warned() -> Option<AutoStage> {
  match deno_lib::kernel_sandbox::stage_override() {
    Ok(stage) => stage,
    Err(value) => {
      static WARN: Once = Once::new();
      WARN.call_once(|| {
        log::warn!(
          "{} {KERNEL_SANDBOX_STAGE_ENV_VAR}={value:?} is ignored: it accepts only \"r3\" and \"r4\"",
          crate::colors::yellow("Warning"),
        );
      });
      None
    }
  }
}

/// How much of the command `enforce` covers: all of `run` of a file and
/// `serve`; `test` and `bench` after their own setup (the type check, the
/// coverage folder: the image before the re-exec finds and checks the
/// modules, the confined one runs exactly those); nothing of the others
/// (they refuse to start under an `enforce` asked for by name; one `auto`
/// chose runs them as `warn`, or, for a deny ceiling, as before).
fn enforce_covers(subcommand: &DenoSubcommand) -> EnforceCoverage {
  match subcommand {
    DenoSubcommand::Run(run) if !run.is_stdin() => EnforceCoverage::Full,
    DenoSubcommand::Serve(_) => EnforceCoverage::Full,
    DenoSubcommand::Test(_) | DenoSubcommand::Bench(_) => {
      EnforceCoverage::AfterSetup
    }
    _ => EnforceCoverage::None,
  }
}

/// What of this run would run user code, or need writes, before or beyond
/// what `enforce` covers of its command ([`enforce_covers`]), so that the
/// command isn't covered after all: `run --eszip-internal-do-not-use`
/// (runs no graph first), a `deno test` that loads a Vitest or Jest config
/// (its code runs while the tests are found, before the sandbox could
/// apply), `deno test --update-snapshots` (writes snapshot files beside the
/// tests, which no grant names), and a `test` / `bench` whose workspace
/// members bring permission sets of their own (`-P`: the kernel holds the
/// top-level grant only). `None`: nothing; else what to say about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Uncovered {
  Eszip,
  TestConfig,
  UpdateSnapshots,
  MemberPermissions,
}

impl Uncovered {
  fn describe(self) -> &'static str {
    match self {
      Self::Eszip => "enforce doesn't cover this command yet",
      Self::TestConfig => {
        "enforce doesn't cover a test run that loads a Vitest or Jest config yet"
      }
      Self::UpdateSnapshots => "enforce doesn't cover --update-snapshots yet",
      Self::MemberPermissions => {
        "enforce doesn't cover workspace members with permission sets of their own yet"
      }
    }
  }
}

/// [`enforce_covers`] for the command, less what [`Uncovered`] finds.
fn coverage_with(
  covered: EnforceCoverage,
  uncovered: Option<Uncovered>,
) -> (EnforceCoverage, Option<Uncovered>) {
  match (covered, uncovered) {
    (EnforceCoverage::None, _) => (EnforceCoverage::None, None),
    (_, Some(why)) => (EnforceCoverage::None, Some(why)),
    (covered, None) => (covered, None),
  }
}

/// What of this run [`enforce_covers`] can't see from the command alone
/// (see [`Uncovered`]).
fn uncovered_by(cli_options: &CliOptions) -> Option<Uncovered> {
  let flags = cli_options.flags();
  let root_options = || cli_options.permissions_options().ok();
  // a member whose permissions differ from the top-level grant (only with
  // -P / config permissions; each member's tests run with their own)
  let members_differ =
    |dirs: Vec<deno_config::workspace::WorkspaceDirectoryRc>| {
      let Some(root) = root_options() else {
        return false;
      };
      dirs.iter().any(|dir| {
        cli_options
          .permissions_options_for_dir(dir)
          .is_ok_and(|member| member != root)
      })
    };
  match &flags.subcommand {
    DenoSubcommand::Run(_) if flags.eszip => Some(Uncovered::Eszip),
    DenoSubcommand::Test(test_flags) => {
      let options = cli_options.resolve_workspace_test_options(test_flags);
      if options.vitest_compat
        && crate::tools::test::vitest::has_test_config(
          cli_options.initial_cwd(),
          test_flags.vitest.config.as_deref(),
        )
      {
        Some(Uncovered::TestConfig)
      } else if options.update_snapshots {
        Some(Uncovered::UpdateSnapshots)
      } else if cli_options
        .resolve_test_options_for_members(test_flags)
        .is_ok_and(|m| members_differ(m.into_iter().map(|(d, _)| d).collect()))
      {
        Some(Uncovered::MemberPermissions)
      } else {
        None
      }
    }
    DenoSubcommand::Bench(bench_flags) => cli_options
      .resolve_bench_options_for_members(bench_flags)
      .is_ok_and(|m| members_differ(m.into_iter().map(|(d, _)| d).collect()))
      .then_some(Uncovered::MemberPermissions),
    _ => None,
  }
}

/// `--kernel-sandbox`, parsed (the parser already checked the value).
fn flag_mode(
  cli_options: &CliOptions,
) -> Result<Option<KernelSandboxMode>, AnyError> {
  Ok(match &cli_options.flags().permissions.kernel_sandbox {
    Some(value) => Some(value.parse()?),
    None => None,
  })
}

/// deno.json `permissions.kernelSandbox`.
fn config_mode(
  cli_options: &CliOptions,
) -> Result<Option<KernelSandboxMode>, AnyError> {
  let config = cli_options.start_dir.to_permissions_config()?;
  Ok(match &config.kernel_sandbox {
    Some(value) => Some(value.parse()?),
    None => None,
  })
}

/// What `deno compile` is building, for [`compile_mode`].
#[derive(Debug, Clone, Copy)]
pub struct CompileKind<'a> {
  /// A desktop app (`deno desktop`): its process hosts a browser engine.
  pub desktop: bool,
  /// `--target` (`None`: this machine's).
  pub target: Option<&'a str>,
  /// Print nothing (a second call for the same build).
  pub quiet: bool,
}

impl CompileKind<'_> {
  /// The OS the program will run on.
  fn os(&self) -> TargetOs {
    match self.target {
      None => TargetOs::current(),
      Some(t) if t.contains("linux") => TargetOs::Linux,
      Some(t) if t.contains("darwin") => TargetOs::Macos,
      Some(t) if t.contains("windows") => TargetOs::Windows,
      Some(_) => TargetOs::Other,
    }
  }
}

/// The mode `deno compile` bakes into a binary: `--kernel-sandbox`, else
/// deno.json `permissions.kernelSandbox`, else nothing (`auto`). The program
/// applies it at startup, in place (DENEXT.md item 473). A mode the program
/// could never start with is refused here, so no such binary is made:
/// `enforce` for a target without an enforcer, and `broker` for one whose
/// compiled programs run no supervisor (Windows, a desktop app). For
/// `enforce` and `broker` the policy is compiled for the target OS with
/// every mechanism present, and the rules that OS can't hold are printed as
/// a warning (the program would refuse to start there; what the machine it
/// runs on lacks is only known at startup).
///
/// A desktop app (DENEXT.md item 489) takes `enforce` and `auto` on every
/// target. Under `enforce` (a strict package) its startup confines the
/// app's own process where its web engine can run inside the sandbox and
/// reports the app headless-only (or `none`) where it can't, so no desktop
/// app refuses to start over its engine; what the package gets for its
/// target and engine is printed here (`kernel_desktop::packaged_guarantee`).
/// `auto` applies nothing to a desktop app, at any stage
/// (`ModeReason::DesktopNotStrict`), and that is said here too.
pub fn compile_mode(
  cli_options: &CliOptions,
  kind: CompileKind,
) -> Result<Option<KernelSandboxMode>, AnyError> {
  let mode = match flag_mode(cli_options)? {
    Some(mode) => Some(mode),
    None => config_mode(cli_options)?,
  };
  match mode {
    Some(KernelSandboxMode::Broker) => {
      let os = kind.os();
      if !compile_target_supervises(os, kind.desktop) {
        bail!(
          "--kernel-sandbox=broker: {} run no supervising broker, so a program compiled with it could not start. Use --kernel-sandbox=enforce, warn, auto or off.",
          if kind.desktop {
            "desktop apps"
          } else {
            "programs for this target"
          }
        );
      }
      warn_compile_gaps(cli_options, os, KernelSandboxMode::Broker)?;
    }
    Some(KernelSandboxMode::Enforce) if kind.desktop => {
      if !kind.quiet {
        note_desktop_guarantee(
          kind.os(),
          deno_lib::kernel_sandbox::Engine::from_backend(
            cli_options.flags().internal.desktop_backend.as_deref(),
          ),
          KernelSandboxMode::Enforce,
        );
      }
    }
    Some(KernelSandboxMode::Auto) if kind.desktop => {
      // only a strict package is confined: `auto` leaves a desktop app
      // alone at every stage (`ModeReason::DesktopNotStrict`)
      if !kind.quiet {
        static NOTE: Once = Once::new();
        NOTE.call_once(|| {
          log::info!(
            "{} --kernel-sandbox=auto: {}; --kernel-sandbox=enforce confines it.",
            crate::colors::green("Kernel sandbox"),
            deno_lib::kernel_sandbox::ModeReason::DesktopNotStrict.describe(),
          );
        });
      }
    }
    Some(KernelSandboxMode::Enforce) => {
      let os = kind.os();
      if !compile_target_enforces(os, kind.desktop) {
        bail!(
          "--kernel-sandbox=enforce: {} have no kernel enforcer, so a program compiled with it could not start. Use --kernel-sandbox=warn, auto or off.",
          if kind.desktop {
            "desktop apps"
          } else {
            "programs for this target"
          }
        );
      }
      warn_compile_gaps(cli_options, os, KernelSandboxMode::Enforce)?;
    }
    _ => {}
  }
  Ok(mode)
}

/// Whether a compiled program for `os` runs its own supervising broker (it
/// forks in place before it confines itself: `deno_lib::kernel_apply`).
fn compile_target_supervises(os: TargetOs, desktop: bool) -> bool {
  !desktop && matches!(os, TargetOs::Linux | TargetOs::Macos)
}

/// Says, once per build, what a desktop app packaged with `mode` gets on
/// `os` with `engine`: confined in place, or headless guarantees only.
fn note_desktop_guarantee(
  os: TargetOs,
  engine: deno_lib::kernel_sandbox::Engine,
  mode: KernelSandboxMode,
) {
  use deno_lib::kernel_desktop::DesktopSandbox;
  let (sandbox, line) =
    deno_lib::kernel_desktop::packaged_guarantee(os, engine);
  static NOTE: Once = Once::new();
  NOTE.call_once(|| match sandbox {
    DesktopSandbox::Kernel => log::info!(
      "{} --kernel-sandbox={mode} ({}, {}): {line}",
      crate::colors::green("Kernel sandbox"),
      os.as_str(),
      engine.as_str(),
    ),
    DesktopSandbox::HeadlessOnly | DesktopSandbox::None => log::warn!(
      "{} --kernel-sandbox={mode} ({}, {}): {line}",
      crate::colors::yellow("Warning"),
      os.as_str(),
      engine.as_str(),
    ),
  });
}

/// Whether a program for `os` has an enforcer for a baked `enforce` (a
/// desktop app never reaches this: its startup decides per engine).
fn compile_target_enforces(os: TargetOs, desktop: bool) -> bool {
  !desktop
    && matches!(os, TargetOs::Linux | TargetOs::Macos | TargetOs::Windows)
}

/// The machine a compiled program is assumed to run on, for the build-time
/// warning: `os` with every mechanism its newest release has.
fn best_capabilities(os: TargetOs) -> Capabilities {
  let mut caps = Capabilities::none(os);
  match os {
    TargetOs::Linux => {
      caps.landlock_abi = Some(u32::MAX);
      caps.seccomp = true;
      caps.user_namespaces = true;
    }
    TargetOs::Macos => caps.seatbelt = true,
    TargetOs::Windows => {
      caps.job_object = true;
      caps.low_integrity = true;
    }
    TargetOs::Other => {}
  }
  caps.enforcer_built = true;
  caps
}

/// Warns, once, about the baked grant's rules the target OS could never
/// hold under `enforce` (the program would refuse to start there).
fn warn_compile_gaps(
  cli_options: &CliOptions,
  os: TargetOs,
  mode: KernelSandboxMode,
) -> Result<(), AnyError> {
  let options = cli_options.permissions_options()?;
  if policy::options_allow_all(&options) && !policy::options_have_deny(&options)
  {
    // plain -A: the program runs with the kernel sandbox off
    return Ok(());
  }
  let mut ctx = Context::new(
    best_capabilities(os),
    mode,
    cli_options.initial_cwd().to_path_buf(),
  );
  ctx.compiled = true;
  // (a desktop app is refused `enforce` before this; `broker` too)
  ctx.compiled_supervisor = compile_target_supervises(os, false);
  ctx.exec_strict = cli_options.flags().permissions.kernel_sandbox_exec_strict;
  let implicit = ImplicitSet::build(&options, &ctx);
  let compiled = KernelPolicy::compile(&options, &implicit, &ctx);
  let fatal: Vec<String> = compiled
    .fatal_gaps()
    .map(|g| format!("  - {}: {}", g.rule, g.detail))
    .collect();
  if !fatal.is_empty() {
    static WARN: Once = Once::new();
    WARN.call_once(|| {
      log::warn!(
        "{} --kernel-sandbox={mode}: {} can't hold these rules, so the compiled program would refuse to start there:\n{}",
        crate::colors::yellow("Warning"),
        os.as_str(),
        fatal.join("\n")
      );
    });
  }
  Ok(())
}

/// Whether this process runs under `denext dev` (its dev server hands its
/// processes `DENEXT_DEV_TOKEN`).
fn under_denext_dev() -> bool {
  std::env::var_os("DENEXT_DEV_TOKEN").is_some()
}

/// The harnesses' forced enforce (`DENO_INTERNAL_KERNEL_SANDBOX_FORCE`,
/// item 451), for the subcommands `enforce` covers (`run`, `serve`), and not
/// for a child of a confined process (its parent's sandbox already holds it;
/// nesting is the subprocess policy's).
fn forced_for(subcommand: &DenoSubcommand) -> Option<ForcedEnforce> {
  let forced = match deno_lib::kernel_sandbox::forced() {
    Ok(forced) => forced?,
    Err(value) => {
      static WARN: Once = Once::new();
      WARN.call_once(|| {
        log::warn!(
          "{} {KERNEL_SANDBOX_FORCE_ENV_VAR}={value:?} is ignored: it accepts only \"enforce\" and \"project\"",
          crate::colors::yellow("Warning"),
        );
      });
      return None;
    }
  };
  if !matches!(
    subcommand,
    DenoSubcommand::Run(_) | DenoSubcommand::Serve(_)
  ) {
    return None;
  }
  if enforce::confined_by_a_parent() {
    return None;
  }
  Some(forced)
}

/// The grant the kernel policy is compiled from: the run's own, or under the
/// harness's `project` force an unscoped read / write narrowed to the
/// workspace, the working directory and the temp folder (Deno's checks keep
/// the run's own grant, so what the kernel refuses beyond them shows what the
/// implicit set lacks).
fn policy_options(
  cli_options: &CliOptions,
  options: &PermissionsOptions,
) -> PermissionsOptions {
  if forced_for(&cli_options.flags().subcommand) != Some(ForcedEnforce::Project)
  {
    return options.clone();
  }
  let mut out = options.clone();
  let mut roots: Vec<String> = [
    cli_options.start_dir.workspace.root_dir_path(),
    cli_options.initial_cwd().to_path_buf(),
    // canonical: on macOS the image after the re-exec finds it by its
    // canonical path (`/private/var/folders/...`), and both images must
    // compile the same policy
    #[allow(clippy::disallowed_methods, reason = "the real temp folder")]
    {
      let temp = std::env::temp_dir();
      temp.canonicalize().unwrap_or(temp)
    },
  ]
  .iter()
  .filter_map(|p| p.to_str().map(|s| s.to_string()))
  .collect();
  roots.sort();
  roots.dedup();
  for list in [&mut out.allow_read, &mut out.allow_write] {
    if list.as_ref().is_some_and(|l| l.is_empty()) {
      *list = Some(roots.clone());
    }
  }
  out
}

fn mode_inputs(
  cli_options: &CliOptions,
  options: &PermissionsOptions,
) -> Result<ModeInputs, AnyError> {
  let flags = cli_options.flags();
  Ok(ModeInputs {
    forced: forced_for(&flags.subcommand),
    stage: stage_override_warned(),
    kind: Some(run_kind(&flags.subcommand)),
    flag: flag_mode(cli_options)?,
    env: env_override_warned(),
    config: config_mode(cli_options)?,
    baked: None,
    allow_all: policy::options_allow_all(options),
    has_deny: policy::options_have_deny(options),
    prompt: options.prompt,
    terminals:
      deno_runtime::deno_permissions::prompter::prompt_terminals_available(),
    watch: flags.watch.is_some(),
    hmr: cli_options.has_hmr(),
    denext_dev: under_denext_dev(),
    dev_build: false,
    permission_broker: deno_runtime::deno_permissions::broker::has_broker(),
    policy_needs_supervisor: false,
    lifecycle_jail: crate::supply_chain::jail::jail_from_env().is_some(),
  })
}

fn context_for(
  cli_options: &CliOptions,
  mode: KernelSandboxMode,
  deno_dir: Option<PathBuf>,
  node_modules: Option<PathBuf>,
) -> Context {
  let mut ctx = Context::new(
    detect_capabilities(),
    mode,
    cli_options.initial_cwd().to_path_buf(),
  );
  // canonical, as the re-exec starts it: on macOS `current_exe` is the path
  // the runtime was started by (a package manager's symlink), and the image
  // after the re-exec must compile the same policy (its hash keys the cache
  // overlay)
  #[allow(clippy::disallowed_methods, reason = "the runtime's real file")]
  {
    ctx.exe = std::env::current_exe()
      .ok()
      .map(|exe| exe.canonicalize().unwrap_or(exe));
  }
  #[allow(clippy::disallowed_methods, reason = "the user's real home")]
  {
    ctx.home = std::env::var_os("HOME")
      .filter(|h| !h.is_empty())
      .map(PathBuf::from);
  }
  ctx.deno_dir = deno_dir;
  ctx.node_modules = node_modules;
  ctx.vendor_dir = cli_options.vendor_dir_path().cloned();
  ctx.link_dirs = cli_options
    .start_dir
    .workspace
    .link_folders()
    .keys()
    .filter_map(|url| url.to_file_path().ok())
    .collect();
  // the config files discovery read: every deno.json / package.json of the
  // workspace (an ancestor folder's too, outside any read grant), the import
  // map and the lockfile. A re-executed image discovers them again under the
  // sandbox, and the compared policy holds the set, so both images must find
  // the same files. Never `.npmrc` (it can hold registry credentials): a
  // confined image treats an unreadable one as absent.
  let workspace = &cli_options.start_dir.workspace;
  let flags = cli_options.flags();
  #[allow(
    clippy::disallowed_methods,
    reason = "the files the real file system holds, as discovery read them"
  )]
  let mut config_files: Vec<PathBuf> = workspace
    .deno_jsons()
    .chain(workspace.link_deno_jsons())
    .filter_map(|c| c.specifier.to_file_path().ok())
    .chain(workspace.package_jsons().map(|p| p.path.clone()))
    .chain(workspace.link_pkg_jsons().map(|p| p.path.clone()))
    .chain(workspace.to_import_map_path().ok().flatten())
    // an import map given by --import-map (a local file)
    .chain(
      flags
        .import_map_path
        .as_ref()
        .filter(|p| !p.contains("://") || p.starts_with("file:"))
        .and_then(|p| {
          deno_path_util::resolve_url_or_path(p, cli_options.initial_cwd()).ok()
        })
        .and_then(|url| url.to_file_path().ok()),
    )
    // the tsconfig.json beside each of them: a package.json project resolves
    // its `compilerOptions.paths` / `baseUrl` from it at run time, in the
    // confined image too (its `extends` / `references` chain isn't followed
    // here: a file it names outside the read grant stays unreadable)
    .chain(
      workspace
        .deno_jsons()
        .filter_map(|c| c.specifier.to_file_path().ok())
        .chain(workspace.package_jsons().map(|p| p.path.clone()))
        .filter_map(|p| Some(p.parent()?.join("tsconfig.json")))
        .collect::<Vec<_>>(),
    )
    .filter(|p| p.is_file())
    .collect();
  // the package.json nearest the main module: the run reads it for the
  // module's type (`"type": "module"`) even when discovery skipped it
  // (`--no-config`), and the confined image reads it again
  let main_script = match &flags.subcommand {
    DenoSubcommand::Run(run_flags) if !run_flags.is_stdin() => {
      Some(run_flags.script.as_str())
    }
    DenoSubcommand::Serve(serve_flags) => Some(serve_flags.script.as_str()),
    _ => None,
  };
  if let Some(script) = main_script
    && let Ok(url) = cli_options.resolve_entrypoint_path(script)
    && let Ok(path) = url.to_file_path()
    && let Some(found) = path
      .ancestors()
      .skip(1)
      .map(|dir| dir.join("package.json"))
      .find(|p| p.is_file())
  {
    config_files.push(found);
  }
  // `deno run .` / `deno run <dir>`: the main module is the directory's
  // package.json "main" (else index.js), which the confined image resolves
  // again from that file
  if let DenoSubcommand::Run(run_flags) = &flags.subcommand
    && !run_flags.is_stdin()
    && let Ok(url) = cli_options.resolve_entrypoint_path(&run_flags.script)
    && let Ok(dir) = url.to_file_path()
    && dir.is_dir()
    && dir.join("package.json").is_file()
  {
    config_files.push(dir.join("package.json"));
  }
  // the lockfile this run uses, whether or not it exists yet: the first
  // image writes a new one before the re-exec (item 451), and the next
  // image must read it (a rule on a path that doesn't exist is skipped)
  if !flags.no_lock {
    let lock = match &flags.lock {
      Some(lock) => Some(cli_options.initial_cwd().join(lock)),
      None => workspace.resolve_lockfile_path().ok().flatten(),
    };
    config_files.extend(lock);
  }
  ctx.config_files = config_files;
  ctx.config_files.sort();
  ctx.config_files.dedup();
  ctx.inspector_port = flags
    .inspect
    .or(flags.inspect_brk)
    .or(flags.inspect_wait)
    .map(|addr| addr.port());
  ctx.runtime_unix_sockets = runtime_unix_sockets(cli_options.initial_cwd());
  ctx.tty_parent = std::io::stdin().is_terminal();
  ctx.ttys = std_stream_ttys();
  ctx.exec_strict = flags.permissions.kernel_sandbox_exec_strict;
  ctx.coverage_dir = cli_options.coverage_dir();
  ctx.profile_dirs = cli_options
    .cpu_prof_dir()
    .into_iter()
    .chain(cli_options.heap_prof_config().map(|c| c.dir))
    .collect();
  ctx.profile_dirs.sort();
  ctx.profile_dirs.dedup();
  ctx.report_files = report_files(cli_options);
  // the permission audit log the runtime creates at startup, in both images
  #[allow(clippy::disallowed_methods, reason = "the runtime reads it so")]
  {
    ctx.audit_file = std::env::var("DENO_AUDIT_PERMISSIONS")
      .ok()
      .filter(|target| !target.is_empty() && target != "otel")
      .map(|target| cli_options.initial_cwd().join(target));
  }
  ctx.tls_files = tls_files();
  // `--cert <file>` (DENO_CERT's flag form), relative to the working folder
  if let Some(deno_lib::args::CaData::File(cert)) = cli_options.ca_data() {
    ctx.tls_files.push(cli_options.initial_cwd().join(cert));
    ctx.tls_files.sort();
    ctx.tls_files.dedup();
  }
  ctx.origin_data = origin_data_dirs(cli_options, ctx.deno_dir.as_deref());
  ctx
}

/// The report files the run writes for the user: `deno test`'s JUnit file
/// (`--junit-path`, or deno.json's `test.reporter`), never stdout (`-`).
/// (The permission audit log is `Context::audit_file`.)
fn report_files(cli_options: &CliOptions) -> Vec<PathBuf> {
  let mut files = Vec::new();
  if let DenoSubcommand::Test(test_flags) = &cli_options.flags().subcommand {
    files.extend(
      cli_options
        .resolve_workspace_test_options(test_flags)
        .junit_path
        .filter(|path| path != "-")
        .map(|path| cli_options.initial_cwd().join(path)),
    );
  }
  files
}

/// A relative path written without `./` (`deno run src/main.ts`) that no
/// import map, deno.json `imports`, package.json dependency or workspace
/// package name can map: the run then resolves it as the path it spells.
fn bare_path_unmapped(cli_options: &CliOptions, script: &str) -> bool {
  // a scheme (`npm:`, `jsr:`, `node:`, …) or a scoped package name
  if script.contains(':') || script.starts_with('@') || script.is_empty() {
    return false;
  }
  if cli_options.flags().import_map_path.is_some() {
    return false;
  }
  let workspace = &cli_options.start_dir.workspace;
  if workspace.to_import_map_path().ok().flatten().is_some() {
    return false;
  }
  let maps = |key: &str| {
    key == script || (key.ends_with('/') && script.starts_with(key))
  };
  for deno_json in workspace.deno_jsons() {
    let json = &deno_json.json;
    if json.import_map.is_some() || json.scopes.is_some() {
      return false;
    }
    if let Some(imports) = json.imports.as_ref().and_then(|i| i.as_object())
      && imports.keys().any(|k| maps(k))
    {
      return false;
    }
  }
  // the package a bare specifier names: its first path segment
  let first = script.split('/').next().unwrap_or(script);
  for pkg_json in workspace.package_jsons() {
    let deps = [
      &pkg_json.dependencies,
      &pkg_json.dev_dependencies,
      &pkg_json.peer_dependencies,
      &pkg_json.optional_dependencies,
    ];
    if pkg_json.name.as_deref() == Some(first)
      || deps
        .iter()
        .any(|d| d.as_ref().is_some_and(|d| d.contains_key(first)))
    {
      return false;
    }
  }
  true
}

/// This program's own origin data folders (`localStorage`, the Cache API):
/// `location_data/<key>` and `location_data/web_cache/<key>`, keyed as the
/// worker keys them (`--location`, deno.json, else the main module), never
/// another program's. The main module is resolved here without
/// `CliOptions::resolve_main_module`, which keeps its first answer (a bare or
/// `npm:` entrypoint resolves through the workspace resolver later); one this
/// can't resolve the same way gets no folder (its `localStorage` fails
/// closed under enforce).
fn origin_data_dirs(
  cli_options: &CliOptions,
  deno_dir: Option<&std::path::Path>,
) -> Vec<PathBuf> {
  let Some(deno_dir) = deno_dir else {
    return Vec::new();
  };
  let resolver = cli_options.resolve_storage_key_resolver();
  // a key that doesn't depend on the main module (--location, deno.json)
  let placeholder =
    |name: &str| deno_core::url::Url::parse(&format!("file:///{name}")).ok();
  let fixed = match (placeholder("a.ts"), placeholder("b.ts")) {
    (Some(a), Some(b)) => {
      let key = resolver.resolve_storage_key(&a);
      (key == resolver.resolve_storage_key(&b)).then_some(key)
    }
    _ => None,
  };
  let key = match fixed {
    Some(key) => key,
    None => {
      let script = match &cli_options.flags().subcommand {
        DenoSubcommand::Run(flags) if !flags.is_stdin() => &flags.script,
        DenoSubcommand::Serve(flags) => &flags.script,
        _ => return Vec::new(),
      };
      // a local file or a URL, resolved as the run resolves it; a bare or
      // `npm:` / `jsr:` entrypoint, a folder or a Node binary main: none. A
      // relative path written without `./` (`deno run src/main.ts`) is
      // bare: the run tries the workspace resolver first and falls back to
      // the path, so it counts only when nothing in the workspace can map
      // it (else the folder could be another program's)
      let plain = script.starts_with('.')
        || std::path::Path::new(script).is_absolute()
        || ["file:", "http:", "https:"]
          .iter()
          .any(|scheme| script.starts_with(scheme))
        || bare_path_unmapped(cli_options, script);
      if !plain || cli_options.is_node_main() {
        return Vec::new();
      }
      let Ok(main_module) = cli_options.resolve_entrypoint_path(script) else {
        return Vec::new();
      };
      #[allow(clippy::disallowed_methods, reason = "the real file system")]
      if main_module.to_file_path().is_ok_and(|path| path.is_dir()) {
        return Vec::new();
      }
      resolver.resolve_storage_key(&main_module)
    }
  };
  let Some(key) = key else {
    return Vec::new();
  };
  let hash = deno_lib::util::checksum::r#gen(&[key.as_bytes()]);
  let origin = deno_dir.join("location_data");
  vec![origin.join(&hash), origin.join("web_cache").join(&hash)]
}

/// The policy-keyed cache overlay for a policy compiled without one:
/// `DENO_DIR/sandboxed/<hash>`. Confined runs of that policy keep their
/// SQLite caches and new emits there; every other run's cache paths are
/// outside `sandboxed/`, so none reads what a confined run wrote (item 451).
pub fn cache_overlay_for(deno_dir: &std::path::Path, hash: &str) -> PathBuf {
  deno_dir
    .join(deno_resolver::cache::SANDBOX_OVERLAY_DIR)
    .join(hash)
}

/// The mode the policy is compiled for: the applying mode, or what `auto`
/// would choose (so the report shows it), else `auto`.
fn policy_mode(resolution: &ModeResolution) -> KernelSandboxMode {
  match resolution.mode {
    KernelSandboxMode::Off => match resolution.would_apply {
      AutoPlan::Enforce | AutoPlan::Broker => KernelSandboxMode::Enforce,
      _ => KernelSandboxMode::Auto,
    },
    // the broker `auto` chose is what `enforce` takes by itself for this
    // grant: the policy is compiled as `enforce` compiles it (an explicit
    // `broker` holds every scoped rule through the broker, which nobody
    // asked for here), in both images of the run
    KernelSandboxMode::Broker if resolution.auto_chosen => {
      KernelSandboxMode::Enforce
    }
    mode => mode,
  }
}

/// The startup check of a `deno run`-family command, once its permissions
/// are resolved: decides the mode, prints `--kernel-sandbox-report`, prints
/// the `warn` notice, refuses to start under `enforce` / `broker` when a
/// rule can't be held, and otherwise prepares the enforcement plan
/// ([`enforce::prepare`]; applied before user code runs).
///
/// The mode is decided twice. Once from the flags alone, to know how the
/// policy is to be compiled; then, when the policy `enforce` compiles has
/// the broker's shape and nothing is left that can't be held, again with
/// that said (`ModeInputs::policy_needs_supervisor`): an `enforce` that
/// `auto` chose becomes `broker`, and the run starts under the supervisor
/// instead of falling back to `warn`. A policy the broker can't hold either
/// (no Landlock, a seccomp filter that can't be installed, a rule no mode
/// holds) still falls back: a mode `auto` chose never refuses to start.
pub fn check_run(
  cli_options: &CliOptions,
  options: &PermissionsOptions,
  deno_dir: Option<PathBuf>,
  node_modules: Option<PathBuf>,
) -> Result<ModeResolution, AnyError> {
  let inputs = mode_inputs(cli_options, options)?;
  let mut resolution = kernel_sandbox_mode(&inputs)?;
  let report = cli_options.flags().permissions.kernel_sandbox_report
    && inputs.kind != Some(RunKind::NotARuntime);
  // off, and no report asked for: nothing below applies. Decided before
  // the coverage of the command is worked out (for `deno test` / `deno
  // bench` that resolves the workspace's test options and each member's
  // permissions, which a run with no sandbox has no use for)
  if resolution.mode == KernelSandboxMode::Off && !report {
    return Ok(resolution);
  }
  // a mode `auto` chose never refuses to start (item 472): where enforce
  // can't hold the grant, the run is a warn one, and says why
  let (covered, uncovered) = coverage_with(
    enforce_covers(&cli_options.flags().subcommand),
    uncovered_by(cli_options),
  );
  let mut degraded: Option<String> = None;
  if resolution.auto_chosen && resolution.mode.fails_closed() {
    if enforce::confined_by_a_parent() || !detect_capabilities().enforcer_built
    {
      // the parent's sandbox already holds this process and its children;
      // or this runtime has no enforcer for the OS: the run stays as it was
      resolution.mode = KernelSandboxMode::Off;
      resolution.auto_chosen = false;
    } else if covered == EnforceCoverage::None && resolution.deny_ceiling() {
      // a deny ceiling in a command enforce doesn't cover: its denies stay
      // Deno's checks, as before (warn would report the whole grant of a
      // run a prompt can widen)
      resolution.mode = KernelSandboxMode::Off;
      resolution.reason = ModeReason::DenyCeilingDenoOnly;
      resolution.auto_chosen = false;
    } else if covered == EnforceCoverage::None {
      degraded = Some(
        uncovered
          .map(Uncovered::describe)
          .unwrap_or("enforce doesn't cover this command yet")
          .to_string(),
      );
      resolution.mode = KernelSandboxMode::Warn;
    }
  }
  if resolution.mode == KernelSandboxMode::Off && !report {
    return Ok(resolution);
  }
  let ctx = context_for(
    cli_options,
    policy_mode(&resolution),
    deno_dir,
    node_modules,
  );
  // a deny ceiling (D1 / D2) compiles the denies alone: every allow
  // unscoped, nothing else changed
  let ceiling = resolution.deny_ceiling();
  let options = &if ceiling {
    policy::deny_ceiling_options(&policy_options(cli_options, options))
  } else {
    policy_options(cli_options, options)
  };
  let mut ctx = ctx;
  ctx.deny_ceiling = ceiling;
  // the node_modules folders the project's linked packages resolve to, as
  // both images find them (never one inside a read deny)
  if let Some(node_modules) = &ctx.node_modules {
    let denies: Vec<PathBuf> = [&options.deny_read, &options.ignore_read]
      .into_iter()
      .flatten()
      .flatten()
      .map(|p| ctx.cwd.join(p))
      .collect();
    ctx.linked_node_modules =
      deno_lib::kernel_apply::linked_node_modules(node_modules, &denies);
  }
  // an `enforce` `auto` chose doesn't scrub the environment (D5): only an
  // explicit (or the harness's forced) enforce / broker does
  ctx.requested = resolution.requested;
  // a ceiling the OS holds none of here (Linux without an overmount, or
  // denies the kernel never holds) applies nothing: no re-exec, no folder
  // made; its denies are reported as Deno's checks only
  if ceiling
    && !KernelPolicy::compile(options, &ImplicitSet::build(options, &ctx), &ctx)
      .policy
      .deny_ceiling_held()
  {
    resolution.mode = KernelSandboxMode::Off;
    resolution.reason = ModeReason::DenyCeilingDenoOnly;
    resolution.auto_chosen = false;
    if !report {
      return Ok(resolution);
    }
  }
  // this program's origin data folders, as the overlay: created by the image
  // that isn't confined yet under enforce, used only when they exist
  #[allow(clippy::disallowed_methods, reason = "the real file system")]
  if resolution.mode.fails_closed() && !enforce::confined_at_startup() {
    for dir in &ctx.origin_data {
      let _ = std::fs::create_dir_all(dir);
    }
  }
  #[allow(clippy::disallowed_methods, reason = "the real file system")]
  ctx.origin_data.retain(|dir| dir.is_dir());
  let mut implicit = ImplicitSet::build(options, &ctx);
  let mut compiled = KernelPolicy::compile(options, &implicit, &ctx);
  // the cache overlay is keyed by the policy without it; it is used only
  // when its folder exists (the image that isn't confined yet creates it
  // under enforce), so both images compile the same policy, and a DENO_DIR
  // that can't be written (a child of a confined process, a read-only
  // container) leaves the caches in memory instead of refusing to start
  if !ctx.compiled
    && let Some(deno_dir) = &ctx.deno_dir
  {
    let overlay = cache_overlay_for(deno_dir, &compiled.hash());
    if resolution.mode.fails_closed() && !enforce::confined_at_startup() {
      enforce::create_cache_overlay(&overlay);
    }
    #[allow(clippy::disallowed_methods, reason = "the real file system")]
    if overlay.is_dir() {
      ctx.cache_overlay = Some(overlay);
      implicit = ImplicitSet::build(options, &ctx);
      compiled = KernelPolicy::compile(options, &implicit, &ctx);
    }
  }
  if resolution.auto_chosen
    && resolution.mode.fails_closed()
    && !enforce::confined_at_startup()
  {
    let fatal = warn::fatal_gaps(&compiled, &ctx);
    if !fatal.is_empty() {
      degraded =
        Some(format!("{} rule(s) the OS can't hold here", fatal.len()));
      resolution.mode = KernelSandboxMode::Warn;
    } else if skipped_npmrcs(cli_options, &ctx, &compiled)
      .iter()
      .any(|path| npmrc_sets_registries(path))
    {
      // the confined image treats the `.npmrc` it can't read as absent,
      // so it would look for this run's packages under another registry
      // than the one this image resolved them from (item 425)
      degraded = Some(
        "an .npmrc sets a package registry, which the confined run can't read"
          .to_string(),
      );
      resolution.mode = KernelSandboxMode::Warn;
    }
  }
  // the policy as `enforce` compiles it needs the supervising broker, and
  // with it every rule is held: decided again with that said
  if compiled.policy.shape == policy::Shape::ModeB
    && ctx.mode == KernelSandboxMode::Enforce
    && degraded.is_none()
    && compiled.fatal_gaps().next().is_none()
  {
    let again = kernel_sandbox_mode(&ModeInputs {
      policy_needs_supervisor: true,
      ..inputs.clone()
    })?;
    resolution.needs_supervisor = again.needs_supervisor;
    if resolution.would_apply != AutoPlan::Off {
      resolution.would_apply = again.would_apply;
    }
    // only a mode nothing above lowered (a parent's sandbox already holds
    // this process, a command enforce doesn't cover)
    if resolution.auto_chosen && resolution.mode == KernelSandboxMode::Enforce {
      resolution.mode = again.mode;
    }
  }
  static PRINTED: Once = Once::new();
  PRINTED.call_once(|| {
    // the image before a re-exec (Linux) already printed it
    if enforce::confined_at_startup() {
      return;
    }
    #[allow(
      clippy::print_stderr,
      reason = "the report is the output asked for"
    )]
    // (beneath a tracing supervisor the image before this one printed it)
    if report && !warn::printed_by_the_image_before() {
      // the OS translation's own gaps too (macOS)
      let mut shown = compiled.clone();
      shown.gaps.extend(enforce::report_gaps(&compiled, &ctx));
      let skipped = skipped_npmrcs(cli_options, &ctx, &compiled);
      eprint!(
        "{}",
        render_report(&resolution, &ctx.caps, &shown, &skipped)
      );
    }
    if resolution.mode == KernelSandboxMode::Warn {
      // warn reports what enforce would do with the grant: the policy as
      // enforce compiles it
      let enforced;
      let (compiled, ctx) = if ctx.mode == KernelSandboxMode::Enforce {
        (&compiled, &ctx)
      } else {
        let mut ctx = ctx.clone();
        ctx.mode = KernelSandboxMode::Enforce;
        // an explicit warn reports what an explicit enforce would do
        if ctx.requested == KernelSandboxMode::Warn {
          ctx.requested = KernelSandboxMode::Enforce;
        }
        let implicit = ImplicitSet::build(options, &ctx);
        enforced = (KernelPolicy::compile(options, &implicit, &ctx), ctx);
        (&enforced.0, &enforced.1)
      };
      warn::start(warn::WarnRun {
        resolution: &resolution,
        compiled,
        ctx,
        options,
        observable: covered != EnforceCoverage::None,
        degraded: degraded.clone(),
        header: !report,
        skipped_npmrcs: skipped_npmrcs(cli_options, ctx, compiled),
      });
    }
  });
  if resolution.mode.fails_closed() {
    let fatal: Vec<String> = compiled
      .fatal_gaps()
      .map(|g| format!("  - {}: {}", g.rule, g.detail))
      .collect();
    if !fatal.is_empty() {
      bail!(
        "--kernel-sandbox={} refuses to start: the OS can't hold these rules here:\n{}\nUse --kernel-sandbox=warn to run with Deno's checks only, or --kernel-sandbox-report for the whole policy.",
        resolution.mode,
        fatal.join("\n")
      );
    }
    enforce::set_auto_chosen(resolution.auto_chosen);
    enforce::prepare(&compiled, &ctx, options)?;
  }
  Ok(resolution)
}

/// Warns once that deno.json's kernel sandbox mode doesn't cover lint
/// plugins: `deno lint` (and the language server) run their code with no
/// kernel sandbox, whatever `permissions.kernelSandbox` says.
pub fn warn_lint_plugins_unconfined(cli_options: &CliOptions) {
  let Ok(Some(mode)) = config_mode(cli_options) else {
    return;
  };
  if !mode.fails_closed() {
    return;
  }
  static WARN: Once = Once::new();
  WARN.call_once(|| {
    log::warn!(
      "{} deno.json permissions.kernelSandbox={mode} doesn't cover lint plugins: their code runs with no kernel sandbox",
      crate::colors::yellow("Warning"),
    );
  });
}

/// Refuses to start `what`, a program this process would run directly
/// rather than through `deno run` (`deno x` of a package's native
/// executable), when the kernel sandbox would enforce for a run with these
/// flags and deno.json: nothing would confine it, and a mode that fails
/// closed never runs code unconfined.
pub fn refuse_unconfined_program(
  cli_options: &CliOptions,
  what: &str,
) -> Result<(), AnyError> {
  let options = cli_options.permissions_options()?;
  let inputs = ModeInputs {
    kind: Some(RunKind::Run),
    ..mode_inputs(cli_options, &options)?
  };
  let resolution = kernel_sandbox_mode(&inputs)?;
  if resolution.mode.fails_closed() {
    bail!(
      "--kernel-sandbox={} refuses to start {what}: it would run directly, outside any kernel sandbox",
      resolution.mode
    );
  }
  Ok(())
}

fn source_name(source: ModeSource) -> &'static str {
  match source {
    ModeSource::Default => "default",
    ModeSource::Flag => "--kernel-sandbox",
    ModeSource::Env => KERNEL_SANDBOX_ENV_VAR,
    ModeSource::Config => "deno.json permissions.kernelSandbox",
    ModeSource::Compiled => "compiled",
    ModeSource::Forced => KERNEL_SANDBOX_FORCE_ENV_VAR,
  }
}

fn plan_name(plan: AutoPlan) -> &'static str {
  match plan {
    AutoPlan::Enforce => "enforce",
    AutoPlan::Warn => "warn",
    AutoPlan::Off => "off",
    AutoPlan::Broker => "broker",
  }
}

fn os_sandbox(caps: &Capabilities) -> &'static str {
  match caps.os {
    TargetOs::Linux if caps.landlock_abi.is_some() => "landlock",
    TargetOs::Linux if caps.seccomp => "seccomp",
    TargetOs::Macos if caps.seatbelt => "seatbelt",
    TargetOs::Windows if caps.job_object => "job-object",
    _ => "none",
  }
}

fn machine_line(caps: &Capabilities) -> String {
  let mut parts = vec![caps.os.as_str().to_string()];
  if let Some(abi) = caps.landlock_abi {
    parts.push(format!("Landlock ABI {abi}"));
  } else if caps.os == TargetOs::Linux {
    parts.push("no Landlock".to_string());
  }
  if caps.seccomp {
    parts.push("seccomp".to_string());
  }
  if caps.user_namespaces {
    parts.push("user namespaces".to_string());
  }
  if caps.seatbelt {
    parts.push("Seatbelt".to_string());
  }
  if caps.job_object {
    parts.push("job objects".to_string());
  }
  if caps.low_integrity {
    parts.push("low integrity".to_string());
  }
  parts.push(format!(
    "enforcer built: {}",
    if caps.enforcer_built { "yes" } else { "no" }
  ));
  parts.join(", ")
}

fn scope_text(scope: &PathScope) -> String {
  match scope {
    PathScope::All => "all".to_string(),
    PathScope::Paths { paths } => {
      let grants: Vec<String> = paths
        .iter()
        .filter(|r| r.implicit.is_none())
        .map(|r| r.path.display().to_string())
        .collect();
      let implicit = paths.iter().filter(|r| r.implicit.is_some()).count();
      match (grants.is_empty(), implicit) {
        (true, 0) => "none".to_string(),
        (true, n) => format!("only the implicit set ({n})"),
        (false, 0) => grants.join(", "),
        (false, n) => format!("{} + implicit set ({n})", grants.join(", ")),
      }
    }
  }
}

fn denies_text(rules: &[DenyRule]) -> String {
  if rules.is_empty() {
    return "none".to_string();
  }
  rules
    .iter()
    .map(|r| {
      let how = serde_json::to_value(r.enforcement)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default();
      format!("{} ({how})", r.path.display())
    })
    .collect::<Vec<_>>()
    .join(", ")
}

fn ports_text(scope: &PortScope) -> String {
  match scope {
    PortScope::None => "none".to_string(),
    PortScope::Any => "any".to_string(),
    PortScope::Ports { ports } => ports
      .iter()
      .map(|p| p.to_string())
      .collect::<Vec<_>>()
      .join(", "),
  }
}

fn hosts_text(scope: &HostScope) -> String {
  match scope {
    HostScope::None => "none".to_string(),
    HostScope::Any => "any".to_string(),
    HostScope::Hosts { hosts } => hosts.join(", "),
  }
}

fn net_text(net: &NetPolicy) -> String {
  match net {
    NetPolicy::Denied => "denied".to_string(),
    NetPolicy::Open => "open".to_string(),
    NetPolicy::Ports { connect, bind } => format!(
      "TCP ports only: connect {}; bind {}",
      ports_text(connect),
      ports_text(bind)
    ),
    NetPolicy::Broker {
      connect,
      bind,
      deny,
    } => {
      let mut text = format!(
        "through the broker: connect {}; bind {}",
        hosts_text(connect),
        hosts_text(bind)
      );
      if !deny.is_empty() {
        write!(text, "; deny {}", deny.join(", ")).unwrap();
      }
      text
    }
  }
}

fn unix_text(unix: &UnixPolicy) -> String {
  match unix {
    UnixPolicy::Denied => "denied".to_string(),
    UnixPolicy::Open => "every socket the user can reach".to_string(),
    UnixPolicy::Paths { paths } => paths
      .iter()
      .map(|p| p.display().to_string())
      .collect::<Vec<_>>()
      .join(", "),
  }
}

/// `--kernel-sandbox-report`: the mode, the machine, the policy and every
/// gap, one line each (`!` marks a gap `enforce` refuses to start on).
/// The `.npmrc` files discovery would read (beside the workspace's
/// deno.json / package.json, and in the home folder) that exist and that the
/// read grant doesn't cover: never readable under enforce (they may hold
/// registry tokens), so a confined process treats them as absent.
fn skipped_npmrcs(
  cli_options: &CliOptions,
  ctx: &Context,
  compiled: &CompiledPolicy,
) -> Vec<PathBuf> {
  let mut dirs: Vec<PathBuf> = vec![
    cli_options.start_dir.workspace.root_dir_path(),
    cli_options.initial_cwd().to_path_buf(),
  ];
  dirs.extend(ctx.config_files.iter().filter_map(|f| {
    let name = f.file_name()?.to_str()?;
    matches!(name, "deno.json" | "deno.jsonc" | "package.json")
      .then(|| f.parent().map(|p| p.to_path_buf()))
      .flatten()
  }));
  dirs.extend(ctx.home.clone());
  #[allow(
    clippy::disallowed_methods,
    reason = "whether the real file exists, as discovery would find it"
  )]
  let mut out: Vec<PathBuf> = dirs
    .into_iter()
    .map(|d| d.join(".npmrc"))
    .filter(|p| p.is_file())
    .filter(|p| !compiled.policy.fs.read.grant_covers(p))
    .collect();
  out.sort();
  out.dedup();
  out
}

/// Whether the `.npmrc` at `path` changes where packages come from (a
/// registry, a scope's registry or a registry host replacement other than
/// what the environment alone gives): a process that treats it as absent
/// resolves packages, the lockfile's tarball URLs and the cache folders
/// differently. A file this image can't read counts as absent here too.
fn npmrc_sets_registries(path: &std::path::Path) -> bool {
  #[allow(
    clippy::disallowed_methods,
    reason = "the real .npmrc, as discovery reads it"
  )]
  let Ok(source) = std::fs::read_to_string(path) else {
    return false;
  };
  npmrc_source_sets_registries(&crate::sys::CliSys::default(), &source)
}

fn npmrc_source_sets_registries(
  sys: &impl sys_traits::EnvVar,
  source: &str,
) -> bool {
  use deno_npmrc::NpmRegistryUrl;
  // one that doesn't parse: discovery fails on it here, and wouldn't there
  let Ok(npmrc) = deno_npmrc::NpmRc::parse(sys, source) else {
    return true;
  };
  if npmrc.replace_registry_host.is_some() {
    return true;
  }
  let env_url = NpmRegistryUrl::for_npm(sys);
  let Ok(resolved) = npmrc.as_resolved(&env_url) else {
    return true;
  };
  if resolved.default_config.registry_url != env_url.url {
    return true;
  }
  let jsr_url = NpmRegistryUrl::for_jsr(sys).url;
  resolved.scopes.iter().any(|(scope, config)| {
    config.registry_url != env_url.url
      && !(scope.trim_start_matches('@') == "jsr"
        && config.registry_url == jsr_url)
  })
}

pub fn render_report(
  resolution: &ModeResolution,
  caps: &Capabilities,
  compiled: &CompiledPolicy,
  skipped_npmrcs: &[PathBuf],
) -> String {
  let p = &compiled.policy;
  let mut out = String::new();
  let w = &mut out;
  writeln!(
    w,
    "kernel sandbox (preview): {} ({}); applies: {}; would apply: {}",
    resolution.requested,
    source_name(resolution.source),
    resolution.mode,
    plan_name(resolution.would_apply),
  )
  .unwrap();
  if resolution.reason != ModeReason::Requested {
    writeln!(w, "  why: {}", resolution.reason.describe()).unwrap();
  }
  writeln!(
    w,
    "  permission set: {}{}",
    if resolution.fixed {
      "fixed (no prompt can widen it)"
    } else {
      "not fixed"
    },
    if !resolution.deny_only {
      ""
    } else if matches!(
      resolution.reason,
      ModeReason::AutoDenyCeiling | ModeReason::DenyCeilingDenoOnly
    ) {
      "; only the deny rules apply, as ceilings no prompt can lift"
    } else {
      "; --allow-all with denies: only the denies apply"
    }
  )
  .unwrap();
  let inherited = inherit::inherited_layers().len();
  if inherit::grants_unknown() {
    writeln!(
      w,
      "  inherited: confined by an ancestor's kernel sandbox whose grants weren't handed down (its descriptor was closed on the way here); every permission is refused"
    )
    .unwrap();
  } else if inherited > 0 {
    writeln!(
      w,
      "  inherited: {inherited} kernel sandbox layer(s) from the starting process (measured); this run's grant is intersected with them"
    )
    .unwrap();
  }
  writeln!(w, "  machine: {}", machine_line(caps)).unwrap();
  let shape = match p.shape {
    policy::Shape::ModeA => "mode A, self-confined",
    policy::Shape::ModeB => "mode B, with a broker",
  };
  writeln!(w, "  policy {} ({shape})", compiled.hash()).unwrap();
  writeln!(w, "    read:       {}", scope_text(&p.fs.read)).unwrap();
  writeln!(w, "    deny read:  {}", denies_text(&p.fs.deny_read)).unwrap();
  writeln!(w, "    write:      {}", scope_text(&p.fs.write)).unwrap();
  writeln!(w, "    deny write: {}", denies_text(&p.fs.deny_write)).unwrap();
  writeln!(
    w,
    "    exec:       {}",
    match p.fs.exec {
      ExecScope::None => "no programs".to_string(),
      ExecScope::Unrestricted =>
        "any readable program (children inherit these rules)".to_string(),
      ExecScope::Listed => format!(
        "only {} (strict, for the whole process tree)",
        p.fs
          .exec_programs
          .iter()
          .map(|p| p.display().to_string())
          .collect::<Vec<_>>()
          .join(", ")
      ),
    }
  )
  .unwrap();
  writeln!(w, "    load code:  {}", scope_text(&p.fs.map_exec)).unwrap();
  writeln!(w, "    net:        {}", net_text(&p.net)).unwrap();
  if !p.prebound.is_empty() {
    let listeners: Vec<String> =
      p.prebound.iter().map(|addr| addr.to_string()).collect();
    writeln!(
      w,
      "    listeners:  bound before the sandbox applies: {}",
      listeners.join(", ")
    )
    .unwrap();
  }
  writeln!(w, "    unix:       {}", unix_text(&p.unix)).unwrap();
  let mut sys: Vec<String> = p
    .sys
    .hidden_files
    .iter()
    .map(|f| f.display().to_string())
    .chain(p.sys.denied_sysctls.iter().cloned())
    .collect();
  if sys.is_empty() {
    sys.push("nothing".to_string());
  }
  writeln!(w, "    sys:        narrows {}", sys.join(", ")).unwrap();
  writeln!(
    w,
    "    env:        {}",
    match &p.env_scrub {
      Some(keep) => format!("scrubbed to {}", keep.join(", ")),
      None => "not scrubbed".to_string(),
    }
  )
  .unwrap();
  let mut implicit: Vec<&str> = compiled
    .implicit
    .entries
    .iter()
    .map(|e| e.need.as_str())
    .collect();
  implicit.sort_unstable();
  implicit.dedup();
  writeln!(w, "    implicit:   {}", implicit.join(", ")).unwrap();
  // folders only listed (macOS: the working directory, and `/` for
  // children), never read beneath
  let listed: Vec<String> = compiled
    .implicit
    .entries
    .iter()
    .filter(|e| e.access == policy::ImplicitAccess::ListDir)
    .map(|e| e.path.display().to_string())
    .collect();
  for npmrc in skipped_npmrcs {
    writeln!(
      w,
      "    skipped:    {} (may hold registry tokens; never readable under enforce; grant it with --allow-read={} if you accept the exposure)",
      npmrc.display(),
      npmrc.display()
    )
    .unwrap();
  }
  if !listed.is_empty() {
    writeln!(
      w,
      "    list only:  {} (the folders' names, never what is beneath them)",
      listed.join(", ")
    )
    .unwrap();
  }
  let fatal = compiled.fatal_gaps().count();
  writeln!(
    w,
    "  gaps: {} ({} refused by enforce, marked !)",
    compiled.gaps.len(),
    fatal
  )
  .unwrap();
  for gap in &compiled.gaps {
    let mark = if gap.is_fatal_under_enforce() {
      "!"
    } else {
      " "
    };
    let resolution = match gap.resolution {
      GapResolution::FailOpen => "fail-open",
      GapResolution::FailClosed => "fail-closed",
      GapResolution::Documented => "documented",
    };
    writeln!(
      w,
      "  {mark} {} [{resolution}] {}: {}",
      gap.class.as_str(),
      gap.rule,
      gap.detail
    )
    .unwrap();
  }
  out
}

/// `deno info --json` → `kernelSandbox`: what this machine offers and the
/// mode a non-interactive `deno run` here would get (shaped like
/// `lifecycleJail`; denext's doctor reads it).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KernelSandboxInfo {
  /// Always `true` in this release line: opt-in only.
  pub preview: bool,
  /// The requested mode (deno.json, `DENO_KERNEL_SANDBOX`, else `auto`).
  pub mode: KernelSandboxMode,
  pub source: ModeSource,
  /// What applies to such a run.
  pub effective: KernelSandboxMode,
  /// What `auto` would choose for such a run.
  pub would_apply: AutoPlan,
  /// How far the rollout of `auto` has come in this runtime: `preview`
  /// (`auto` applies nothing), `r3`, `r4`.
  pub stage: AutoStage,
  /// Why `effective` is what it is.
  pub reason: String,
  /// Whether the kernel enforces anything for such a run (`auto` applies
  /// nothing in this release line: only an explicit `enforce` does).
  pub enforced: bool,
  /// Whether this runtime has an enforcer for this OS.
  pub enforcer_built: bool,
  /// Whether this runtime has the supervising broker (Mode B) for file
  /// rules (an exact write deny inside a write allow on Linux).
  pub broker_built: bool,
  /// Whether its broker holds the network too (a host-scoped
  /// `--allow-net`, Unix sockets by path).
  pub broker_net_built: bool,
  /// `"landlock"`, `"seccomp"`, `"seatbelt"`, `"job-object"` or `"none"`.
  pub os_sandbox: &'static str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub landlock_abi: Option<u32>,
  pub capabilities: Capabilities,
  /// The features the machine's mechanisms provide (the guarantee matrix's
  /// row for this machine).
  pub features: Vec<Feature>,
  pub warnings: Vec<String>,
}

/// `kernelSandbox` for `deno info --json`.
pub fn info(cli_options: &CliOptions) -> KernelSandboxInfo {
  let caps = detect_capabilities();
  let mut warnings = Vec::new();
  let config = config_mode(cli_options).unwrap_or_else(|err| {
    warnings.push(format!("deno.json permissions.kernelSandbox: {err}"));
    None
  });
  let inputs = ModeInputs {
    kind: Some(RunKind::Run),
    env: env_override_warned(),
    config,
    prompt: true,
    terminals: false,
    stage: stage_override_warned(),
    ..Default::default()
  };
  let resolution =
    kernel_sandbox_mode(&inputs).expect("no flag or watch: never an error");
  let mut features: Vec<Feature> = policy::CAPABILITY_TABLE
    .iter()
    .map(|row| row.feature)
    .filter(|f| caps.supports(*f))
    .collect();
  features.sort();
  features.dedup();
  if !caps.enforcer_built {
    warnings.push(
      "kernel sandbox: no enforcer for this OS is built into this runtime; --kernel-sandbox=enforce and broker refuse to start".to_string(),
    );
  } else if !policy::BROKER_NET_BUILT {
    warnings.push(
      "kernel sandbox: the supervising broker (Mode B) holds file rules only (a write deny inside a write allow on Linux); its network side is not built in yet, so --kernel-sandbox=broker and enforce refuse rules only that side can hold (hosts, Unix sockets by path on Linux, module fetches after start)".to_string(),
    );
  }
  if caps.os == TargetOs::Linux && caps.landlock_abi.is_none() {
    warnings.push(
      "kernel sandbox: Landlock is not available here (kernel older than 5.13, not in the LSM list, or refused by a container's seccomp profile)".to_string(),
    );
  }
  KernelSandboxInfo {
    preview: true,
    mode: resolution.requested,
    source: resolution.source,
    effective: resolution.mode,
    would_apply: resolution.would_apply,
    stage: resolution.stage,
    reason: resolution.reason.describe().to_string(),
    enforced: resolution.enforced,
    enforcer_built: caps.enforcer_built,
    broker_built: caps.enforcer_built && policy::BROKER_FS_BUILT,
    broker_net_built: caps.enforcer_built && policy::BROKER_NET_BUILT,
    os_sandbox: os_sandbox(&caps),
    landlock_abi: caps.landlock_abi,
    capabilities: caps,
    features,
    warnings,
  }
}

#[cfg(test)]
mod coverage_tests {
  use super::*;
  use crate::args::BenchFlags;
  use crate::args::TestFlags;

  /// `auto_stage_matrix`'s denext.30 row for `test` / `bench`: `enforce`
  /// covers them after type check and coverage setup.
  #[test]
  fn test_and_bench_are_covered_after_setup() {
    assert_eq!(
      enforce_covers(&DenoSubcommand::Test(TestFlags::default())),
      EnforceCoverage::AfterSetup
    );
    assert_eq!(
      enforce_covers(&DenoSubcommand::Bench(BenchFlags::default())),
      EnforceCoverage::AfterSetup
    );
  }

  /// What runs user code before the sandbox could apply, or writes what no
  /// grant names, makes a covered command uncovered (`auto` then runs it as
  /// `warn`, never refusing); a command already uncovered stays so, with
  /// the generic reason.
  #[test]
  fn uncovered_parts_lower_the_coverage() {
    for why in [
      Uncovered::Eszip,
      Uncovered::TestConfig,
      Uncovered::UpdateSnapshots,
      Uncovered::MemberPermissions,
    ] {
      for covered in [EnforceCoverage::Full, EnforceCoverage::AfterSetup] {
        assert_eq!(
          coverage_with(covered, Some(why)),
          (EnforceCoverage::None, Some(why))
        );
      }
      assert_eq!(
        coverage_with(EnforceCoverage::None, Some(why)),
        (EnforceCoverage::None, None)
      );
      assert!(why.describe().starts_with("enforce doesn't cover"));
    }
    assert_eq!(
      coverage_with(EnforceCoverage::AfterSetup, None),
      (EnforceCoverage::AfterSetup, None)
    );
  }
}

#[cfg(test)]
mod npmrc_tests {
  use sys_traits::impls::InMemorySys;

  use super::npmrc_source_sets_registries;

  /// An `.npmrc` the confined image can't read moves `auto` to warn only
  /// when it changes where packages come from (item 425): a token alone,
  /// or the default registry spelled out, doesn't.
  #[test]
  fn npmrc_registries_move_auto_to_warn() {
    let sys = InMemorySys::default();
    let sets = |source: &str| npmrc_source_sets_registries(&sys, source);
    assert!(!sets(""));
    assert!(!sets("//registry.npmjs.org/:_authToken=secret\n"));
    assert!(!sets(
      "registry=https://registry.npmjs.org/\n//registry.npmjs.org/:_authToken=${NODE_AUTH_TOKEN}\n"
    ));
    assert!(!sets("@jsr:registry=https://npm.jsr.io/\n"));
    assert!(sets("registry=http://localhost:4261/\n"));
    assert!(sets("@denotest:registry=http://localhost:4261/\n"));
  }
}
