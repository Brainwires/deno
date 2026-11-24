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
//! What `auto` does is one constant, [`AUTO_STAGE`] (DENEXT.md item 472):
//!
//! | stage | author-fixed (compiled, explicit `auto`) | fixed only by no prompt |
//! | --- | --- | --- |
//! | [`AutoStage::Preview`] | nothing (recorded in `would_apply`) | nothing |
//! | [`AutoStage::R3`] | enforce | warn |
//! | [`AutoStage::R4`] | enforce | enforce |
//!
//! Where the grant has a rule only the supervising broker holds exactly (a
//! host-scoped network, a write deny inside a write allow on Linux), `auto`
//! chooses the broker wherever it would have chosen `enforce`
//! ([`AutoPlan::Broker`], [`ModeResolution::needs_supervisor`]): the caller
//! compiles the policy, and says so in [`ModeInputs::policy_needs_supervisor`].
//!
//! A mode `auto` chose never refuses to start: where neither `enforce` nor
//! the broker can hold the grant (a rule no mode holds here, a command they
//! don't cover) the caller falls back to `warn`
//! ([`ModeResolution::auto_chosen`]). `enforce` applies the policy on Linux
//! and macOS (and refuses to start when the OS can't hold a rule, or
//! elsewhere), for the CLI and for a compiled program baked with it
//! (`deno_lib::kernel_apply`); `broker` is `enforce` under the supervising
//! process, in the CLI and in a compiled program alike.

use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;

use deno_runtime::deno_permissions::PermissionsOptions;
use serde::Deserialize;
use serde::Serialize;

/// How far the rollout of `auto` has come (DENEXT.md item 472).
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Default,
)]
#[serde(rename_all = "lowercase")]
pub enum AutoStage {
  /// `auto` applies nothing (the opt-in preview): it equals `off`, and
  /// records what the next stage would apply.
  #[default]
  Preview,
  /// `auto` enforces where the author fixed the permissions (a compiled
  /// program, `auto` written in a flag or deno.json, as `denext start` and a
  /// strict desktop package do), and warns for a run whose permissions are
  /// fixed only because no prompt can happen (no terminal, `--no-prompt`).
  R3,
  /// `auto` enforces wherever the permission set is fixed.
  R4,
}

impl AutoStage {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Preview => "preview",
      Self::R3 => "r3",
      Self::R4 => "r4",
    }
  }

  /// The harness variable's value: `None` when unset or empty, `Err` for
  /// anything but `r3` / `r4`.
  pub fn parse(value: Option<&str>) -> Result<Option<Self>, String> {
    match value {
      None | Some("") => Ok(None),
      Some("r3") => Ok(Some(Self::R3)),
      Some("r4") => Ok(Some(Self::R4)),
      Some(other) => Err(other.to_string()),
    }
  }
}

/// The stage this runtime ships at. The flip to the next stage is this one
/// line, made only when that stage's preconditions hold
/// (`tests/kernel_sandbox_beds/r4_preconditions.ts`).
pub const AUTO_STAGE: AutoStage = AutoStage::Preview;

/// The test harnesses' switch (never a user setting): runs `auto` at a later
/// stage than [`AUTO_STAGE`], so the beds soak the next stage's defaults
/// before the constant moves. It can only raise the stage (an earlier one is
/// ignored), so it only ever narrows; read with `DENO_KERNEL_SANDBOX`, before
/// any `--env-file`.
pub const KERNEL_SANDBOX_STAGE_ENV_VAR: &str =
  "DENO_INTERNAL_KERNEL_SANDBOX_STAGE";

/// The environment variable that can lower the mode of a `deno run`-family
/// command without editing it (break-glass). It accepts only `off` and
/// `warn`, is read before any `--env-file` is loaded (so a project's env file
/// can't set it), and is ignored by compiled programs, whose mode is their
/// author's.
pub const KERNEL_SANDBOX_ENV_VAR: &str = "DENO_KERNEL_SANDBOX";

/// The test harnesses' switch (never a user setting): every `deno run` /
/// `deno serve` of a suite runs under `--kernel-sandbox=enforce`, `-A`
/// included (DENEXT.md item 451). `enforce` keeps each run's own grant;
/// `project` also narrows an unscoped read or write grant, for the kernel
/// only, to the project, the working directory and the temp folder, so a
/// suite of `-A` programs shows what the implicit set lacks. It can only
/// narrow: an explicit `off` (the flag, `DENO_KERNEL_SANDBOX=off` or
/// deno.json's) still wins, and an unfixed run (`--watch`, HMR, `denext dev`, a permission
/// broker) stays as it is.
pub const KERNEL_SANDBOX_FORCE_ENV_VAR: &str =
  "DENO_INTERNAL_KERNEL_SANDBOX_FORCE";

/// Names the test harness's marker: the absolute path of a file the harness
/// made for its run, named `deno-kernel-sandbox-harness-<anything>`. The
/// harness switches ([`KERNEL_SANDBOX_FORCE_ENV_VAR`],
/// [`KERNEL_SANDBOX_STAGE_ENV_VAR`]) take effect only while that file
/// exists ([`harness_marked`]): they change what a release binary does with
/// no flag in sight, so a variable that merely stayed behind in an
/// environment (a CI image, a copied shell profile) must not be enough; a
/// marker left with it names a file that isn't there. Every process of the
/// run checks the same file (a file's metadata stays readable inside the
/// kernel sandbox, and the variable is kept through its environment scrub),
/// so the images of one run decide alike.
pub const KERNEL_SANDBOX_HARNESS_ENV_VAR: &str =
  "DENO_INTERNAL_KERNEL_SANDBOX_HARNESS";

/// What the file [`KERNEL_SANDBOX_HARNESS_ENV_VAR`] names is called.
pub const KERNEL_SANDBOX_HARNESS_FILE_PREFIX: &str =
  "deno-kernel-sandbox-harness-";

/// Whether `marker` ([`KERNEL_SANDBOX_HARNESS_ENV_VAR`]'s value) names a
/// harness's marker file that exists: an absolute path to a regular file
/// (not a link) whose name starts with
/// [`KERNEL_SANDBOX_HARNESS_FILE_PREFIX`].
#[allow(
  clippy::disallowed_methods,
  reason = "the harness's marker is on the real file system"
)]
pub fn harness_marked(marker: Option<&std::ffi::OsStr>) -> bool {
  let Some(marker) = marker.filter(|m| !m.is_empty()) else {
    return false;
  };
  let path = std::path::Path::new(marker);
  path.is_absolute()
    && path
      .file_name()
      .and_then(|name| name.to_str())
      .is_some_and(|name| {
        name.len() > KERNEL_SANDBOX_HARNESS_FILE_PREFIX.len()
          && name.starts_with(KERNEL_SANDBOX_HARNESS_FILE_PREFIX)
      })
    && std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file())
}

/// The test harnesses' forced mode ([`KERNEL_SANDBOX_FORCE_ENV_VAR`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ForcedEnforce {
  /// `enforce`, each run with its own grant.
  Enforce,
  /// `enforce`, an unscoped read / write grant narrowed for the kernel to
  /// the project, the working directory and the temp folder.
  Project,
}

impl ForcedEnforce {
  /// The variable's value: `None` when unset or empty, `Err` for anything
  /// but `enforce` / `project`.
  pub fn parse(value: Option<&str>) -> Result<Option<Self>, String> {
    match value {
      None | Some("") => Ok(None),
      Some("enforce") => Ok(Some(Self::Enforce)),
      Some("project") => Ok(Some(Self::Project)),
      Some(other) => Err(other.to_string()),
    }
  }
}

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
  /// A packaged desktop app (`denort_desktop`): fixed, mode baked at compile
  /// time, flags, environment and deno.json ignored as for
  /// [`RunKind::Compiled`]. `strict` is `desktop.permissions: "strict"`
  /// (carried as a baked `enforce`), the only desktop apps the kernel
  /// sandbox is applied to: under `auto` one that isn't strict is off at
  /// every stage ([`ModeReason::DesktopNotStrict`]). `engine` is the web
  /// engine baked into the package. Whether the engine can run inside the
  /// sandbox on this machine is not this module's question:
  /// `kernel_apply::start_desktop` answers it and reports the app as
  /// kernel-held or headless-only
  /// (DENEXT.md item 489).
  DesktopPackaged { strict: bool, engine: Engine },
  /// Anything that isn't a user-code runtime (`install`, `cache`, `compile`,
  /// `fmt`, `lint`, `check`, `task`, `lsp`, …). npm lifecycle scripts keep the
  /// lifecycle-script jail.
  NotARuntime,
}

/// A packaged desktop app's web engine. What each one means depends on the
/// OS it runs on (denext.30 plan §1.1):
///
/// - [`Engine::Webview`]: the OS web view — WKWebView on macOS, WebKitGTK on
///   Linux, WebView2 on Windows.
/// - [`Engine::Cef`]: the bundled Chromium Embedded Framework. On Linux its
///   namespace sandbox (or only the setuid helper) decides how far the
///   kernel sandbox can nest; on macOS a CEF app gets headless guarantees
///   only.
/// - [`Engine::Gpui`]: no web engine. laufey's GPUI host draws the app's
///   native tree itself (Metal on macOS, Vulkan on Linux), in the app's
///   own process, with no helper process: its profile is the OS's
///   toolkit, fonts and GPU, without a web engine's folders and helpers
///   (`kernel_desktop::engine_paths`; DENEXT.md item 5NN).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Engine {
  Webview,
  Cef,
  Gpui,
}

impl Engine {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Webview => "webview",
      Self::Cef => "cef",
      Self::Gpui => "gpui",
    }
  }

  /// `deno desktop --backend` / deno.json `desktop.backend`: `cef` is
  /// Chromium, `gpui` the GPUI host, everything else the OS web view.
  pub fn from_backend(backend: Option<&str>) -> Self {
    match backend {
      Some("cef") => Self::Cef,
      Some("gpui") => Self::Gpui,
      _ => Self::Webview,
    }
  }
}

/// How much of a command `enforce` covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EnforceCoverage {
  /// The whole run, from before user code loads (`run` of a file, `serve`).
  Full,
  /// The run after the command's own setup (type check, coverage folder):
  /// `test` / `bench`. The image before the re-exec type-checks and finds
  /// the modules; the confined one runs exactly those (WP-A, denext.30).
  AfterSetup,
  /// Not covered: an `enforce` asked for by name refuses to start, one
  /// `auto` chose runs as `warn`.
  None,
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
  /// The test harnesses' `DENO_INTERNAL_KERNEL_SANDBOX_FORCE`.
  Forced,
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
  /// `auto` chose the mode: the author fixed the permissions (a compiled
  /// program, or `auto` written in a flag or deno.json).
  AutoAuthorFixed,
  /// `auto` chose the mode: no prompt can widen the grant in this run (no
  /// terminal, or `--no-prompt`).
  AutoNoPrompt,
  /// `auto` chose the mode: a prompt can widen the grant, so only its deny
  /// rules are enforced, as ceilings no prompt can lift (D1; `-A` with
  /// denies on a terminal, D2).
  AutoDenyCeiling,
  /// `auto` would enforce the deny rules alone, but the OS can't hold any
  /// of them here without changing the rest of the run (Linux without an
  /// overmount): they stay Deno's checks only, reported, never refused.
  DenyCeilingDenoOnly,
  /// A packaged desktop app whose own process isn't confined: its web
  /// engine can't run inside the kernel sandbox on this machine (or this OS
  /// has no enforcer for desktop apps), so the app has the headless
  /// guarantees only (DENEXT.md item 489).
  DesktopEngine,
  /// A packaged desktop app that doesn't bake strict permissions
  /// (`desktop.permissions: "strict"`, a baked `enforce`): `auto` applies
  /// nothing to it at any stage. Such an app starts `open` / `osascript` /
  /// `xdg-open` for `openPath`, `revealInFileManager` and the auth
  /// session's browser, and only http / https / mailto links have a way
  /// out of the sandbox (the outside helper), so confining it by default
  /// would break what it does (DENEXT.md item 489, the R4 decision).
  DesktopNotStrict,
  /// The process runs inside an npm lifecycle script's jail
  /// (`DENO_INTERNAL_LIFECYCLE_JAIL`): the jail is its confinement (the OS
  /// sandbox around the script's whole process tree, and the permission
  /// clamp), so `auto` adds nothing (denext.30 plan §1.1: the lifecycle
  /// jail is unchanged).
  LifecycleJail,
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
      Self::AutoAuthorFixed => {
        "auto: the author fixed the permissions (a compiled program, or auto set explicitly)"
      }
      Self::AutoNoPrompt => {
        "auto: no prompt can widen the grant here (no terminal, or --no-prompt)"
      }
      Self::AutoDenyCeiling => {
        "auto: only the deny rules are enforced (a prompt can't lift a deny), and nothing else changes"
      }
      Self::DenyCeilingDenoOnly => {
        "auto: the deny rules are held by Deno's checks only here (the OS can't hold them without changing the rest of the run)"
      }
      Self::DesktopEngine => {
        "the desktop app's web engine can't run inside the kernel sandbox here"
      }
      Self::DesktopNotStrict => {
        "auto applies nothing to a desktop app that isn't packaged strict (desktop.permissions: \"strict\")"
      }
      Self::LifecycleJail => {
        "auto: the npm lifecycle-script jail already confines this process"
      }
    }
  }
}

/// What `auto` chooses on this run from [`AutoStage::R3`] on (in the preview
/// it is only recorded).
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
  /// The policy needs the supervising broker (Mode B): host-scoped network,
  /// a write deny inside an allow, Unix sockets by path on Linux. What
  /// `auto` applies wherever it would otherwise enforce; a run it only
  /// warns about at [`AutoStage::R3`] records it all the same (the broker is
  /// what holds this grant once the run is enforced).
  Broker,
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
  /// The test harnesses' forced `enforce` (CLI `run` / `serve` only).
  pub forced: Option<ForcedEnforce>,
  /// The stage `auto` runs at; `None`: [`AUTO_STAGE`]. Set from the
  /// harnesses' [`KERNEL_SANDBOX_STAGE_ENV_VAR`], and by tests.
  pub stage: Option<AutoStage>,
  /// The compiled policy needs the supervisor (Mode B) and every rule
  /// enforce can't hold in place is one the broker closes: the policy
  /// compiled as `enforce` compiles it has the broker's shape and no rule
  /// left that the OS can't hold. The mode is decided before a policy
  /// exists, so the caller decides once without it, compiles the policy,
  /// and decides again with it (`cli/sandbox::check_run`,
  /// `kernel_apply::start_compiled`).
  pub policy_needs_supervisor: bool,
  /// The process runs inside an npm lifecycle script's jail
  /// (`DENO_INTERNAL_LIFECYCLE_JAIL` is set): `auto` is then `off`
  /// ([`ModeReason::LifecycleJail`]); an explicit mode applies as asked.
  pub lifecycle_jail: bool,
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
  /// Only the deny rules would be enforced, as ceilings: `-A` with deny
  /// rules (D2), or, under `auto`, a run where a prompt can happen and that
  /// has deny rules (D1).
  pub deny_only: bool,
  /// The run's policy needs the supervising broker (Mode B), whichever mode
  /// applies: `auto` then chooses `broker` where it would enforce, and an
  /// explicit `enforce` takes the broker by itself.
  pub needs_supervisor: bool,
  /// What `auto` chooses here (applied from [`AutoStage::R3`] on).
  pub would_apply: AutoPlan,
  /// The stage `auto` ran at.
  pub stage: AutoStage,
  /// `auto` chose [`ModeResolution::mode`] (nobody asked for it by name):
  /// such a mode never refuses to start. Where `enforce` can't hold the
  /// grant, the caller runs it as `warn`.
  pub auto_chosen: bool,
  /// Whether the kernel enforces anything for this process. Always `false`
  /// in this runtime.
  pub enforced: bool,
}

impl ModeResolution {
  /// `auto` enforces the deny rules alone (D1 / D2): the policy is compiled
  /// as allow-everything-except-the-denies, with nothing else changed (no
  /// seccomp list, no no-new-privileges, no environment scrub, no read
  /// expansion), and a deny the OS can't hold that way is a reported
  /// Deno-only rule, never a refusal (decision O4). An explicit `enforce`
  /// with `-A` and denies is the full policy, as asked.
  pub fn deny_ceiling(&self) -> bool {
    self.deny_only && self.auto_chosen && self.mode.fails_closed()
  }
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

/// Decides the kernel-sandbox mode (plan §2.1 as amended by the review, and
/// the denext.30 plan §1.1). The order is the contract; the first rule that
/// matches decides:
///
/// 1. Not a user-code runtime → `off` ([`ModeReason::Subcommand`]).
/// 2. The requested mode: a compiled program's or packaged desktop app's
///    baked mode (flag, env, config and the harness's switch ignored), else
///    `--kernel-sandbox`, else `DENO_KERNEL_SANDBOX` (`off` / `warn` only),
///    else the harness's forced `enforce`, else deno.json, else `auto`.
/// 3. `off` → `off`. A development build of a compiled desktop app → `off`,
///    whatever is baked.
///    Inside an npm lifecycle script's jail, `auto` (by default or written
///    out) → `off` ([`ModeReason::LifecycleJail`]): the jail is the
///    process's confinement.
/// 4. `-A` with no deny → `off` (plain `-A` changes nothing), unless the
///    harness forced the mode.
/// 5. `--watch`, HMR, `denext dev` or a permission broker: the set isn't
///    fixed. `auto` → `off`, `warn` stays, `enforce` / `broker` → error (a
///    forced run stays as it was).
/// 6. The D1 ceiling (WP-A). A prompt can happen (CLI, prompting on, stdin
///    and stderr terminals; never a compiled program or a desktop app):
///    `auto` → `off`, unless the run has deny rules: from
///    [`AutoStage::R4`] on, `auto` then enforces the denies alone (D1, D2:
///    a prompt can't lift a deny; [`ModeResolution::deny_ceiling`]), and the
///    prompt stays. A ceiling is Mode A only and never takes the broker.
///    An explicit `warn` / `enforce` / `broker` applies, and then turns
///    prompting into denials.
/// 7. The desktop resolution (WP-C, the R4 decision). A packaged desktop
///    app that isn't strict ([`RunKind::DesktopPackaged`] with
///    `strict: false`) under `auto` → `off` at every stage
///    ([`ModeReason::DesktopNotStrict`]). A strict one is a baked
///    `enforce` (rule 9); whether its engine can run inside the sandbox is
///    `kernel_apply::start_desktop_with`'s question, after this function.
/// 8. Otherwise the set is fixed and `auto` decides by stage: it records
///    what it would choose, applies nothing in [`AutoStage::Preview`], and
///    from [`AutoStage::R3`] on applies it (`auto_chosen`): `enforce` where
///    the author fixed the permissions or the stage is R4, else `warn`.
///    `-A` with denies is then a deny ceiling too (D2,
///    [`ModeResolution::deny_only`]).
/// 9. An explicit `warn` / `enforce` / `broker` applies as asked.
///
/// After the policy is compiled the caller decides once more (WP-B,
/// `cli/sandbox::check_run`, `kernel_apply::start_compiled`): when the
/// policy `enforce` compiles has the broker's shape, the same inputs with
/// [`ModeInputs::policy_needs_supervisor`] turn rule 8's `enforce` into
/// `broker` (`auto` takes the supervisor instead of falling back to
/// `warn`). That second decision never changes rules 1 to 7: a deny
/// ceiling's policy is never Mode B, and a desktop app runs no supervisor
/// (`Context::compiled_supervisor` stays false in `start_desktop_with`, so
/// what only the broker holds is a rule the OS can't hold there).
pub fn kernel_sandbox_mode(
  inputs: &ModeInputs,
) -> Result<ModeResolution, ModeError> {
  let kind = inputs.kind.unwrap_or(RunKind::NotARuntime);
  // a packaged desktop app is a compiled program: its mode is baked, its
  // set is its author's, and no prompt counts
  let compiled =
    matches!(kind, RunKind::Compiled | RunKind::DesktopPackaged { .. });
  let (requested, source) = if compiled {
    match inputs.baked {
      Some(mode) => (mode, ModeSource::Compiled),
      None => (KernelSandboxMode::Auto, ModeSource::Default),
    }
  } else if let Some(mode) = inputs.flag
    && (inputs.forced.is_none() || mode == KernelSandboxMode::Off)
  {
    (mode, ModeSource::Flag)
  } else if let Some(mode) = inputs
    .env
    .filter(|m| matches!(m, KernelSandboxMode::Off | KernelSandboxMode::Warn))
    .filter(|m| inputs.forced.is_none() || *m == KernelSandboxMode::Off)
  {
    (mode, ModeSource::Env)
  } else if inputs.forced.is_some()
    && inputs.config != Some(KernelSandboxMode::Off)
  {
    // the harness's switch replaces every mode but an explicit off
    (KernelSandboxMode::Enforce, ModeSource::Forced)
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
    needs_supervisor: inputs.policy_needs_supervisor,
    would_apply: AutoPlan::Off,
    stage: inputs.stage.unwrap_or(AUTO_STAGE).max(AUTO_STAGE),
    auto_chosen: false,
    enforced: false,
  };
  let off = |mut r: ModeResolution, reason: ModeReason| {
    r.mode = KernelSandboxMode::Off;
    r.reason = reason;
    r.would_apply = AutoPlan::Off;
    r.needs_supervisor = false;
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
  // the lifecycle jail is unchanged at every stage: its OS sandbox and the
  // permission clamp confine the script's process tree already, and a
  // second policy compiled inside it can't see what the jail hides
  if inputs.lifecycle_jail && requested == KernelSandboxMode::Auto {
    return off(resolution, ModeReason::LifecycleJail);
  }
  // the harness forces `-A` runs too: the policy is then the whole grant
  // plus the seccomp rules (or, for `project`, the narrowed one)
  if inputs.allow_all && !inputs.has_deny && source != ModeSource::Forced {
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
      // a forced run that isn't fixed stays as it was
      _ if source == ModeSource::Forced => off(resolution, reason),
      mode if mode.fails_closed() => Err(ModeError { mode, reason }),
      _ => off(resolution, reason),
    };
  }
  if terminal_prompt && requested == KernelSandboxMode::Auto {
    // D1 / D2: the deny rules are ceilings no prompt can lift, so they are
    // fixed even while a prompt can widen the rest (from R4); the prompt
    // stays, and nothing but the denies is enforced
    if !inputs.has_deny || resolution.stage < AutoStage::R4 {
      return off(resolution, ModeReason::Prompt);
    }
    resolution.deny_only = true;
    resolution.would_apply = AutoPlan::Enforce;
    resolution.mode = KernelSandboxMode::Enforce;
    resolution.reason = ModeReason::AutoDenyCeiling;
    resolution.auto_chosen = true;
    return Ok(resolution);
  }
  if requested == KernelSandboxMode::Auto
    && matches!(kind, RunKind::DesktopPackaged { strict: false, .. })
  {
    // the R4 decision: a desktop app is kernel-enforced only when it is
    // packaged strict (a baked enforce). One that isn't shells out to the
    // system's openers, which only http / https / mailto links have a way
    // around (the outside helper), so `auto` leaves it alone at every stage
    return off(resolution, ModeReason::DesktopNotStrict);
  }
  if requested == KernelSandboxMode::Auto {
    let author_fixed = compiled || source != ModeSource::Default;
    // whether this stage enforces the run (else it warns about it)
    let enforces = author_fixed || resolution.stage == AutoStage::R4;
    resolution.would_apply = if inputs.policy_needs_supervisor {
      AutoPlan::Broker
    } else if enforces {
      AutoPlan::Enforce
    } else {
      AutoPlan::Warn
    };
    if resolution.stage == AutoStage::Preview {
      resolution.mode = KernelSandboxMode::Off;
      resolution.reason = ModeReason::NotYetEnforced;
      return Ok(resolution);
    }
    resolution.mode = match resolution.would_apply {
      _ if !enforces => KernelSandboxMode::Warn,
      AutoPlan::Enforce => KernelSandboxMode::Enforce,
      AutoPlan::Warn => KernelSandboxMode::Warn,
      AutoPlan::Off => KernelSandboxMode::Off,
      AutoPlan::Broker => KernelSandboxMode::Broker,
    };
    resolution.reason = if author_fixed {
      ModeReason::AutoAuthorFixed
    } else {
      ModeReason::AutoNoPrompt
    };
    resolution.auto_chosen = true;
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

/// A compiled program's mode asks for a sandbox that wasn't applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
  "this program was compiled with --kernel-sandbox={0}, but the kernel sandbox wasn't applied to this process, so it refuses to start"
)]
pub struct NoEnforcerError(pub KernelSandboxMode);

/// A compiled program's mode: the baked one (flags, `DENO_KERNEL_SANDBOX`
/// and deno.json are not read; the mode is the author's). Pure.
pub fn compiled_resolution(
  baked: Option<KernelSandboxMode>,
  dev_build: bool,
  options: &PermissionsOptions,
) -> ModeResolution {
  compiled_resolution_with(baked, dev_build, options, false)
}

/// [`compiled_resolution`], decided again once the program's policy is
/// compiled: `policy_needs_supervisor` says it has the broker's shape
/// ([`ModeInputs::policy_needs_supervisor`]).
pub fn compiled_resolution_with(
  baked: Option<KernelSandboxMode>,
  dev_build: bool,
  options: &PermissionsOptions,
  policy_needs_supervisor: bool,
) -> ModeResolution {
  kernel_sandbox_mode(&ModeInputs {
    kind: Some(RunKind::Compiled),
    baked,
    dev_build,
    allow_all: options_allow_all(options),
    has_deny: options_have_deny(options),
    prompt: options.prompt,
    policy_needs_supervisor,
    ..Default::default()
  })
  // a compiled program has no watch, broker or dev-server inputs
  .expect("a compiled program's mode never conflicts")
}

/// A packaged desktop app's mode: the baked one, as
/// [`compiled_resolution`] (`strict`: a baked `enforce`). An app that isn't
/// strict (nothing baked, or a baked `auto`) is off at every stage
/// ([`ModeReason::DesktopNotStrict`]). `stage`: the harnesses' later stage
/// for `auto` ([`KERNEL_SANDBOX_STAGE_ENV_VAR`]; it can only raise the
/// stage, so a desktop app reads it like every other runtime), which
/// therefore changes nothing for a desktop app today. Pure.
pub fn desktop_resolution(
  baked: Option<KernelSandboxMode>,
  dev_build: bool,
  options: &PermissionsOptions,
  engine: Engine,
  stage: Option<AutoStage>,
) -> ModeResolution {
  kernel_sandbox_mode(&ModeInputs {
    kind: Some(RunKind::DesktopPackaged {
      strict: baked == Some(KernelSandboxMode::Enforce),
      engine,
    }),
    baked,
    dev_build,
    allow_all: options_allow_all(options),
    has_deny: options_have_deny(options),
    prompt: options.prompt,
    stage,
    ..Default::default()
  })
  // as a compiled program: no watch, broker or dev-server inputs
  .expect("a desktop app's mode never conflicts")
}

/// What a packaged desktop app's startup decided
/// (`deno_lib::kernel_apply::start_desktop`), for [`check_compiled`].
static DESKTOP: OnceLock<ModeResolution> = OnceLock::new();

/// Records the resolution a packaged desktop app's kernel-sandbox startup
/// reached. From then on [`check_compiled`] answers with it: a desktop
/// app's startup is the one place that decides whether its process is
/// confined (its engine may not run inside the sandbox, and it then runs
/// headless-only instead of refusing to start). The first call wins.
pub fn set_desktop_resolution(resolution: ModeResolution) {
  let _ = DESKTOP.set(resolution);
}

/// The resolution [`set_desktop_resolution`] recorded.
pub fn desktop_resolution_recorded() -> Option<&'static ModeResolution> {
  DESKTOP.get()
}

/// `denort`'s guard before it builds the program's permissions: a baked
/// mode that fails closed (`enforce`, `broker`) runs only when the sandbox
/// was applied at startup (`deno_lib::kernel_apply::start_compiled`, which
/// records it in `kernel_sandbox_state`). A code path that reaches user code
/// without it (a runtime entry that doesn't call the startup, an OS without
/// an enforcer) refuses instead of running unconfined, unless `auto` chose
/// the mode: a mode `auto` chose never refuses to start (item 472), and
/// `start_compiled` has already fallen back to `warn`'s line for it.
pub fn check_compiled(
  baked: Option<KernelSandboxMode>,
  dev_build: bool,
  options: &PermissionsOptions,
) -> Result<ModeResolution, NoEnforcerError> {
  // a packaged desktop app's startup decided already
  if let Some(resolution) = DESKTOP.get() {
    return Ok(resolution.clone());
  }
  let mut resolution = compiled_resolution(baked, dev_build, options);
  if resolution.mode.fails_closed()
    && !deno_runtime::deno_permissions::kernel_sandbox_state::is_enforcing()
  {
    if !resolution.auto_chosen {
      return Err(NoEnforcerError(resolution.mode));
    }
    // the sandbox wasn't applied to this process: the program runs as it
    // did, held by Deno's checks
    resolution.mode = KernelSandboxMode::Off;
    resolution.reason = ModeReason::NotYetEnforced;
    resolution.auto_chosen = false;
  }
  Ok(resolution)
}

/// `DENO_KERNEL_SANDBOX` as the CLI read it at startup.
static ENV_OVERRIDE: OnceLock<Option<String>> = OnceLock::new();

/// Records `DENO_KERNEL_SANDBOX`, read by the CLI before any `--env-file` is
/// loaded so an env file can't set it. Later calls keep the first value.
pub fn snapshot_env_override(value: Option<String>) {
  let _ = ENV_OVERRIDE.set(value);
}

/// The break-glass override: `Ok(None)` when unset, `Ok(Some(off|warn))`, or
/// the raw value when it is anything else (the caller warns and ignores it).
pub fn env_override() -> Result<Option<KernelSandboxMode>, String> {
  match ENV_OVERRIDE.get().and_then(|v| v.as_deref()) {
    None | Some("") => Ok(None),
    Some("off") => Ok(Some(KernelSandboxMode::Off)),
    Some("warn") => Ok(Some(KernelSandboxMode::Warn)),
    Some(other) => Err(other.to_string()),
  }
}

/// `DENO_INTERNAL_KERNEL_SANDBOX_FORCE` as the CLI read it at startup.
static FORCED: OnceLock<Option<String>> = OnceLock::new();

/// Records the harness's [`KERNEL_SANDBOX_FORCE_ENV_VAR`], read with
/// `DENO_KERNEL_SANDBOX` (before any `--env-file`). Later calls keep the first
/// value.
pub fn snapshot_forced(value: Option<String>) {
  let _ = FORCED.set(value);
}

/// The harness's forced mode, or the raw value when it isn't one (the caller
/// warns and ignores it).
pub fn forced() -> Result<Option<ForcedEnforce>, String> {
  ForcedEnforce::parse(FORCED.get().and_then(|v| v.as_deref()))
}

/// The harness switches that were set without the harness's marker, as
/// the CLI found at startup ([`snapshot_harness_switches`]).
static HARNESS_IGNORED: OnceLock<Vec<&'static str>> = OnceLock::new();

/// Records the harness switches ([`snapshot_forced`], [`snapshot_stage`])
/// as the CLI read them at startup, with the harness's marker
/// ([`KERNEL_SANDBOX_HARNESS_ENV_VAR`]): without a marker file
/// ([`harness_marked`]) a switch that is set is not recorded (it has no
/// effect) and its name is kept for the caller's one notice
/// ([`harness_switches_ignored`]). Later calls keep the first values.
pub fn snapshot_harness_switches(
  forced: Option<String>,
  stage: Option<String>,
  marker: Option<&std::ffi::OsStr>,
) {
  let (forced, stage, ignored) =
    gate_harness_switches(forced, stage, || harness_marked(marker));
  snapshot_forced(forced);
  snapshot_stage(stage);
  let _ = HARNESS_IGNORED.set(ignored);
}

/// The harness switches that take effect, and the names of those that are
/// set and don't: all of them when `marked` (asked only when one is set)
/// says the harness's marker is missing. An empty value is an unset one.
fn gate_harness_switches(
  forced: Option<String>,
  stage: Option<String>,
  marked: impl FnOnce() -> bool,
) -> (Option<String>, Option<String>, Vec<&'static str>) {
  let forced = forced.filter(|v| !v.is_empty());
  let stage = stage.filter(|v| !v.is_empty());
  if (forced.is_none() && stage.is_none()) || marked() {
    return (forced, stage, Vec::new());
  }
  let mut ignored = Vec::new();
  if forced.is_some() {
    ignored.push(KERNEL_SANDBOX_FORCE_ENV_VAR);
  }
  if stage.is_some() {
    ignored.push(KERNEL_SANDBOX_STAGE_ENV_VAR);
  }
  (None, None, ignored)
}

/// The harness switches set in this process's environment that have no
/// effect, for want of the harness's marker.
pub fn harness_switches_ignored() -> &'static [&'static str] {
  HARNESS_IGNORED.get().map(|v| v.as_slice()).unwrap_or(&[])
}

/// `DENO_INTERNAL_KERNEL_SANDBOX_STAGE` as the CLI read it at startup.
static STAGE: OnceLock<Option<String>> = OnceLock::new();

/// Records the harness's [`KERNEL_SANDBOX_STAGE_ENV_VAR`], read with
/// `DENO_KERNEL_SANDBOX` (before any `--env-file`). Later calls keep the first
/// value.
pub fn snapshot_stage(value: Option<String>) {
  let _ = STAGE.set(value);
}

/// The harness's stage, or the raw value when it isn't one (the caller warns
/// and ignores it).
pub fn stage_override() -> Result<Option<AutoStage>, String> {
  AutoStage::parse(STAGE.get().and_then(|v| v.as_deref()))
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

  // denext (DENEXT.md item 5NN): the harness switches need the harness's
  // marker file
  #[test]
  fn harness_switches_need_the_marker() {
    let some = |v: &str| Some(v.to_string());
    // nothing set: the marker isn't even looked for
    assert_eq!(
      gate_harness_switches(None, some(""), || panic!("not asked")),
      (None, None, Vec::new())
    );
    // set, marked: as they are
    assert_eq!(
      gate_harness_switches(some("enforce"), some("r4"), || true),
      (some("enforce"), some("r4"), Vec::new())
    );
    // set, not marked: neither takes effect, each is named
    assert_eq!(
      gate_harness_switches(some("enforce"), some("r4"), || false),
      (
        None,
        None,
        vec![KERNEL_SANDBOX_FORCE_ENV_VAR, KERNEL_SANDBOX_STAGE_ENV_VAR]
      )
    );
    assert_eq!(
      gate_harness_switches(None, some("r4"), || false),
      (None, None, vec![KERNEL_SANDBOX_STAGE_ENV_VAR])
    );
    // (an invalid value is still the caller's to report, when marked)
    assert_eq!(
      gate_harness_switches(some("on"), None, || true),
      (some("on"), None, Vec::new())
    );
  }

  #[test]
  #[allow(
    clippy::disallowed_methods,
    reason = "the marker is a real file in the temp folder"
  )]
  fn the_harness_marker_is_a_file_that_exists() {
    use std::ffi::OsStr;
    let dir = std::env::temp_dir()
      .join(format!("deno-harness-marker-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let marker =
      dir.join(format!("{KERNEL_SANDBOX_HARNESS_FILE_PREFIX}abc123"));
    // named, not there yet (a variable that stayed behind in an image)
    assert!(!harness_marked(Some(marker.as_os_str())));
    std::fs::write(&marker, "").unwrap();
    assert!(harness_marked(Some(marker.as_os_str())));
    // unset, empty, relative, another name, a folder, the bare prefix
    assert!(!harness_marked(None));
    assert!(!harness_marked(Some(OsStr::new(""))));
    assert!(!harness_marked(Some(OsStr::new(
      "deno-kernel-sandbox-harness-abc123"
    ))));
    let other = dir.join("marker");
    std::fs::write(&other, "").unwrap();
    assert!(!harness_marked(Some(other.as_os_str())));
    let folder =
      dir.join(format!("{KERNEL_SANDBOX_HARNESS_FILE_PREFIX}folder"));
    std::fs::create_dir(&folder).unwrap();
    assert!(!harness_marked(Some(folder.as_os_str())));
    let bare = dir.join(KERNEL_SANDBOX_HARNESS_FILE_PREFIX);
    std::fs::write(&bare, "").unwrap();
    assert!(!harness_marked(Some(bare.as_os_str())));
    // a link to a marker is not one
    #[cfg(unix)]
    {
      let link = dir.join(format!("{KERNEL_SANDBOX_HARNESS_FILE_PREFIX}link"));
      std::os::unix::fs::symlink(&marker, &link).unwrap();
      assert!(!harness_marked(Some(link.as_os_str())));
    }
    std::fs::remove_dir_all(&dir).unwrap();
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
      assert!(
        !r.auto_chosen,
        "{}: the preview's auto applies nothing",
        row.name
      );
      assert_eq!(r.stage, AUTO_STAGE, "{}", row.name);
    }
  }

  /// The rollout stages of `auto` (DENEXT.md item 472). Each row is a run
  /// and what `auto` applies to it in the preview, at R3 and at R4.
  #[test]
  fn auto_stage_matrix() {
    let tty = |mut i: ModeInputs| {
      i.terminals = true;
      i
    };
    let compiled = || ModeInputs {
      kind: Some(RunKind::Compiled),
      ..run()
    };
    // (name, inputs, [preview, r3, r4])
    let rows: Vec<(&str, ModeInputs, [KernelSandboxMode; 3])> = vec![
      (
        "a bare run with no terminal (CI): warn at R3, enforce at R4",
        run(),
        [Off, Warn, Enforce],
      ),
      (
        "--no-prompt on a terminal: the same",
        ModeInputs {
          prompt: false,
          ..tty(run())
        },
        [Off, Warn, Enforce],
      ),
      (
        "a compiled program: enforce from R3",
        compiled(),
        [Off, Enforce, Enforce],
      ),
      (
        "a compiled program on a terminal with prompting: fixed all the same",
        tty(compiled()),
        [Off, Enforce, Enforce],
      ),
      (
        "auto written in a flag (denext start): enforce from R3",
        ModeInputs {
          flag: Some(Auto),
          ..run()
        },
        [Off, Enforce, Enforce],
      ),
      (
        "auto written in deno.json: enforce from R3",
        ModeInputs {
          config: Some(Auto),
          ..run()
        },
        [Off, Enforce, Enforce],
      ),
      (
        "-A with a deny, no terminal: the denies, warn then enforce",
        ModeInputs {
          allow_all: true,
          has_deny: true,
          ..run()
        },
        [Off, Warn, Enforce],
      ),
      (
        "jupyter never prompts",
        ModeInputs {
          kind: Some(RunKind::Jupyter),
          ..tty(run())
        },
        [Off, Warn, Enforce],
      ),
      // never changed by a stage
      ("a prompt can happen", tty(run()), [Off, Off, Off]),
      (
        "explicit auto while a prompt can happen",
        ModeInputs {
          flag: Some(Auto),
          ..tty(run())
        },
        [Off, Off, Off],
      ),
      (
        "plain -A",
        ModeInputs {
          allow_all: true,
          ..run()
        },
        [Off, Off, Off],
      ),
      (
        "--watch",
        ModeInputs {
          watch: true,
          ..run()
        },
        [Off, Off, Off],
      ),
      (
        "denext dev",
        ModeInputs {
          denext_dev: true,
          ..run()
        },
        [Off, Off, Off],
      ),
      (
        "a permission broker",
        ModeInputs {
          permission_broker: true,
          ..run()
        },
        [Off, Off, Off],
      ),
      (
        "not a runtime",
        ModeInputs {
          kind: Some(RunKind::NotARuntime),
          ..run()
        },
        [Off, Off, Off],
      ),
      (
        "a compiled desktop development build",
        ModeInputs {
          dev_build: true,
          ..compiled()
        },
        [Off, Off, Off],
      ),
      (
        "DENO_KERNEL_SANDBOX=off (break-glass)",
        ModeInputs {
          env: Some(Off),
          ..run()
        },
        [Off, Off, Off],
      ),
      (
        "DENO_KERNEL_SANDBOX=warn lowers R4's enforce",
        ModeInputs {
          env: Some(Warn),
          ..run()
        },
        [Warn, Warn, Warn],
      ),
      (
        "deno.json off",
        ModeInputs {
          config: Some(Off),
          ..run()
        },
        [Off, Off, Off],
      ),
      (
        "a compiled program baked off",
        ModeInputs {
          baked: Some(Off),
          ..compiled()
        },
        [Off, Off, Off],
      ),
      (
        "an explicit enforce",
        ModeInputs {
          flag: Some(Enforce),
          ..run()
        },
        [Enforce, Enforce, Enforce],
      ),
      (
        "inside an npm lifecycle script's jail (no terminal): the jail confines it",
        ModeInputs {
          lifecycle_jail: true,
          ..run()
        },
        [Off, Off, Off],
      ),
      (
        "inside the lifecycle jail, auto written in deno.json: still the jail's",
        ModeInputs {
          lifecycle_jail: true,
          config: Some(Auto),
          ..run()
        },
        [Off, Off, Off],
      ),
      (
        "inside the lifecycle jail, an explicit enforce applies as asked",
        ModeInputs {
          lifecycle_jail: true,
          flag: Some(Enforce),
          ..run()
        },
        [Enforce, Enforce, Enforce],
      ),
    ];
    let stages = [AutoStage::Preview, AutoStage::R3, AutoStage::R4];
    for (name, inputs, expected) in rows {
      for (stage, want) in stages.iter().zip(expected) {
        let r = resolve(&ModeInputs {
          stage: Some(*stage),
          ..inputs.clone()
        });
        assert_eq!(r.mode, want, "{name} at {stage:?}");
        assert_eq!(r.stage, *stage, "{name} at {stage:?}");
        // only a mode `auto` chose is marked, and it is never the preview's
        let chosen = inputs.flag.is_none_or(|m| m == Auto)
          && inputs.env.is_none()
          && inputs.baked.is_none()
          && inputs.config.is_none_or(|m| m == Auto)
          && want != Off;
        assert_eq!(r.auto_chosen, chosen, "{name} at {stage:?}");
        if r.auto_chosen {
          assert!(
            matches!(
              r.reason,
              ModeReason::AutoAuthorFixed | ModeReason::AutoNoPrompt
            ),
            "{name} at {stage:?}"
          );
          assert!(r.fixed, "{name} at {stage:?}");
        }
      }
    }
    // the reasons say which kind of fixed
    let r = resolve(&ModeInputs {
      stage: Some(AutoStage::R3),
      ..run()
    });
    assert_eq!(r.reason, ModeReason::AutoNoPrompt);
    assert_eq!(r.would_apply, AutoPlan::Warn);
    let r = resolve(&ModeInputs {
      stage: Some(AutoStage::R3),
      ..compiled()
    });
    assert_eq!(r.reason, ModeReason::AutoAuthorFixed);
    // what R4 applies is what it records
    let r = resolve(&ModeInputs {
      stage: Some(AutoStage::R4),
      ..run()
    });
    assert_eq!(r.would_apply, AutoPlan::Enforce);
    // an unfixed run under an auto stage is never an error
    for stage in stages {
      assert!(
        kernel_sandbox_mode(&ModeInputs {
          stage: Some(stage),
          watch: true,
          ..run()
        })
        .is_ok()
      );
    }
  }

  /// `auto_stage_matrix`'s denext.30 rows: packaged desktop apps (WP-C).
  /// What `auto` applies at each stage; `[preview, r3, r4]` as there.
  #[test]
  fn auto_stage_matrix_desktop_packaged() {
    let desktop = |strict, engine| ModeInputs {
      kind: Some(RunKind::DesktopPackaged { strict, engine }),
      ..run()
    };
    let rows: Vec<(&str, ModeInputs, [KernelSandboxMode; 3])> = vec![
      (
        "a strict desktop package (webview): enforce from R3",
        desktop(true, Engine::Webview),
        [Off, Enforce, Enforce],
      ),
      (
        "a strict desktop package (CEF): enforce from R3 (headless-only where the engine can't nest is the enforcer's call)",
        desktop(true, Engine::Cef),
        [Off, Enforce, Enforce],
      ),
      (
        "a desktop package that isn't strict (webview): auto applies nothing at any stage (the R4 decision)",
        desktop(false, Engine::Webview),
        [Off, Off, Off],
      ),
      (
        "a desktop package that isn't strict (CEF): the same",
        desktop(false, Engine::Cef),
        [Off, Off, Off],
      ),
      (
        "a desktop package that isn't strict, -A with denies: no deny ceiling either",
        ModeInputs {
          allow_all: true,
          has_deny: true,
          ..desktop(false, Engine::Webview)
        },
        [Off, Off, Off],
      ),
      (
        "a strict desktop package launched from a terminal: fixed all the same",
        ModeInputs {
          terminals: true,
          ..desktop(true, Engine::Webview)
        },
        [Off, Enforce, Enforce],
      ),
      (
        "a desktop development build",
        ModeInputs {
          dev_build: true,
          ..desktop(true, Engine::Webview)
        },
        [Off, Off, Off],
      ),
    ];
    let stages = [AutoStage::Preview, AutoStage::R3, AutoStage::R4];
    for (name, inputs, expected) in rows {
      for (stage, want) in stages.iter().zip(expected) {
        let r = resolve(&ModeInputs {
          stage: Some(*stage),
          ..inputs.clone()
        });
        assert_eq!(r.mode, want, "{name} at {stage:?}");
        if want != Off {
          assert!(r.auto_chosen, "{name} at {stage:?}");
          assert_eq!(r.reason, ModeReason::AutoAuthorFixed, "{name}");
        }
        if matches!(
          inputs.kind,
          Some(RunKind::DesktopPackaged { strict: false, .. })
        ) {
          // not "not yet": no later stage changes it
          assert_eq!(r.reason, ModeReason::DesktopNotStrict, "{name}");
          assert_eq!(r.would_apply, AutoPlan::Off, "{name} at {stage:?}");
          assert!(!r.auto_chosen && !r.deny_ceiling(), "{name}");
        }
      }
    }
    // what isn't `auto` is the author's word and applies as baked, strict
    // or not: `warn` reports, and `off` is off
    let r = resolve(&ModeInputs {
      baked: Some(Warn),
      stage: Some(AutoStage::R4),
      ..desktop(false, Engine::Webview)
    });
    assert_eq!((r.mode, r.reason), (Warn, ModeReason::Requested));
    // the second decision (a policy with the broker's shape) changes
    // nothing for a desktop app that isn't strict
    let r = resolve(&ModeInputs {
      policy_needs_supervisor: true,
      stage: Some(AutoStage::R4),
      ..desktop(false, Engine::Webview)
    });
    assert_eq!((r.mode, r.reason), (Off, ModeReason::DesktopNotStrict));
    assert!(!r.needs_supervisor);
  }

  // a packaged desktop app's mode is its baked one: the flag, the
  // environment, deno.json and the harness's switch are not read, a
  // terminal doesn't unfix it, and `strict` is a baked enforce
  #[test]
  fn desktop_packaged_reads_only_the_baked_mode() {
    let desktop = |engine| ModeInputs {
      kind: Some(RunKind::DesktopPackaged {
        strict: true,
        engine,
      }),
      flag: Some(Off),
      env: Some(Off),
      config: Some(Warn),
      forced: Some(ForcedEnforce::Project),
      terminals: true,
      prompt: true,
      ..Default::default()
    };
    for engine in [Engine::Webview, Engine::Cef] {
      let r = resolve(&ModeInputs {
        baked: Some(Enforce),
        ..desktop(engine)
      });
      assert_eq!((r.mode, r.source), (Enforce, ModeSource::Compiled));
      assert!(r.fixed && !r.auto_chosen);
      let r = resolve(&ModeInputs {
        baked: Some(Off),
        ..desktop(engine)
      });
      assert_eq!((r.mode, r.reason), (Off, ModeReason::TurnedOff));
      // nothing baked: auto, which the preview doesn't apply
      let r = resolve(&desktop(engine));
      assert_eq!((r.requested, r.source), (Auto, ModeSource::Default));
      assert_eq!(r.would_apply, AutoPlan::Enforce);
      // a development build is off whatever is baked
      let r = resolve(&ModeInputs {
        baked: Some(Enforce),
        dev_build: true,
        ..desktop(engine)
      });
      assert_eq!((r.mode, r.reason), (Off, ModeReason::DevBuild));
      // -A with no deny leaves nothing to narrow
      let r = resolve(&ModeInputs {
        baked: Some(Enforce),
        allow_all: true,
        ..desktop(engine)
      });
      assert_eq!((r.mode, r.reason), (Off, ModeReason::AllowAll));
    }
    let options = PermissionsOptions {
      allow_read: Some(vec!["/data".to_string()]),
      prompt: true,
      ..Default::default()
    };
    let r =
      desktop_resolution(Some(Enforce), false, &options, Engine::Cef, None);
    assert_eq!(r.mode, Enforce);
    assert!(
      !desktop_resolution(Some(Enforce), true, &options, Engine::Cef, None)
        .mode
        .fails_closed()
    );
    // the harness's stage raises what auto applies, never what is baked
    let auto = |stage| {
      desktop_resolution(None, false, &options, Engine::Webview, stage).mode
    };
    // (nothing baked is a package that isn't strict: off at every stage)
    assert_eq!(auto(None), Off);
    assert_eq!(auto(Some(AutoStage::R4)), Off);
    assert_eq!(
      desktop_resolution(
        None,
        false,
        &options,
        Engine::Webview,
        Some(AutoStage::R4)
      )
      .reason,
      ModeReason::DesktopNotStrict
    );
    assert_eq!(
      desktop_resolution(
        Some(Off),
        false,
        &options,
        Engine::Webview,
        Some(AutoStage::R4)
      )
      .mode,
      Off
    );
    assert_eq!(Engine::from_backend(Some("cef")), Engine::Cef);
    assert_eq!(Engine::from_backend(Some("webview")), Engine::Webview);
    assert_eq!(Engine::from_backend(None), Engine::Webview);
    // gpui gets its own engine (and its own profile); a backend this
    // runtime doesn't name keeps the OS web view's profile, never none.
    assert_eq!(Engine::from_backend(Some("gpui")), Engine::Gpui);
    assert_eq!(Engine::Gpui.as_str(), "gpui");
    assert_eq!(Engine::from_backend(Some("raw")), Engine::Webview);
    assert_eq!(Engine::from_backend(Some("unknown")), Engine::Webview);
  }

  /// `auto_stage_matrix`'s denext.30 row: a policy only the broker can hold
  /// (host-scoped network, a write deny inside an allow) goes to the
  /// supervisor at R4 instead of falling back to warn (WP-B).
  #[test]
  fn auto_stage_matrix_broker_closable() {
    let inputs = ModeInputs {
      policy_needs_supervisor: true,
      ..run()
    };
    let r = resolve(&ModeInputs {
      stage: Some(AutoStage::R4),
      ..inputs.clone()
    });
    assert_eq!(r.would_apply, AutoPlan::Broker);
    assert_eq!(r.mode, Broker);
    assert!(r.needs_supervisor);
    assert!(r.auto_chosen);
    // the preview records it and applies nothing
    let r = resolve(&ModeInputs {
      stage: Some(AutoStage::Preview),
      ..inputs
    });
    assert_eq!(r.mode, Off);
    assert_eq!(r.would_apply, AutoPlan::Broker);
    assert!(!r.auto_chosen);
  }

  /// The rest of the broker's rows: where `auto` would enforce it takes the
  /// supervisor, where it only warns it still warns, and nothing that keeps
  /// `auto` off is changed by what the policy needs.
  #[test]
  fn auto_stage_matrix_broker_rows() {
    let tty = |mut i: ModeInputs| {
      i.terminals = true;
      i
    };
    let needs = |i: ModeInputs| ModeInputs {
      policy_needs_supervisor: true,
      ..i
    };
    let compiled = || ModeInputs {
      kind: Some(RunKind::Compiled),
      ..run()
    };
    // (name, inputs, [preview, r3, r4])
    let rows: Vec<(&str, ModeInputs, [KernelSandboxMode; 3])> = vec![
      (
        "a bare run with no terminal: warn at R3, the supervisor at R4",
        needs(run()),
        [Off, Warn, Broker],
      ),
      (
        "a compiled program: the supervisor from R3",
        needs(compiled()),
        [Off, Broker, Broker],
      ),
      (
        "auto written in a flag (denext start): the supervisor from R3",
        needs(ModeInputs {
          flag: Some(Auto),
          ..run()
        }),
        [Off, Broker, Broker],
      ),
      // never changed by what the policy needs
      ("a prompt can happen", needs(tty(run())), [Off, Off, Off]),
      (
        "plain -A",
        needs(ModeInputs {
          allow_all: true,
          ..run()
        }),
        [Off, Off, Off],
      ),
      (
        "--watch",
        needs(ModeInputs {
          watch: true,
          ..run()
        }),
        [Off, Off, Off],
      ),
      (
        "an explicit off",
        needs(ModeInputs {
          flag: Some(Off),
          ..run()
        }),
        [Off, Off, Off],
      ),
      (
        "DENO_KERNEL_SANDBOX=warn lowers the supervisor too",
        needs(ModeInputs {
          env: Some(Warn),
          ..run()
        }),
        [Warn, Warn, Warn],
      ),
      (
        "an explicit enforce stays enforce (it takes the broker by itself)",
        needs(ModeInputs {
          flag: Some(Enforce),
          ..run()
        }),
        [Enforce, Enforce, Enforce],
      ),
    ];
    let stages = [AutoStage::Preview, AutoStage::R3, AutoStage::R4];
    for (name, inputs, expected) in rows {
      for (stage, want) in stages.iter().zip(expected) {
        let r = resolve(&ModeInputs {
          stage: Some(*stage),
          ..inputs.clone()
        });
        assert_eq!(r.mode, want, "{name} at {stage:?}");
        // the supervisor is needed exactly where a mode is decided at all
        let decided = r.reason != ModeReason::Prompt
          && r.reason != ModeReason::AllowAll
          && r.reason != ModeReason::Watch
          && r.reason != ModeReason::TurnedOff;
        assert_eq!(r.needs_supervisor, decided, "{name} at {stage:?}");
        if r.mode == Broker {
          assert!(r.auto_chosen, "{name} at {stage:?}");
          assert_eq!(r.would_apply, AutoPlan::Broker, "{name} at {stage:?}");
          assert!(r.fixed, "{name} at {stage:?}");
        }
      }
    }
    // a run R3 only warns about records the broker as what would apply
    let r = resolve(&ModeInputs {
      stage: Some(AutoStage::R3),
      ..needs(run())
    });
    assert_eq!((r.mode, r.would_apply), (Warn, AutoPlan::Broker));
    assert_eq!(r.reason, ModeReason::AutoNoPrompt);
    // without the input nothing changed: every run decides as before
    let r = resolve(&ModeInputs {
      stage: Some(AutoStage::R4),
      ..run()
    });
    assert_eq!((r.mode, r.needs_supervisor), (Enforce, false));
  }

  /// `auto_stage_matrix`'s denext.30 rows: a run where a prompt can happen
  /// enforces its deny rules only (D1), and `-A --deny-*` does the same on
  /// a terminal (D2). WP-A.
  #[test]
  fn auto_stage_matrix_deny_only_prompt() {
    let tty = |mut i: ModeInputs| {
      i.terminals = true;
      i
    };
    let rows: Vec<(&str, ModeInputs)> = vec![
      (
        "a prompt can happen, with a --deny-*: the denies (D1)",
        ModeInputs {
          has_deny: true,
          ..tty(run())
        },
      ),
      (
        "-A with a --deny-* on a terminal: the denies (D2)",
        ModeInputs {
          allow_all: true,
          has_deny: true,
          ..tty(run())
        },
      ),
    ];
    for (name, inputs) in rows {
      let r = resolve(&ModeInputs {
        stage: Some(AutoStage::R4),
        ..inputs.clone()
      });
      assert_eq!(r.mode, Enforce, "{name}");
      assert!(r.deny_only, "{name}");
      assert!(r.auto_chosen, "{name}");
      // the prompt stays: the set isn't fixed, only its ceiling is
      assert!(!r.fixed, "{name}");
      let r = resolve(&ModeInputs {
        stage: Some(AutoStage::Preview),
        ..inputs
      });
      assert_eq!(r.mode, Off, "{name}");
    }
    // with no deny a prompt still means off at every stage
    let r = resolve(&ModeInputs {
      stage: Some(AutoStage::R4),
      ..tty(run())
    });
    assert_eq!(r.mode, Off);
  }

  /// The deny-only rows (D1, D2): who gets the deny ceiling, at which
  /// stage, and why. `(name, inputs, stage, mode, deny_only, ceiling,
  /// reason)`.
  #[test]
  fn deny_only_matrix() {
    let tty = |mut i: ModeInputs| {
      i.terminals = true;
      i
    };
    let deny = |i: ModeInputs| ModeInputs {
      has_deny: true,
      ..i
    };
    let all = |i: ModeInputs| ModeInputs {
      allow_all: true,
      ..i
    };
    let compiled = || ModeInputs {
      kind: Some(RunKind::Compiled),
      ..run()
    };
    use AutoStage::*;
    #[allow(clippy::type_complexity, reason = "a table")]
    let rows: Vec<(
      &str,
      ModeInputs,
      AutoStage,
      KernelSandboxMode,
      bool,
      bool,
      ModeReason,
    )> = vec![
      (
        "prompt + deny at R4: the ceiling",
        deny(tty(run())),
        R4,
        Enforce,
        true,
        true,
        ModeReason::AutoDenyCeiling,
      ),
      (
        "prompt + deny at R3: nothing yet (D1 is an R4 default)",
        deny(tty(run())),
        R3,
        Off,
        false,
        false,
        ModeReason::Prompt,
      ),
      (
        "prompt + deny in the preview: nothing",
        deny(tty(run())),
        Preview,
        Off,
        false,
        false,
        ModeReason::Prompt,
      ),
      (
        "prompt, no deny, at R4: off",
        tty(run()),
        R4,
        Off,
        false,
        false,
        ModeReason::Prompt,
      ),
      (
        "-A + deny on a terminal at R4: the ceiling (D2)",
        all(deny(tty(run()))),
        R4,
        Enforce,
        true,
        true,
        ModeReason::AutoDenyCeiling,
      ),
      (
        "-A + deny, no terminal, at R4: the ceiling (D2)",
        all(deny(run())),
        R4,
        Enforce,
        true,
        true,
        ModeReason::AutoNoPrompt,
      ),
      (
        "-A + deny, no terminal, at R3: warn, deny-only reported",
        all(deny(run())),
        R3,
        Warn,
        true,
        false,
        ModeReason::AutoNoPrompt,
      ),
      (
        "a partial grant + deny, no terminal, at R4: the whole policy",
        deny(run()),
        R4,
        Enforce,
        false,
        false,
        ModeReason::AutoNoPrompt,
      ),
      (
        "explicit enforce + deny on a terminal: the whole policy, as asked",
        ModeInputs {
          flag: Some(Enforce),
          ..deny(tty(run()))
        },
        R4,
        Enforce,
        false,
        false,
        ModeReason::Requested,
      ),
      (
        "explicit enforce with -A + deny: the denies, but not the ceiling (seccomp, expansion as enforce does)",
        ModeInputs {
          flag: Some(Enforce),
          ..all(deny(run()))
        },
        R4,
        Enforce,
        true,
        false,
        ModeReason::Requested,
      ),
      (
        "explicit warn + deny on a terminal: warn",
        ModeInputs {
          flag: Some(Warn),
          ..deny(tty(run()))
        },
        R4,
        Warn,
        false,
        false,
        ModeReason::Requested,
      ),
      (
        "DENO_KERNEL_SANDBOX=off + deny on a terminal: off",
        ModeInputs {
          env: Some(Off),
          ..deny(tty(run()))
        },
        R4,
        Off,
        false,
        false,
        ModeReason::TurnedOff,
      ),
      (
        "--watch + deny on a terminal: off (not fixed at all)",
        ModeInputs {
          watch: true,
          ..deny(tty(run()))
        },
        R4,
        Off,
        false,
        false,
        ModeReason::Watch,
      ),
      (
        "a compiled program with -A + deny: the ceiling from R3",
        all(deny(compiled())),
        R3,
        Enforce,
        true,
        true,
        ModeReason::AutoAuthorFixed,
      ),
      (
        "a compiled program with a partial grant + deny on a terminal: the whole policy",
        deny(tty(compiled())),
        R4,
        Enforce,
        false,
        false,
        ModeReason::AutoAuthorFixed,
      ),
      (
        "repl on a terminal with a deny: the ceiling at R4",
        ModeInputs {
          kind: Some(RunKind::Repl),
          ..deny(tty(run()))
        },
        R4,
        Enforce,
        true,
        true,
        ModeReason::AutoDenyCeiling,
      ),
    ];
    for (name, inputs, stage, mode, deny_only, ceiling, reason) in rows {
      let r = resolve(&ModeInputs {
        stage: Some(stage),
        ..inputs
      });
      assert_eq!(r.mode, mode, "{name}");
      assert_eq!(r.deny_only, deny_only, "{name}");
      assert_eq!(r.deny_ceiling(), ceiling, "{name}");
      assert_eq!(r.reason, reason, "{name}");
    }
  }

  /// In prompt mode with deny rules (D1) the prompt stays: the set isn't
  /// fixed, only its ceiling is, and the plan says enforce.
  #[test]
  fn prompt_mode_denies() {
    let inputs = ModeInputs {
      terminals: true,
      has_deny: true,
      stage: Some(AutoStage::R4),
      ..run()
    };
    let r = resolve(&inputs);
    assert!(!r.fixed, "a prompt can still widen the rest");
    assert_eq!(r.would_apply, AutoPlan::Enforce);
    assert_eq!(r.source, ModeSource::Default);
    assert!(r.auto_chosen && r.deny_ceiling());
    // explicit auto (a flag or deno.json) behaves the same
    for i in [
      ModeInputs {
        flag: Some(Auto),
        ..inputs.clone()
      },
      ModeInputs {
        config: Some(Auto),
        ..inputs.clone()
      },
    ] {
      let r = resolve(&i);
      assert_eq!((r.mode, r.reason), (Enforce, ModeReason::AutoDenyCeiling));
      assert!(r.deny_ceiling());
    }
    // --no-prompt on the same terminal: fixed, so the whole policy
    let r = resolve(&ModeInputs {
      prompt: false,
      ..inputs.clone()
    });
    assert!(r.fixed && !r.deny_only && !r.deny_ceiling());
    assert_eq!(r.mode, Enforce);
    // a permission broker: never fixed, never a ceiling
    let r = resolve(&ModeInputs {
      permission_broker: true,
      ..inputs.clone()
    });
    assert_eq!((r.mode, r.reason), (Off, ModeReason::PermissionBroker));
    // jupyter has no prompt: its set is fixed, the whole policy
    let r = resolve(&ModeInputs {
      kind: Some(RunKind::Jupyter),
      ..inputs.clone()
    });
    assert!(r.fixed && !r.deny_ceiling());
    // the harness's forced enforce: the whole policy, never the ceiling
    let r = resolve(&ModeInputs {
      forced: Some(ForcedEnforce::Enforce),
      ..inputs
    });
    assert_eq!((r.mode, r.source), (Enforce, ModeSource::Forced));
    assert!(!r.deny_ceiling());
    // the reasons say what they are
    assert!(
      ModeReason::AutoDenyCeiling
        .describe()
        .contains("deny rules")
    );
    assert!(
      ModeReason::DenyCeilingDenoOnly
        .describe()
        .contains("Deno's checks only")
    );
  }

  #[test]
  fn the_stage_variable_only_raises() {
    assert_eq!(AutoStage::parse(None), Ok(None));
    assert_eq!(AutoStage::parse(Some("")), Ok(None));
    assert_eq!(AutoStage::parse(Some("r3")), Ok(Some(AutoStage::R3)));
    assert_eq!(AutoStage::parse(Some("r4")), Ok(Some(AutoStage::R4)));
    // the preview can't be asked for: the variable never lowers the stage
    assert!(AutoStage::parse(Some("preview")).is_err());
    assert!(AutoStage::parse(Some("R4")).is_err());
    assert!(
      AutoStage::Preview < AutoStage::R3 && AutoStage::R3 < AutoStage::R4
    );
    // a stage before the shipped one is raised to it
    let r = resolve(&ModeInputs {
      stage: Some(AutoStage::Preview),
      ..run()
    });
    assert_eq!(r.stage, AUTO_STAGE);
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
    // no baked mode: `auto`, which at this stage applies nothing
    let r = check_compiled(None, false, &options).unwrap();
    assert_eq!((r.mode, r.auto_chosen), (Off, false));
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

  /// The harness's forced enforce (item 451): it raises `auto`, `warn` and
  /// `-A` to `enforce`, never an explicit `off`, and leaves an unfixed run
  /// as it was (no error).
  #[test]
  fn forced_enforce_rows() {
    let forced = |i: ModeInputs| ModeInputs {
      forced: Some(ForcedEnforce::Enforce),
      ..i
    };
    // a default run
    let r = resolve(&forced(run()));
    assert_eq!((r.mode, r.source), (Enforce, ModeSource::Forced));
    assert!(r.fixed);
    // -A with no deny: forced too (unforced, it is off)
    let all = ModeInputs {
      allow_all: true,
      ..run()
    };
    assert_eq!(resolve(&all).reason, ModeReason::AllowAll);
    assert_eq!(resolve(&forced(all)).mode, Enforce);
    // a warn flag, config or variable is raised
    for i in [
      ModeInputs {
        flag: Some(Warn),
        ..run()
      },
      ModeInputs {
        env: Some(Warn),
        ..run()
      },
      ModeInputs {
        config: Some(Auto),
        ..run()
      },
    ] {
      assert_eq!(resolve(&forced(i)).mode, Enforce);
    }
    // an explicit off wins (flag or break-glass variable)
    for i in [
      ModeInputs {
        flag: Some(Off),
        ..run()
      },
      ModeInputs {
        env: Some(Off),
        ..run()
      },
      ModeInputs {
        config: Some(Off),
        ..run()
      },
    ] {
      let r = resolve(&forced(i));
      assert_eq!((r.mode, r.reason), (Off, ModeReason::TurnedOff));
    }
    // unfixed: stays off, never the error an explicit enforce gets
    let watch = ModeInputs {
      watch: true,
      ..run()
    };
    let r = resolve(&forced(watch.clone()));
    assert_eq!((r.mode, r.reason), (Off, ModeReason::Watch));
    assert!(
      kernel_sandbox_mode(&ModeInputs {
        flag: Some(Enforce),
        ..watch
      })
      .is_err()
    );
    // a prompt that could happen no longer keeps it off
    let tty = ModeInputs {
      terminals: true,
      ..run()
    };
    assert_eq!(resolve(&forced(tty)).mode, Enforce);
    // not a runtime: off
    let r = resolve(&forced(ModeInputs {
      kind: Some(RunKind::NotARuntime),
      ..Default::default()
    }));
    assert_eq!(r.mode, Off);
    // the variable's values
    assert_eq!(ForcedEnforce::parse(None), Ok(None));
    assert_eq!(ForcedEnforce::parse(Some("")), Ok(None));
    assert_eq!(
      ForcedEnforce::parse(Some("project")),
      Ok(Some(ForcedEnforce::Project))
    );
    assert!(ForcedEnforce::parse(Some("on")).is_err());
  }
}
