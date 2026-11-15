// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: the kernel sandbox (DENEXT.md item 420), an opt-in preview.
//!
//! - [`policy`]: the policy model (what each OS mechanism would apply, and
//!   the gaps), pure data.
//! - The mode decision lives in `deno_lib::kernel_sandbox`, shared with
//!   `denort`.
//! - This module wires them to a CLI run: [`check_run`] at startup
//!   (`--kernel-sandbox`, `--kernel-sandbox-report`), and [`info`] for
//!   `deno info --json` (`kernelSandbox`).
//!
//! Nothing is enforced yet: `auto` applies nothing, `warn` reports, and
//! `enforce` / `broker` refuse to start because no enforcer is built in.

pub mod policy;

use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Once;

use deno_core::anyhow::bail;
use deno_core::error::AnyError;
use deno_lib::kernel_sandbox::AutoPlan;
use deno_lib::kernel_sandbox::KERNEL_SANDBOX_ENV_VAR;
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

/// The mechanisms this machine offers. No enforcer is built in yet.
pub fn detect_capabilities() -> Capabilities {
  let os = TargetOs::current();
  #[allow(unused_mut, reason = "set per OS")]
  let mut caps = Capabilities::none(os);
  #[cfg(target_os = "linux")]
  {
    caps.landlock_abi = crate::supply_chain::sandbox_linux::landlock_abi().ok();
    // the lifecycle jail installs seccomp filters on every supported arch
    caps.seccomp = cfg!(any(target_arch = "x86_64", target_arch = "aarch64"));
    caps.user_namespaces =
      crate::supply_chain::sandbox_linux::netns_available().is_ok();
  }
  #[cfg(target_os = "macos")]
  {
    caps.seatbelt = crate::supply_chain::sandbox_macos::available().is_ok();
  }
  #[cfg(windows)]
  {
    caps.job_object = true;
    caps.low_integrity = true;
  }
  caps
}

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

/// The mode `deno compile` bakes into a binary: `--kernel-sandbox`, else
/// deno.json `permissions.kernelSandbox`, else nothing (`auto`). A mode that
/// fails closed is refused while no enforcer is built in, so no binary is
/// made that can't start.
pub fn compile_mode(
  cli_options: &CliOptions,
) -> Result<Option<KernelSandboxMode>, AnyError> {
  let mode = match flag_mode(cli_options)? {
    Some(mode) => Some(mode),
    None => config_mode(cli_options)?,
  };
  if let Some(mode) = mode
    && mode.fails_closed()
    && !detect_capabilities().enforcer_built
  {
    bail!(
      "--kernel-sandbox={mode}: no kernel enforcer is built into this runtime yet, so a program compiled with it could not start. Use --kernel-sandbox=warn, auto or off."
    );
  }
  Ok(mode)
}

/// Whether this process runs under `denext dev` (its dev server hands its
/// processes `DENEXT_DEV_TOKEN`).
fn under_denext_dev() -> bool {
  std::env::var_os("DENEXT_DEV_TOKEN").is_some()
}

fn mode_inputs(
  cli_options: &CliOptions,
  options: &PermissionsOptions,
) -> Result<ModeInputs, AnyError> {
  let flags = cli_options.flags();
  Ok(ModeInputs {
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
  ctx.exe = std::env::current_exe().ok();
  ctx.deno_dir = deno_dir;
  ctx.node_modules = node_modules;
  let workspace = &cli_options.start_dir.workspace;
  ctx.config_files = workspace
    .root_deno_json()
    .into_iter()
    .chain(cli_options.start_dir.member_deno_json())
    .filter_map(|c| c.specifier.to_file_path().ok())
    .collect();
  ctx.config_files.sort();
  ctx.config_files.dedup();
  let flags = cli_options.flags();
  ctx.inspector_port = flags
    .inspect
    .or(flags.inspect_brk)
    .or(flags.inspect_wait)
    .map(|addr| addr.port());
  ctx.tty_parent = std::io::stdin().is_terminal();
  ctx
}

/// The mode the policy is compiled for: the applying mode, or what `auto`
/// would choose (so the report shows it), else `auto`.
fn policy_mode(resolution: &ModeResolution) -> KernelSandboxMode {
  match resolution.mode {
    KernelSandboxMode::Off => match resolution.would_apply {
      AutoPlan::Enforce => KernelSandboxMode::Enforce,
      _ => KernelSandboxMode::Auto,
    },
    mode => mode,
  }
}

/// The startup check of a `deno run`-family command, once its permissions
/// are resolved: decides the mode, prints `--kernel-sandbox-report`, prints
/// the `warn` notice, and refuses to start under `enforce` / `broker` when a
/// rule can't be held (always, while no enforcer is built in).
pub fn check_run(
  cli_options: &CliOptions,
  options: &PermissionsOptions,
  deno_dir: Option<PathBuf>,
  node_modules: Option<PathBuf>,
) -> Result<ModeResolution, AnyError> {
  let inputs = mode_inputs(cli_options, options)?;
  let resolution = kernel_sandbox_mode(&inputs)?;
  let report = cli_options.flags().permissions.kernel_sandbox_report
    && inputs.kind != Some(RunKind::NotARuntime);
  if resolution.mode == KernelSandboxMode::Off && !report {
    return Ok(resolution);
  }
  let ctx = context_for(
    cli_options,
    policy_mode(&resolution),
    deno_dir,
    node_modules,
  );
  let implicit = ImplicitSet::build(options, &ctx);
  let compiled = KernelPolicy::compile(options, &implicit, &ctx);
  static PRINTED: Once = Once::new();
  PRINTED.call_once(|| {
    #[allow(clippy::print_stderr, reason = "the report is the output asked for")]
    if report {
      eprint!("{}", render_report(&resolution, &ctx.caps, &compiled));
    } else if resolution.mode == KernelSandboxMode::Warn {
      let deno_only = compiled
        .gaps
        .iter()
        .filter(|g| g.resolution == GapResolution::FailOpen)
        .count();
      eprintln!(
        "{} kernel sandbox (warn, preview): nothing is refused; {deno_only} rule(s) would be held by Deno's checks only. Run with --kernel-sandbox-report for the policy.",
        crate::colors::yellow("Warning"),
      );
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
  }
  Ok(resolution)
}

fn source_name(source: ModeSource) -> &'static str {
  match source {
    ModeSource::Default => "default",
    ModeSource::Flag => "--kernel-sandbox",
    ModeSource::Env => KERNEL_SANDBOX_ENV_VAR,
    ModeSource::Config => "deno.json permissions.kernelSandbox",
    ModeSource::Compiled => "compiled",
  }
}

fn plan_name(plan: AutoPlan) -> &'static str {
  match plan {
    AutoPlan::Enforce => "enforce",
    AutoPlan::Warn => "warn",
    AutoPlan::Off => "off",
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
pub fn render_report(
  resolution: &ModeResolution,
  caps: &Capabilities,
  compiled: &CompiledPolicy,
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
    if resolution.deny_only {
      "; --allow-all with denies: only the denies apply"
    } else {
      ""
    }
  )
  .unwrap();
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
      ExecScope::None => "no programs",
      ExecScope::Unrestricted =>
        "any readable program (children inherit these rules)",
    }
  )
  .unwrap();
  writeln!(w, "    load code:  {}", scope_text(&p.fs.map_exec)).unwrap();
  writeln!(w, "    net:        {}", net_text(&p.net)).unwrap();
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
  /// Why `effective` is what it is.
  pub reason: String,
  /// Whether the kernel enforces anything. `false` in this release line.
  pub enforced: bool,
  /// Whether this runtime has an enforcer for this OS.
  pub enforcer_built: bool,
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
      "kernel sandbox: no enforcer is built into this runtime yet; --kernel-sandbox=enforce and broker refuse to start".to_string(),
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
    reason: resolution.reason.describe().to_string(),
    enforced: resolution.enforced,
    enforcer_built: caps.enforcer_built,
    os_sandbox: os_sandbox(&caps),
    landlock_abi: caps.landlock_abi,
    capabilities: caps,
    features,
    warnings,
  }
}
