// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: which kernel-sandbox mode a process runs in (DENEXT.md item 420).
//!
//! The kernel sandbox holds native code (FFI, Node-API addons) and
//! subprocesses to the same grants Deno's own checks enforce, using the OS
//! (Landlock + seccomp on Linux, Seatbelt on macOS). It is a backstop under
//! Deno's checks, never a replacement: those checks and their `NotCapable`
//! errors stay as they are.
//!
//! This module is the one place that decides the mode, shared by the CLI
//! (`deno run` and friends) and `denort` (compiled programs), so the two can't
//! disagree. It is pure: every input is passed in [`ModeInputs`].
//!
//! `auto` (the default) applies nothing in this release line: the kernel
//! sandbox is an opt-in preview (`--kernel-sandbox=warn|enforce|broker`).
//! [`ModeResolution::would_apply`] records what `auto` would choose, so
//! turning it on is the one-line [`AUTO_APPLIES`] change: enforce where the
//! author fixed the permissions (a compiled program, an explicit `auto`), warn
//! for other runs whose permissions are fixed only because no prompt can
//! happen. Until an enforcer is built in, `enforce` / `broker` refuse to start
//! (the CLI reports which rules it would have needed).

use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;

use deno_runtime::deno_permissions::PermissionsOptions;
use serde::Deserialize;
use serde::Serialize;

/// Whether `auto` applies what [`ModeResolution::would_apply`] names. `false`
/// for the whole preview line: `auto` equals `off` (no default change).
pub const AUTO_APPLIES: bool = false;

/// The environment variable that can lower the mode of a `deno run`-family
/// command without editing it (break-glass). It accepts only `off` and
/// `warn`, is read before any `--env-file` is loaded (so a project's env file
/// can't set it), and is ignored by compiled programs, whose mode is their
/// author's.
pub const KERNEL_SANDBOX_ENV_VAR: &str = "DENO_KERNEL_SANDBOX";

/// The values `--kernel-sandbox`, deno.json `permissions.kernelSandbox` and
/// `deno compile --kernel-sandbox` accept.
pub const KERNEL_SANDBOX_VALUES: &[&str] =
  &["off", "warn", "auto", "enforce", "broker"];

/// A kernel-sandbox mode, as written in a flag or config.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "lowercase")]
pub enum KernelSandboxMode {
  /// No kernel layer.
  Off,
  /// Compute the policy and report what it would refuse; refuse nothing.
  Warn,
  /// Apply what the OS can express; report the rest (the default).
  #[default]
  Auto,
  /// Apply the policy; any rule the OS can't hold exactly is a startup error.
  Enforce,
  /// `enforce`, with a supervising broker process for the rules only it can
  /// hold exactly (host-scoped network, Unix sockets by path, write denies
  /// inside an allowed directory on Linux).
  Broker,
}

impl KernelSandboxMode {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Off => "off",
      Self::Warn => "warn",
      Self::Auto => "auto",
      Self::Enforce => "enforce",
      Self::Broker => "broker",
    }
  }

  /// `enforce` and `broker`: modes that fail closed, refusing to start when
  /// a rule can't be held.
  pub fn fails_closed(self) -> bool {
    matches!(self, Self::Enforce | Self::Broker)
  }
}

impl fmt::Display for KernelSandboxMode {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
  "invalid kernel sandbox mode '{0}': possible values: off, warn, auto, enforce, broker"
)]
pub struct ParseKernelSandboxModeError(pub String);

impl FromStr for KernelSandboxMode {
  type Err = ParseKernelSandboxModeError;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "off" => Ok(Self::Off),
      "warn" => Ok(Self::Warn),
      "auto" => Ok(Self::Auto),
      "enforce" => Ok(Self::Enforce),
      "broker" => Ok(Self::Broker),
      other => Err(ParseKernelSandboxModeError(other.to_string())),
    }
  }
}

/// What kind of process asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RunKind {
  /// `run`, `eval`, `serve`, `test`, `bench`, `x`: user code with a
  /// permission set.
  Run,
  /// `repl`: interactive, so prompting decides it as for `run`.
  Repl,
  /// The Jupyter kernel: runs with its permission set; never has a terminal.
  Jupyter,
  /// A compiled program (`denort`): always fixed, mode baked at compile time.
  Compiled,
  /// Anything that isn't a user-code runtime (`install`, `cache`, `compile`,
  /// `fmt`, `lint`, `check`, `task`, `lsp`, …). npm lifecycle scripts keep the
  /// lifecycle-script jail.
  NotARuntime,
}

/// Where the requested mode came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ModeSource {
  /// Nothing set it: `auto`.
  Default,
  /// `--kernel-sandbox`.
  Flag,
  /// `DENO_KERNEL_SANDBOX` (`off` / `warn` only).
  Env,
  /// deno.json `permissions.kernelSandbox`.
  Config,
  /// `deno compile --kernel-sandbox`, read from the binary.
  Compiled,
}

/// Why the effective mode is what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ModeReason {
  /// The requested mode applies as asked.
  Requested,
  /// The mode was set to `off`.
  TurnedOff,
  /// Not a user-code runtime.
  Subcommand,
  /// `-A` with no deny rule: nothing to narrow, and plain `-A` is promised
  /// no change at all.
  AllowAll,
  /// A prompt can widen the grant (stdin and stderr are terminals and
  /// prompting is on), so the set isn't fixed.
  Prompt,
  /// `--watch` re-resolves the module graph after start.
  Watch,
  /// `--watch-hmr` / hot module replacement.
  Hmr,
  /// Under `denext dev` (it re-fetches and re-resolves after start).
  DenextDev,
  /// A development build of a desktop app (`deno desktop --hmr` /
  /// `--inspect*`).
  DevBuild,
  /// An external permission broker (`DENO_PERMISSION_BROKER_PATH`) can grant
  /// anything at any time, so the set isn't fixed.
  PermissionBroker,
  /// `auto` on a fixed permission set: `auto` applies nothing in this
  /// release line (the kernel sandbox is opt-in); `would_apply` says what it
  /// would choose.
  NotYetEnforced,
}

impl ModeReason {
  pub fn describe(self) -> &'static str {
    match self {
      Self::Requested => "as requested",
      Self::TurnedOff => "turned off",
      Self::Subcommand => "not a user-code runtime",
      Self::AllowAll => {
        "--allow-all with no deny rule leaves nothing to narrow"
      }
      Self::Prompt => "a permission prompt can widen the grant",
      Self::Watch => "--watch re-resolves modules after start",
      Self::Hmr => "hot module replacement re-resolves modules after start",
      Self::DenextDev => "denext dev re-resolves modules after start",
      Self::DevBuild => "a desktop development build",
      Self::PermissionBroker => {
        "a permission broker (DENO_PERMISSION_BROKER_PATH) can grant anything"
      }
      Self::NotYetEnforced => {
        "auto applies nothing yet (the kernel sandbox is an opt-in preview)"
      }
    }
  }
}

/// What `auto` would choose on this run (applied only with [`AUTO_APPLIES`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AutoPlan {
  /// The author fixed the permissions (a compiled program, or `auto` written
  /// explicitly in a flag or deno.json): enforce.
  Enforce,
  /// The set is fixed only because no prompt can happen (no terminal, or
  /// `--no-prompt`): warn first, enforce in a later release.
  Warn,
  /// Not fixed, or turned off: nothing.
  Off,
}

/// Everything [`kernel_sandbox_mode`] decides from.
#[derive(Debug, Clone, Default)]
pub struct ModeInputs {
  pub kind: Option<RunKind>,
  /// `--kernel-sandbox` (CLI only; a compiled program ignores it).
  pub flag: Option<KernelSandboxMode>,
  /// `DENO_KERNEL_SANDBOX` as read before `--env-file` (CLI only).
  pub env: Option<KernelSandboxMode>,
  /// deno.json `permissions.kernelSandbox` (CLI only).
  pub config: Option<KernelSandboxMode>,
  /// The mode baked into a compiled program (`None`: `auto`).
  pub baked: Option<KernelSandboxMode>,
  /// Every allow is unscoped (`-A`, or `permissions.all`).
  pub allow_all: bool,
  /// Any `--deny-*` / `--ignore-*` rule.
  pub has_deny: bool,
  /// Prompting is enabled (not `--no-prompt` / `DENO_NO_PROMPT`).
  pub prompt: bool,
  /// stdin and stderr are terminals (`prompt_terminals_available`).
  pub terminals: bool,
  pub watch: bool,
  pub hmr: bool,
  pub denext_dev: bool,
  /// A compiled desktop development build.
  pub dev_build: bool,
  /// An upstream permission broker is connected.
  pub permission_broker: bool,
}

/// The mode a process runs in, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeResolution {
  /// The mode asked for (after precedence: compiled > flag > env > config >
  /// default).
  pub requested: KernelSandboxMode,
  pub source: ModeSource,
  /// The mode that applies to this process.
  pub mode: KernelSandboxMode,
  pub reason: ModeReason,
  /// No prompt can widen the grant during this process's life.
  pub fixed: bool,
  /// `-A` with deny rules: only the denies would be enforced.
  pub deny_only: bool,
  /// What `auto` would choose here (`AUTO_APPLIES` turns it on).
  pub would_apply: AutoPlan,
  /// Whether the kernel enforces anything for this process. Always `false`
  /// in this runtime.
  pub enforced: bool,
}

/// An explicit `enforce` / `broker` that can't hold for this run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
  "--kernel-sandbox={mode} can't be used here: {}. Use --kernel-sandbox=warn or auto, or leave it out.",
  reason.describe()
)]
pub struct ModeError {
  pub mode: KernelSandboxMode,
  pub reason: ModeReason,
}

/// Decides the kernel-sandbox mode (plan §2.1 as amended by the review):
///
/// 1. Not a user-code runtime → `off`.
/// 2. The requested mode: a compiled program's baked mode (flag, env and
///    config ignored), else `--kernel-sandbox`, else `DENO_KERNEL_SANDBOX`
///    (`off` / `warn` only), else deno.json, else `auto`.
/// 3. `off` → `off`. A desktop development build → `off`.
/// 4. `-A` with no deny → `off` (plain `-A` changes nothing).
/// 5. `--watch`, HMR, `denext dev` or a permission broker: the set isn't
///    fixed. `auto` → `off`, `warn` stays, `enforce` / `broker` → error.
/// 6. A prompt can happen (CLI, prompting on, stdin and stderr terminals):
///    `auto` → `off`. An explicit `warn` / `enforce` / `broker` applies, and
///    then turns prompting into denials.
/// 7. Otherwise the set is fixed: the requested mode applies; `auto` records
///    what it would choose and applies nothing while `AUTO_APPLIES` is off.
pub fn kernel_sandbox_mode(
  inputs: &ModeInputs,
) -> Result<ModeResolution, ModeError> {
  let kind = inputs.kind.unwrap_or(RunKind::NotARuntime);
  let compiled = kind == RunKind::Compiled;
  let (requested, source) = if compiled {
    match inputs.baked {
      Some(mode) => (mode, ModeSource::Compiled),
      None => (KernelSandboxMode::Auto, ModeSource::Default),
    }
  } else if let Some(mode) = inputs.flag {
    (mode, ModeSource::Flag)
  } else if let Some(mode) = inputs
    .env
    .filter(|m| matches!(m, KernelSandboxMode::Off | KernelSandboxMode::Warn))
  {
    (mode, ModeSource::Env)
  } else if let Some(mode) = inputs.config {
    (mode, ModeSource::Config)
  } else {
    (KernelSandboxMode::Auto, ModeSource::Default)
  };
  let terminal_prompt =
    !compiled && inputs.prompt && inputs.terminals && kind != RunKind::Jupyter;
  let fixed = !terminal_prompt
    && !inputs.permission_broker
    && !inputs.watch
    && !inputs.hmr
    && !inputs.denext_dev;
  let mut resolution = ModeResolution {
    requested,
    source,
    mode: KernelSandboxMode::Off,
    reason: ModeReason::Requested,
    fixed,
    deny_only: inputs.allow_all && inputs.has_deny,
    would_apply: AutoPlan::Off,
    enforced: false,
  };
  let off = |mut r: ModeResolution, reason: ModeReason| {
    r.mode = KernelSandboxMode::Off;
    r.reason = reason;
    r.would_apply = AutoPlan::Off;
    Ok(r)
  };
  if kind == RunKind::NotARuntime {
    return off(resolution, ModeReason::Subcommand);
  }
  if requested == KernelSandboxMode::Off {
    return off(resolution, ModeReason::TurnedOff);
  }
  if compiled && inputs.dev_build {
    return off(resolution, ModeReason::DevBuild);
  }
  if inputs.allow_all && !inputs.has_deny {
    return off(resolution, ModeReason::AllowAll);
  }
  let unfixed_reason = if inputs.permission_broker {
    Some(ModeReason::PermissionBroker)
  } else if inputs.hmr {
    Some(ModeReason::Hmr)
  } else if inputs.watch {
    Some(ModeReason::Watch)
  } else if inputs.denext_dev {
    Some(ModeReason::DenextDev)
  } else {
    None
  };
  if let Some(reason) = unfixed_reason {
    return match requested {
      KernelSandboxMode::Warn => {
        resolution.mode = KernelSandboxMode::Warn;
        Ok(resolution)
      }
      mode if mode.fails_closed() => Err(ModeError { mode, reason }),
      _ => off(resolution, reason),
    };
  }
  if terminal_prompt && requested == KernelSandboxMode::Auto {
    return off(resolution, ModeReason::Prompt);
  }
  if requested == KernelSandboxMode::Auto {
    resolution.would_apply = if compiled || source != ModeSource::Default {
      AutoPlan::Enforce
    } else {
      AutoPlan::Warn
    };
    if AUTO_APPLIES {
      resolution.mode = match resolution.would_apply {
        AutoPlan::Enforce => KernelSandboxMode::Enforce,
        AutoPlan::Warn => KernelSandboxMode::Warn,
        AutoPlan::Off => KernelSandboxMode::Off,
      };
      return Ok(resolution);
    }
    resolution.mode = KernelSandboxMode::Off;
    resolution.reason = ModeReason::NotYetEnforced;
    return Ok(resolution);
  }
  // an explicit warn / enforce / broker: no prompt may widen it
  resolution.fixed = true;
  resolution.mode = requested;
  Ok(resolution)
}

/// Every allow unscoped (`-A`, or `permissions.all`).
pub fn options_allow_all(options: &PermissionsOptions) -> bool {
  let all = |o: &Option<Vec<String>>| o.as_ref().is_some_and(|v| v.is_empty());
  all(&options.allow_read)
    && all(&options.allow_write)
    && all(&options.allow_net)
    && all(&options.allow_env)
    && all(&options.allow_run)
    && all(&options.allow_ffi)
    && all(&options.allow_sys)
}

/// Any deny or ignore rule.
pub fn options_have_deny(options: &PermissionsOptions) -> bool {
  [
    &options.deny_read,
    &options.ignore_read,
    &options.deny_write,
    &options.deny_net,
    &options.deny_net_connect,
    &options.deny_net_listen,
    &options.deny_env,
    &options.ignore_env,
    &options.deny_run,
    &options.deny_ffi,
    &options.deny_sys,
  ]
  .iter()
  .any(|o| o.is_some())
}

/// A compiled program's mode needs an enforcer this runtime doesn't have.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
  "this program was compiled with --kernel-sandbox={0}, but no kernel enforcer is built into this runtime, so it refuses to start"
)]
pub struct NoEnforcerError(pub KernelSandboxMode);

/// `denort`'s startup check: the baked mode (flags, `DENO_KERNEL_SANDBOX`
/// and deno.json are not read; the mode is the author's). `warn` prints one
/// line; `enforce` / `broker` refuse to start while no enforcer is built in
/// (`deno compile` already refuses to bake them).
pub fn check_compiled(
  baked: Option<KernelSandboxMode>,
  dev_build: bool,
  options: &PermissionsOptions,
) -> Result<ModeResolution, NoEnforcerError> {
  let resolution = kernel_sandbox_mode(&ModeInputs {
    kind: Some(RunKind::Compiled),
    baked,
    dev_build,
    allow_all: options_allow_all(options),
    has_deny: options_have_deny(options),
    prompt: options.prompt,
    ..Default::default()
  })
  // a compiled program has no watch, broker or dev-server inputs
  .expect("a compiled program's mode never conflicts");
  if resolution.mode.fails_closed() {
    return Err(NoEnforcerError(resolution.mode));
  }
  if resolution.mode == KernelSandboxMode::Warn {
    #[allow(clippy::print_stderr, reason = "the warn mode's one line")]
    {
      eprintln!(
        "Warning kernel sandbox (warn, preview): nothing is refused; no kernel enforcer is built into this runtime, so every rule is held by Deno's checks only."
      );
    }
  }
  Ok(resolution)
}

/// `DENO_KERNEL_SANDBOX`, read once. Call [`snapshot_env_override`] before
/// any `--env-file` is loaded so an env file can't set it.
static ENV_OVERRIDE: OnceLock<Option<String>> = OnceLock::new();

/// Reads `DENO_KERNEL_SANDBOX` now, before `--env-file` loading. Later calls
/// return the first reading.
pub fn snapshot_env_override() {
  ENV_OVERRIDE.get_or_init(|| std::env::var(KERNEL_SANDBOX_ENV_VAR).ok());
}

/// The break-glass override: `Ok(None)` when unset, `Ok(Some(off|warn))`, or
/// the raw value when it is anything else (the caller warns and ignores it).
pub fn env_override() -> Result<Option<KernelSandboxMode>, String> {
  snapshot_env_override();
  match ENV_OVERRIDE.get().and_then(|v| v.as_deref()) {
    None | Some("") => Ok(None),
    Some("off") => Ok(Some(KernelSandboxMode::Off)),
    Some("warn") => Ok(Some(KernelSandboxMode::Warn)),
    Some(other) => Err(other.to_string()),
  }
}

#[cfg(test)]
mod tests {
  use KernelSandboxMode::*;

  use super::*;

  fn run() -> ModeInputs {
    ModeInputs {
      kind: Some(RunKind::Run),
      prompt: true,
      ..Default::default()
    }
  }

  fn resolve(inputs: &ModeInputs) -> ModeResolution {
    kernel_sandbox_mode(inputs).unwrap()
  }

  #[test]
  fn parses_every_value_and_rejects_others() {
    for value in KERNEL_SANDBOX_VALUES {
      let mode: KernelSandboxMode = value.parse().unwrap();
      assert_eq!(mode.as_str(), *value);
      assert_eq!(
        serde_json::to_value(mode).unwrap(),
        serde_json::Value::String(value.to_string())
      );
    }
    assert!("on".parse::<KernelSandboxMode>().is_err());
    assert!("Auto".parse::<KernelSandboxMode>().is_err());
  }

  /// The mode matrix: subcommand × flags × TTY × compiled × watch. Each row
  /// is (name, inputs, expected effective mode, reason, auto plan).
  #[test]
  fn mode_matrix() {
    struct Row {
      name: &'static str,
      inputs: ModeInputs,
      mode: KernelSandboxMode,
      reason: ModeReason,
      plan: AutoPlan,
      fixed: bool,
    }
    let tty = |mut i: ModeInputs| {
      i.terminals = true;
      i
    };
    let rows = vec![
      Row {
        name: "run, no terminal: fixed, auto would warn",
        inputs: run(),
        mode: Off,
        reason: ModeReason::NotYetEnforced,
        plan: AutoPlan::Warn,
        fixed: true,
      },
      Row {
        name: "run on a terminal with prompting: not fixed",
        inputs: tty(run()),
        mode: Off,
        reason: ModeReason::Prompt,
        plan: AutoPlan::Off,
        fixed: false,
      },
      Row {
        name: "run on a terminal with --no-prompt: fixed, warn plan",
        inputs: ModeInputs {
          prompt: false,
          ..tty(run())
        },
        mode: Off,
        reason: ModeReason::NotYetEnforced,
        plan: AutoPlan::Warn,
        fixed: true,
      },
      Row {
        name: "explicit --kernel-sandbox=auto: the author's choice, enforce plan",
        inputs: ModeInputs {
          flag: Some(Auto),
          ..run()
        },
        mode: Off,
        reason: ModeReason::NotYetEnforced,
        plan: AutoPlan::Enforce,
        fixed: true,
      },
      Row {
        name: "deno.json auto: enforce plan",
        inputs: ModeInputs {
          config: Some(Auto),
          ..run()
        },
        mode: Off,
        reason: ModeReason::NotYetEnforced,
        plan: AutoPlan::Enforce,
        fixed: true,
      },
      Row {
        name: "explicit auto on a terminal still waits for the prompt",
        inputs: ModeInputs {
          flag: Some(Auto),
          ..tty(run())
        },
        mode: Off,
        reason: ModeReason::Prompt,
        plan: AutoPlan::Off,
        fixed: false,
      },
      Row {
        name: "warn on a terminal applies and fixes the set",
        inputs: ModeInputs {
          flag: Some(Warn),
          ..tty(run())
        },
        mode: Warn,
        reason: ModeReason::Requested,
        plan: AutoPlan::Off,
        fixed: true,
      },
      Row {
        name: "enforce applies as requested",
        inputs: ModeInputs {
          flag: Some(Enforce),
          ..run()
        },
        mode: Enforce,
        reason: ModeReason::Requested,
        plan: AutoPlan::Off,
        fixed: true,
      },
      Row {
        name: "broker applies as requested",
        inputs: ModeInputs {
          config: Some(Broker),
          ..run()
        },
        mode: Broker,
        reason: ModeReason::Requested,
        plan: AutoPlan::Off,
        fixed: true,
      },
      Row {
        name: "off",
        inputs: ModeInputs {
          flag: Some(Off),
          ..run()
        },
        mode: Off,
        reason: ModeReason::TurnedOff,
        plan: AutoPlan::Off,
        fixed: true,
      },
      Row {
        name: "plain -A: nothing to narrow, even with enforce",
        inputs: ModeInputs {
          allow_all: true,
          flag: Some(Enforce),
          ..run()
        },
        mode: Off,
        reason: ModeReason::AllowAll,
        plan: AutoPlan::Off,
        fixed: true,
      },
      Row {
        name: "-A with a deny: deny-only, auto plan applies",
        inputs: ModeInputs {
          allow_all: true,
          has_deny: true,
          ..run()
        },
        mode: Off,
        reason: ModeReason::NotYetEnforced,
        plan: AutoPlan::Warn,
        fixed: true,
      },
      Row {
        name: "--watch: auto off",
        inputs: ModeInputs {
          watch: true,
          ..run()
        },
        mode: Off,
        reason: ModeReason::Watch,
        plan: AutoPlan::Off,
        fixed: false,
      },
      Row {
        name: "--watch with warn keeps warn",
        inputs: ModeInputs {
          watch: true,
          flag: Some(Warn),
          ..run()
        },
        mode: Warn,
        reason: ModeReason::Requested,
        plan: AutoPlan::Off,
        fixed: false,
      },
      Row {
        name: "HMR: auto off",
        inputs: ModeInputs {
          watch: true,
          hmr: true,
          ..run()
        },
        mode: Off,
        reason: ModeReason::Hmr,
        plan: AutoPlan::Off,
        fixed: false,
      },
      Row {
        name: "denext dev: auto off",
        inputs: ModeInputs {
          denext_dev: true,
          ..run()
        },
        mode: Off,
        reason: ModeReason::DenextDev,
        plan: AutoPlan::Off,
        fixed: false,
      },
      Row {
        name: "permission broker: auto off",
        inputs: ModeInputs {
          permission_broker: true,
          ..run()
        },
        mode: Off,
        reason: ModeReason::PermissionBroker,
        plan: AutoPlan::Off,
        fixed: false,
      },
      Row {
        name: "repl on a terminal: off",
        inputs: ModeInputs {
          kind: Some(RunKind::Repl),
          ..tty(run())
        },
        mode: Off,
        reason: ModeReason::Prompt,
        plan: AutoPlan::Off,
        fixed: false,
      },
      Row {
        name: "jupyter never prompts: fixed",
        inputs: ModeInputs {
          kind: Some(RunKind::Jupyter),
          ..tty(run())
        },
        mode: Off,
        reason: ModeReason::NotYetEnforced,
        plan: AutoPlan::Warn,
        fixed: true,
      },
      Row {
        name: "not a runtime: off whatever the flag says",
        inputs: ModeInputs {
          kind: Some(RunKind::NotARuntime),
          flag: Some(Enforce),
          ..run()
        },
        mode: Off,
        reason: ModeReason::Subcommand,
        plan: AutoPlan::Off,
        fixed: true,
      },
      Row {
        name: "compiled on a terminal with prompting: always fixed, enforce plan",
        inputs: ModeInputs {
          kind: Some(RunKind::Compiled),
          ..tty(run())
        },
        mode: Off,
        reason: ModeReason::NotYetEnforced,
        plan: AutoPlan::Enforce,
        fixed: true,
      },
      Row {
        name: "compiled ignores the flag, env and config",
        inputs: ModeInputs {
          kind: Some(RunKind::Compiled),
          flag: Some(Off),
          env: Some(Off),
          config: Some(Off),
          ..run()
        },
        mode: Off,
        reason: ModeReason::NotYetEnforced,
        plan: AutoPlan::Enforce,
        fixed: true,
      },
      Row {
        name: "compiled with a baked warn",
        inputs: ModeInputs {
          kind: Some(RunKind::Compiled),
          baked: Some(Warn),
          ..run()
        },
        mode: Warn,
        reason: ModeReason::Requested,
        plan: AutoPlan::Off,
        fixed: true,
      },
      Row {
        name: "compiled with a baked off",
        inputs: ModeInputs {
          kind: Some(RunKind::Compiled),
          baked: Some(Off),
          ..run()
        },
        mode: Off,
        reason: ModeReason::TurnedOff,
        plan: AutoPlan::Off,
        fixed: true,
      },
      Row {
        name: "compiled desktop dev build: off",
        inputs: ModeInputs {
          kind: Some(RunKind::Compiled),
          baked: Some(Enforce),
          dev_build: true,
          ..run()
        },
        mode: Off,
        reason: ModeReason::DevBuild,
        plan: AutoPlan::Off,
        fixed: true,
      },
      Row {
        name: "compiled with a baked enforce",
        inputs: ModeInputs {
          kind: Some(RunKind::Compiled),
          baked: Some(Enforce),
          ..run()
        },
        mode: Enforce,
        reason: ModeReason::Requested,
        plan: AutoPlan::Off,
        fixed: true,
      },
    ];
    for row in rows {
      let r = resolve(&row.inputs);
      assert_eq!(r.mode, row.mode, "{}", row.name);
      assert_eq!(r.reason, row.reason, "{}", row.name);
      assert_eq!(r.would_apply, row.plan, "{}", row.name);
      assert_eq!(r.fixed, row.fixed, "{}", row.name);
      assert!(!r.enforced, "{}: nothing is enforced yet", row.name);
    }
  }

  #[test]
  fn precedence_flag_env_config() {
    let r = resolve(&ModeInputs {
      flag: Some(Warn),
      env: Some(Off),
      config: Some(Enforce),
      ..run()
    });
    assert_eq!((r.requested, r.source), (Warn, ModeSource::Flag));
    let r = resolve(&ModeInputs {
      env: Some(Off),
      config: Some(Enforce),
      ..run()
    });
    assert_eq!((r.requested, r.source), (Off, ModeSource::Env));
    let r = resolve(&ModeInputs {
      config: Some(Enforce),
      ..run()
    });
    assert_eq!((r.requested, r.source), (Enforce, ModeSource::Config));
    let r = resolve(&run());
    assert_eq!((r.requested, r.source), (Auto, ModeSource::Default));
  }

  #[test]
  fn env_can_only_lower() {
    // the break-glass variable never raises the mode
    for mode in [Auto, Enforce, Broker] {
      let r = resolve(&ModeInputs {
        env: Some(mode),
        ..run()
      });
      assert_eq!(r.source, ModeSource::Default, "{mode}");
    }
  }

  #[test]
  fn enforce_refuses_an_unfixed_set() {
    for (inputs, reason) in [
      (
        ModeInputs {
          watch: true,
          ..run()
        },
        ModeReason::Watch,
      ),
      (
        ModeInputs {
          watch: true,
          hmr: true,
          ..run()
        },
        ModeReason::Hmr,
      ),
      (
        ModeInputs {
          denext_dev: true,
          ..run()
        },
        ModeReason::DenextDev,
      ),
      (
        ModeInputs {
          permission_broker: true,
          ..run()
        },
        ModeReason::PermissionBroker,
      ),
    ] {
      for mode in [Enforce, Broker] {
        let err = kernel_sandbox_mode(&ModeInputs {
          flag: Some(mode),
          ..inputs.clone()
        })
        .unwrap_err();
        assert_eq!(err, ModeError { mode, reason });
        assert!(err.to_string().starts_with(&format!(
          "--kernel-sandbox={mode} can't be used here: "
        )));
      }
    }
  }

  #[test]
  fn compiled_check() {
    let options = PermissionsOptions::default();
    assert_eq!(check_compiled(None, false, &options).unwrap().mode, Off);
    assert_eq!(
      check_compiled(Some(Warn), false, &options).unwrap().mode,
      Warn
    );
    assert_eq!(
      check_compiled(Some(Enforce), false, &options).unwrap_err(),
      NoEnforcerError(Enforce)
    );
    assert_eq!(
      check_compiled(Some(Broker), false, &options).unwrap_err(),
      NoEnforcerError(Broker)
    );
    // a desktop development build never applies one
    assert_eq!(
      check_compiled(Some(Enforce), true, &options).unwrap().mode,
      Off
    );
  }

  #[test]
  fn allow_all_and_denies() {
    let all = Some(Vec::new());
    let mut options = PermissionsOptions {
      allow_read: all.clone(),
      allow_write: all.clone(),
      allow_net: all.clone(),
      allow_env: all.clone(),
      allow_run: all.clone(),
      allow_ffi: all.clone(),
      allow_sys: all,
      ..Default::default()
    };
    assert!(options_allow_all(&options));
    assert!(!options_have_deny(&options));
    options.ignore_env = Some(vec!["X".to_string()]);
    assert!(options_have_deny(&options));
    options.allow_read = Some(vec!["/a".to_string()]);
    assert!(!options_allow_all(&options));
  }

  #[test]
  fn deny_only_is_reported() {
    let r = resolve(&ModeInputs {
      allow_all: true,
      has_deny: true,
      flag: Some(Warn),
      ..run()
    });
    assert!(r.deny_only);
    assert_eq!(r.mode, Warn);
  }
}
