// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: what the CLI and `denort` share when the kernel sandbox applies
//! (DENEXT.md items 425 and 901): the machine's mechanisms, the context
//! pieces read from the process, a compiled program's startup
//! ([`start_compiled`]) and a packaged desktop app's ([`start_desktop`],
//! item 488).
//!
//! A compiled program applies its sandbox in-process, with no re-exec: its
//! grant is baked, its modules are embedded, and it has fetched nothing, so
//! the first image is already the one that may be confined. `denort` calls
//! [`start_compiled`] first thing in `main`, while the process has one
//! thread and has opened nothing (Linux: Landlock and seccomp apply to the
//! calling thread only; macOS: `sandbox_init` is process-wide, but what is
//! already open stays open).
//!
//! Where the baked grant has a rule only the supervising broker holds (a
//! host-scoped network, a write deny inside a write allow on Linux), the
//! program forks first, in place: the parent stays as the supervisor (one
//! thread at the fork, no JavaScript engine, none of the embedded modules
//! loaded, ever), and the child confines itself and runs the program
//! (`kernel_supervise::fork_supervised`). The supervisor keeps the PID the
//! program was started with and exits as the program exits.

#![allow(
  clippy::disallowed_methods,
  reason = "the sandbox acts on the real file system and environment, never a virtual one"
)]

use std::path::Path;
use std::path::PathBuf;

use deno_runtime::deno_permissions::PermissionsOptions;
use deno_runtime::deno_permissions::kernel_sandbox_state;
use deno_sandbox::plan::EnforcePlan;

use crate::kernel_desktop::DesktopReport;
use crate::kernel_desktop::DesktopSandbox;
use crate::kernel_desktop::EngineFacts;
use crate::kernel_desktop::engine_nests;
use crate::kernel_inherit;
use crate::kernel_plan::plan_for;
use crate::kernel_policy::Capabilities;
use crate::kernel_policy::CompiledPolicy;
use crate::kernel_policy::Context;
use crate::kernel_policy::GapResolution;
use crate::kernel_policy::ImplicitSet;
use crate::kernel_policy::KernelPolicy;
use crate::kernel_policy::Shape;
use crate::kernel_policy::TargetOs;
use crate::kernel_sandbox::Engine;
use crate::kernel_sandbox::KernelSandboxMode;
use crate::kernel_sandbox::ModeResolution;
use crate::kernel_sandbox::compiled_resolution;
use crate::kernel_sandbox::compiled_resolution_with;
use crate::kernel_sandbox::desktop_resolution;

/// The mechanisms this machine offers, and whether this runtime has a Mode A
/// enforcer for them (Linux on x86_64 / aarch64, macOS).
pub fn detect_capabilities() -> Capabilities {
  let os = TargetOs::current();
  #[allow(unused_mut, reason = "set per OS")]
  let mut caps = Capabilities::none(os);
  #[cfg(target_os = "linux")]
  {
    caps.landlock_abi = deno_sandbox::linux::landlock_abi().ok();
    // the seccomp programs are assembled for these architectures (the frame
    // kills other architectures' syscalls); whether this process may
    // install one (an outer profile may refuse it) is probed
    let arch = cfg!(any(target_arch = "x86_64", target_arch = "aarch64"));
    caps.seccomp =
      arch && deno_sandbox::enforce_linux::seccomp_available().is_ok();
    caps.user_namespaces = deno_sandbox::linux::netns_available().is_ok();
    caps.enforcer_built = arch;
  }
  #[cfg(target_os = "macos")]
  {
    caps.seatbelt = deno_sandbox::macos::sandbox_init_available()
      || deno_sandbox::macos::available().is_ok();
    caps.enforcer_built = true;
  }
  #[cfg(windows)]
  {
    caps.job_object = true;
    caps.low_integrity = true;
  }
  caps
}

/// The terminals the standard streams are (`ttyname` of fds 0 to 2),
/// deduplicated.
pub fn std_stream_ttys() -> Vec<PathBuf> {
  #[allow(unused_mut, reason = "filled on unix")]
  let mut out = Vec::new();
  #[cfg(unix)]
  for fd in 0..=2 {
    use std::os::unix::ffi::OsStrExt;
    let mut buf = [0u8; 256];
    // SAFETY: a buffer of the given size; isatty / ttyname_r have no other
    // preconditions
    let ok = unsafe {
      libc::isatty(fd) == 1
        && libc::ttyname_r(fd, buf.as_mut_ptr() as *mut libc::c_char, buf.len())
          == 0
    };
    if !ok {
      continue;
    }
    let len = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    let path =
      PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len]).to_os_string());
    if path.is_absolute() && !out.contains(&path) {
      out.push(path);
    }
  }
  out.sort();
  out
}

/// The certificate files and folders the environment names (read only with
/// network; each variable is kept through the environment scrub).
pub fn tls_files() -> Vec<PathBuf> {
  let mut out: Vec<PathBuf> = [
    "DENO_CERT",
    "NODE_EXTRA_CA_CERTS",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
  ]
  .iter()
  .filter_map(std::env::var_os)
  .flat_map(|v| {
    // SSL_CERT_DIR may list several folders
    std::env::split_paths(&v).collect::<Vec<_>>()
  })
  .filter(|p| p.is_absolute())
  .collect();
  out.sort();
  out.dedup();
  out
}

/// Creates a cache overlay folder, the user's alone (0700, its parent too),
/// in the image that isn't confined yet. A failure leaves it absent: the
/// policy then has no overlay and the caches stay in memory.
pub fn create_cache_overlay(overlay: &Path) {
  let created = std::fs::create_dir_all(overlay).is_ok();
  #[cfg(not(unix))]
  let _ = created;
  #[cfg(unix)]
  if created {
    use std::os::unix::fs::PermissionsExt;
    let parent = overlay.parent().unwrap_or(overlay);
    for dir in [parent, overlay] {
      let _ =
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
  }
}

/// Whether `dir` is a folder of this user alone: a real directory (no
/// symlink), owned by the effective user, mode 0700. A compiled program's
/// cache folder lives in the temp folder, which other users can write on
/// Linux, so one that somebody else made is never used. On Windows the temp
/// folder is the user's own: a real directory is enough.
pub fn is_private_dir(dir: &Path) -> bool {
  let Ok(metadata) = std::fs::symlink_metadata(dir) else {
    return false;
  };
  if metadata.file_type().is_symlink() || !metadata.is_dir() {
    return false;
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: no arguments, no failure
    let uid = unsafe { libc::geteuid() };
    if metadata.uid() != uid || metadata.permissions().mode() & 0o777 != 0o700 {
      return false;
    }
  }
  true
}

/// `dir` by its canonical path where the policy names paths that way (the
/// temp folder is behind a symlink on macOS). Windows keeps the path as it
/// is: a canonical path there is a verbatim one (`\\?\C:\...`), which the
/// write roots' labels and their guard don't need.
pub fn canonical_dir(dir: PathBuf) -> PathBuf {
  if cfg!(windows) {
    dir
  } else {
    dir.canonicalize().unwrap_or(dir)
  }
}

// ---------------------------------------------------------------------------
// A compiled program's startup
// ---------------------------------------------------------------------------

/// What `denort` knows when it starts.
#[derive(Debug, Clone)]
pub struct CompiledStart<'a> {
  /// The mode baked by `deno compile --kernel-sandbox` (`None`: `auto`).
  pub baked: Option<KernelSandboxMode>,
  /// `deno compile --kernel-sandbox-exec=strict`.
  pub exec_strict: bool,
  /// A desktop development build.
  pub dev_build: bool,
  /// The grant the kernel holds the program to: the baked permissions,
  /// already intersected with the layers a confined ancestor handed down
  /// ([`kernel_inherit::clamp`]), plus the folder a self-extracting program
  /// extracted its files to. Never the embedded file system's virtual root
  /// (nothing on disk).
  pub options: &'a PermissionsOptions,
  /// Where this program may keep its own cache (the V8 code cache, its
  /// extracted native addons) while confined: `<cache_base>/<policy hash>`,
  /// created here, the user's alone, read and written by confined runs of
  /// this policy only. `None`: no cache.
  pub cache_base: Option<PathBuf>,
}

/// What [`start_compiled`] did.
#[derive(Debug, Clone)]
pub struct CompiledOutcome {
  pub resolution: ModeResolution,
  /// The kernel holds this process to its grant from here on.
  pub enforcing: bool,
  /// The program's own cache folder ([`CompiledStart::cache_base`]), when
  /// it enforces and the folder could be made.
  pub cache_dir: Option<PathBuf>,
  /// The plan that was applied.
  pub plan: Option<EnforcePlan>,
}

/// A compiled program that can't start under its baked mode.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CompiledStartError(pub String);

fn refuse(
  mode: KernelSandboxMode,
  why: impl std::fmt::Display,
) -> CompiledStartError {
  CompiledStartError(format!(
    "this program was compiled with --kernel-sandbox={mode} and refuses to start: {why}"
  ))
}

/// The context a compiled program's policy is compiled in.
fn compiled_context(
  mode: KernelSandboxMode,
  exec_strict: bool,
  cwd: PathBuf,
) -> Context {
  use std::io::IsTerminal;
  let mut ctx = Context::new(detect_capabilities(), mode, cwd);
  ctx.exe = std::env::current_exe()
    .ok()
    .map(|exe| exe.canonicalize().unwrap_or(exe));
  ctx.home =
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
      .filter(|h| !h.is_empty())
      .map(PathBuf::from);
  ctx.compiled = true;
  // Windows' small tier is built for compiled programs, which apply it in
  // place at startup; the CLI has no Windows enforcer (it has opened its
  // caches by the time it could apply one)
  if cfg!(windows) {
    ctx.caps.enforcer_built = true;
  }
  ctx.exec_strict = exec_strict;
  ctx.tty_parent = std::io::stdin().is_terminal();
  ctx.ttys = std_stream_ttys();
  ctx.tls_files = tls_files();
  ctx
}

fn compile_policy(
  options: &PermissionsOptions,
  ctx: &Context,
) -> CompiledPolicy {
  let implicit = ImplicitSet::build(options, ctx);
  KernelPolicy::compile(options, &implicit, ctx)
}

fn fatal_list<'a>(
  gaps: impl Iterator<Item = &'a crate::kernel_policy::PolicyGap>,
) -> Vec<String> {
  gaps
    .filter(|g| g.is_fatal_under_enforce())
    .map(|g| format!("  - {}: {}", g.rule, g.detail))
    .collect()
}

// ---------------------------------------------------------------------------
// A packaged desktop app's startup (DENEXT.md item 488)
// ---------------------------------------------------------------------------

/// The outside helper a confined desktop app keeps for what must happen
/// outside its sandbox (`deno_sandbox::outside`).
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use deno_sandbox::outside;

/// What a desktop app's startup knows beyond a compiled program's.
#[derive(Debug, Default)]
pub struct DesktopExtras<'a> {
  /// This process is a fork of the app with no engine (a worker, a
  /// sidecar): it is held by the app's sandbox when the app has one, and
  /// confines itself like a compiled program when the app is headless-only.
  pub worker: bool,
  /// The baked permissions, which decide the mode
  /// ([`CompiledStart::options`] is the kernel's grant, widened for the
  /// engine). `None`: the grant itself.
  pub baked: Option<&'a PermissionsOptions>,
  /// The folders whose programs the engine runs (Linux), and the install
  /// (`kernel_desktop::EnginePaths`).
  pub exec: Vec<PathBuf>,
  pub install: Option<PathBuf>,
  /// What the grant already leaves to Deno's checks
  /// (`kernel_desktop::DesktopGrant::residuals`).
  pub residuals: Vec<String>,
  /// The harnesses' later stage for `auto`
  /// (`kernel_sandbox::KERNEL_SANDBOX_STAGE_ENV_VAR`), which only raises it.
  pub stage: Option<crate::kernel_sandbox::AutoStage>,
  /// Why the app's own process can't be confined, when the caller knows a
  /// reason the engine table doesn't (an app without an identifier under a
  /// scoped grant): the app is then headless-only.
  pub blocked: Option<String>,
  /// What the outside helper may do; `None`: no helper.
  #[cfg(any(target_os = "linux", target_os = "macos"))]
  pub outside: Option<outside::Config>,
}

static DESKTOP_REPORT: std::sync::OnceLock<DesktopReport> =
  std::sync::OnceLock::new();

/// What a desktop app's startup decided, for the capability probe. `None`
/// outside a packaged desktop app (or before its startup ran).
pub fn desktop_report() -> Option<&'static DesktopReport> {
  DESKTOP_REPORT.get()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
static OUTSIDE: std::sync::OnceLock<outside::Outside> =
  std::sync::OnceLock::new();

/// The outside helper of this (confined) desktop app.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn desktop_outside() -> Option<&'static outside::Outside> {
  OUTSIDE.get()
}

/// Whether this process has exactly one thread (Linux: counted; elsewhere
/// not needed, and reported as it can be told).
fn single_threaded() -> bool {
  #[cfg(target_os = "linux")]
  {
    deno_sandbox::enforce_linux::single_threaded() == Some(true)
  }
  #[cfg(not(target_os = "linux"))]
  {
    false
  }
}

/// A desktop app's startup decision: the outcome, and the report the
/// capability probe gives.
pub type DesktopDecision = (CompiledOutcome, DesktopReport);

/// Builds a desktop app's decision.
fn desktop_outcome(
  mut resolution: ModeResolution,
  engine: Engine,
  sandbox: DesktopSandbox,
  reason: String,
  residuals: Vec<String>,
  plan: Option<EnforcePlan>,
) -> DesktopDecision {
  let enforcing = sandbox == DesktopSandbox::Kernel;
  if !enforcing && resolution.mode.fails_closed() {
    // the app's own process isn't confined: it runs held by Deno's checks
    // (a strict package says so; it does not refuse to start)
    resolution.mode = KernelSandboxMode::Off;
    resolution.reason = crate::kernel_sandbox::ModeReason::DesktopEngine;
    resolution.auto_chosen = false;
  }
  (
    CompiledOutcome {
      resolution,
      enforcing,
      cache_dir: None,
      plan,
    },
    DesktopReport {
      sandbox,
      reason,
      engine,
      residuals,
    },
  )
}

/// A packaged desktop app's kernel-sandbox startup: in place, before the
/// web engine starts and before any thread. `strict` is
/// `desktop.permissions: "strict"` (a baked `enforce`, which
/// [`CompiledStart::baked`] carries), `engine` the engine baked into the
/// package ([`Engine`]). See [`start_desktop_with`].
pub fn start_desktop(
  start: &CompiledStart,
  strict: bool,
  engine: Engine,
) -> Result<CompiledOutcome, CompiledStartError> {
  start_desktop_with(start, strict, engine, DesktopExtras::default())
}

/// A packaged desktop app's kernel-sandbox startup.
///
/// The app's host calls the runtime before it starts its web engine or any
/// thread, and the sandbox is applied there, in place, as a compiled
/// program's ([`start_compiled`]): no re-exec. What differs is the process:
/// it hosts a web engine, so
///
/// - whether it can be confined at all depends on the engine and the
///   machine (`kernel_desktop::engine_nests`). Where it can't (CEF on
///   macOS, CEF on a Linux machine with only Chromium's setuid helper, a
///   host that started the engine first on Linux), the app runs with its
///   own process unconfined and is reported `headless-only`: never a
///   refusal, also under a baked `enforce` (one line on stderr says so
///   then). A system without an enforcer for desktop apps reports `none`;
/// - macOS applies the desktop Seatbelt profile (allow by default, the
///   grant's denies, no LaunchServices opens, AppleEvents or job
///   submission), Linux the plan with the engine's needs
///   (`kernel_desktop::desktop_plan`) and, under Chromium, the namespaces
///   its sandbox makes, after a trial that those still work inside the
///   plan (`enforce_linux::trial_engine_namespaces`);
/// - the outside helper is forked right before the sandbox applies, for
///   opening links and the updater's swap.
///
/// Only a strict package (`desktop.permissions: "strict"`, a baked
/// `enforce`) is confined. A development build, `off`, `-A` and `auto` at
/// every stage (an app that isn't strict:
/// `kernel_sandbox::ModeReason::DesktopNotStrict`) apply nothing: such an
/// app starts the system's openers (`open`, `osascript`, `xdg-open`) for
/// `openPath`, `revealInFileManager` and the auth session's browser, and
/// only http / https / mailto links have a way out of the sandbox (the
/// outside helper). Under `enforce`, a rule the OS can't hold and a failed apply
/// refuse to start (a mode `auto` chose falls back instead), exactly as for
/// a compiled program. The decision is recorded for the capability probe
/// ([`desktop_report`]) and for `kernel_sandbox::check_compiled` (the
/// first one of a process).
pub fn start_desktop_with(
  start: &CompiledStart,
  strict: bool,
  engine: Engine,
  extras: DesktopExtras,
) -> Result<CompiledOutcome, CompiledStartError> {
  let (outcome, report) = decide_desktop(start, strict, engine, extras)?;
  let _ = DESKTOP_REPORT.set(report);
  crate::kernel_sandbox::set_desktop_resolution(outcome.resolution.clone());
  Ok(outcome)
}

/// [`start_desktop_with`] without the recording: decides, applies, and
/// returns the decision.
pub fn decide_desktop(
  start: &CompiledStart,
  strict: bool,
  engine: Engine,
  extras: DesktopExtras,
) -> Result<DesktopDecision, CompiledStartError> {
  // `strict` is the baked enforce: the resolution carries it
  let _ = strict;
  let resolution = desktop_resolution(
    start.baked,
    start.dev_build,
    extras.baked.unwrap_or(start.options),
    engine,
    extras.stage,
  );
  let mode = resolution.mode;
  let auto = resolution.auto_chosen;
  let mut residuals = extras.residuals.clone();
  match mode {
    KernelSandboxMode::Warn | KernelSandboxMode::Enforce => {}
    KernelSandboxMode::Broker => {
      return Err(refuse(
        mode,
        "the supervising broker can't hold a process that hosts a web engine",
      ));
    }
    KernelSandboxMode::Off | KernelSandboxMode::Auto => {
      let reason = resolution.reason.describe().to_string();
      return Ok(desktop_outcome(
        resolution,
        engine,
        DesktopSandbox::None,
        reason,
        Vec::new(),
        None,
      ));
    }
  }
  // inside a sandbox an ancestor applied (the app's own, for a fork of
  // it): that sandbox holds this process, and a second one isn't stacked
  // (macOS can't; the permissions are clamped to the ancestor's grant)
  if !kernel_inherit::inherited_layers().is_empty() {
    return Ok(desktop_outcome(
      resolution,
      engine,
      DesktopSandbox::Kernel,
      "held by the kernel sandbox of the process that started it".to_string(),
      residuals,
      None,
    ));
  }
  let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
  let mut ctx = compiled_context(mode, start.exec_strict, cwd);
  // a desktop app never forks a supervisor of its own (its process hosts
  // the web engine): what only the broker would hold stays a rule the OS
  // can't hold, never a Mode B plan nothing runs
  ctx.compiled_supervisor = false;
  // a desktop app has its own enforcers: none on Windows (the small tier
  // is a compiled program's)
  ctx.caps = detect_capabilities();
  let single = single_threaded();
  let facts = EngineFacts {
    os: ctx.caps.os,
    engine,
    enforcer_built: ctx.caps.enforcer_built,
    single_threaded: single,
    user_namespaces: ctx.caps.user_namespaces,
    worker: extras.worker,
  };
  let not_confined = |resolution: ModeResolution,
                      sandbox: DesktopSandbox,
                      reason: String| {
    if mode == KernelSandboxMode::Enforce && !auto {
      #[allow(clippy::print_stderr, reason = "the one line a strict app says")]
      {
        eprintln!(
          "Warning kernel sandbox (desktop, {}): this app's own process is not confined: {reason}. Deno's permission checks hold it.",
          sandbox.as_str()
        );
      }
    }
    desktop_outcome(resolution, engine, sandbox, reason, Vec::new(), None)
  };
  if let Err(not) = engine_nests(&facts) {
    return Ok(not_confined(resolution, not.sandbox, not.reason));
  }
  if !extras.worker
    && let Some(why) = &extras.blocked
  {
    return Ok(not_confined(
      resolution,
      DesktopSandbox::HeadlessOnly,
      why.clone(),
    ));
  }
  if extras.worker && cfg!(target_os = "linux") && !single {
    // a fork of a headless-only app, started by a host that doesn't call
    // the runtime first: it can't confine itself in place either
    return Ok(not_confined(
      resolution,
      DesktopSandbox::HeadlessOnly,
      "the app's host started threads before the runtime could confine this process".to_string(),
    ));
  }
  let mut compiled = compile_policy(start.options, &ctx);
  if mode == KernelSandboxMode::Warn {
    let deno_only = compiled
      .gaps
      .iter()
      .filter(|g| g.resolution == GapResolution::FailOpen)
      .count();
    #[allow(clippy::print_stderr, reason = "the warn mode's one line")]
    {
      eprintln!(
        "Warning kernel sandbox (warn, desktop): nothing is refused; {deno_only} rule(s) would be held by Deno's checks only."
      );
    }
    return Ok(desktop_outcome(
      resolution,
      engine,
      DesktopSandbox::None,
      "warn: nothing is refused".to_string(),
      Vec::new(),
      None,
    ));
  }
  // a mode `auto` chose never refuses to start: where enforce can't hold
  // the grant the app runs as it did, and the report says why
  let fall_back = |resolution: ModeResolution, why: String| {
    #[allow(clippy::print_stderr, reason = "the warn mode's one line")]
    {
      eprintln!(
        "Warning kernel sandbox (warn, auto): nothing is refused; enforce can't hold this app's grant here ({why}), so its rules are held by Deno's checks only."
      );
    }
    desktop_outcome(
      resolution,
      engine,
      DesktopSandbox::None,
      format!("enforce can't hold this app's grant here: {why}"),
      Vec::new(),
      None,
    )
  };
  if let Some(base) = &start.cache_base {
    let dir = base.join(compiled.hash());
    create_cache_overlay(&dir);
    if is_private_dir(base) && is_private_dir(&dir) {
      ctx.cache_overlay = Some(canonical_dir(dir));
      compiled = compile_policy(start.options, &ctx);
    }
  }
  let hint = "Package it with other permissions, or with --kernel-sandbox=warn to run with Deno's checks only.";
  let fatal = fatal_list(compiled.gaps.iter());
  let (plan, gaps) = if fatal.is_empty() {
    plan_for(&compiled, &ctx)
  } else {
    (None, Vec::new())
  };
  let fatal = if fatal.is_empty() {
    fatal_list(gaps.iter())
  } else {
    fatal
  };
  if !fatal.is_empty() {
    if auto {
      return Ok(fall_back(
        resolution,
        format!("{} rule(s) the OS can't hold", fatal.len()),
      ));
    }
    return Err(refuse(
      mode,
      format!(
        "the OS can't hold these rules here:\n{}\n{hint}",
        fatal.join("\n")
      ),
    ));
  }
  let Some(mut plan) = plan else {
    if auto {
      return Ok(fall_back(resolution, "no plan for this OS".to_string()));
    }
    return Err(refuse(mode, "no plan for this OS"));
  };
  if !extras.worker {
    crate::kernel_desktop::desktop_plan(
      &mut plan,
      &extras.exec,
      extras.install.as_deref(),
      // removing variables while other threads run is a data race: an app
      // confined after its engine started (macOS, a host that doesn't call
      // first) keeps its environment
      single,
    );
    if plan.env_keep.is_none() && compiled.policy.env_scrub.is_some() {
      residuals.push("environment: not scrubbed (the app's host started threads before the runtime could confine the process)".to_string());
    }
    residuals.extend(crate::kernel_desktop::standing_residuals(
      ctx.caps.os,
      engine,
    ));
  }
  let engine_namespaces =
    !extras.worker && engine == Engine::Cef && ctx.caps.os == TargetOs::Linux;
  #[cfg(target_os = "linux")]
  if engine_namespaces
    && let Err(why) = deno_sandbox::enforce_linux::trial_engine_namespaces(
      &plan,
      deno_sandbox::enforce_linux::DesktopVariant {
        engine_namespaces: true,
      },
    )
  {
    // never run web content with less than it has today: without the
    // kernel sandbox Chromium keeps its own
    return Ok(not_confined(resolution, DesktopSandbox::HeadlessOnly, why));
  }
  let mut layers = kernel_inherit::inherited_layers().to_vec();
  layers.push(kernel_inherit::layer_for(
    start.options,
    &ctx.cwd,
    plan.probe.clone(),
  ));
  if layers.len() > kernel_inherit::MAX_LAYERS {
    if auto {
      return Ok(fall_back(
        resolution,
        "too many nested kernel sandboxes".to_string(),
      ));
    }
    return Err(refuse(mode, "too many kernel sandboxes are nested here"));
  }
  let carried = kernel_inherit::Carried {
    format: kernel_inherit::CARRIER_FORMAT,
    layers,
  };
  // the outside helper keeps what the process can do now: forked last
  #[cfg(any(target_os = "linux", target_os = "macos"))]
  if !extras.worker
    && let Some(config) = extras.outside
  {
    match outside::start(config) {
      Ok(helper) => {
        let _ = OUTSIDE.set(helper);
      }
      Err(err) => residuals.push(format!(
        "outside helper: not started ({err}); links don't open in the browser and the updater can't swap the install from inside the sandbox"
      )),
    }
  }
  if let Err(why) = apply_desktop_in_place(
    &plan,
    &carried,
    extras.worker,
    engine_namespaces,
    single,
  ) {
    if auto {
      return Ok(fall_back(resolution, why));
    }
    return Err(refuse(mode, why));
  }
  kernel_sandbox_state::set_enforcing();
  // baked when the program was built: the first refusal names that switch
  kernel_sandbox_state::set_baked();
  deno_runtime::deno_core::error::set_permission_denied_note(
    kernel_inherit::permission_denied_note,
  );
  let reason = if extras.worker {
    "a fork of the app, confined in place like a compiled program"
  } else {
    "the app's process, its native code and its children are held to the baked grant"
  };
  let mut decision = desktop_outcome(
    resolution,
    engine,
    DesktopSandbox::Kernel,
    reason.to_string(),
    residuals,
    Some(plan),
  );
  decision.0.cache_dir = ctx.cache_overlay.clone();
  Ok(decision)
}

/// [`apply_in_place`] for a desktop app's process: the desktop Seatbelt
/// profile (a fork without an engine takes the cli profile), the Linux plan
/// with the engine's namespaces, and no change to the environment once
/// other threads exist (the carrier then reaches children through the
/// pinned child environment alone).
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn apply_desktop_in_place(
  plan: &EnforcePlan,
  carried: &kernel_inherit::Carried,
  worker: bool,
  engine_namespaces: bool,
  single_threaded: bool,
) -> Result<(), String> {
  use std::os::fd::IntoRawFd;

  use deno_sandbox::inherit::POLICY_ENV_VAR;
  #[cfg(target_os = "macos")]
  {
    match deno_sandbox::macos::is_sandboxed() {
      Some(false) => {}
      Some(true) => {
        return Err(
          "this process already runs inside another Seatbelt sandbox (an app sandbox, an agent's sandbox, sandbox-exec), and macOS doesn't stack a second profile"
            .to_string(),
        );
      }
      None => {
        return Err(
          "whether the process is sandboxed can't be measured here (no sandbox_check)"
            .to_string(),
        );
      }
    }
    if !deno_sandbox::macos::sandbox_init_available() {
      return Err("sandbox_init is not available here".to_string());
    }
  }
  let carrier =
    deno_sandbox::inherit::seal(&carried.to_bytes()).map_err(|err| {
      format!("the policy for child processes can't be handed down: {err}")
    })?;
  if single_threaded {
    scrub_env(plan);
  }
  #[cfg(target_os = "linux")]
  {
    let _ = worker;
    deno_sandbox::enforce_linux::apply_self_variant(
      plan,
      deno_sandbox::enforce_linux::DesktopVariant { engine_namespaces },
    )?;
  }
  #[cfg(target_os = "macos")]
  {
    let _ = engine_namespaces;
    let Some(macos) = &plan.macos else {
      return Err("the plan has no macOS part".to_string());
    };
    let profile = if worker {
      deno_sandbox::seatbelt::cli_profile(macos)
    } else {
      deno_sandbox::seatbelt::desktop_profile(macos)
    };
    deno_sandbox::macos::apply_profile(&profile)?;
    if deno_sandbox::macos::is_sandboxed() != Some(true) {
      return Err("the profile was applied but doesn't show".to_string());
    }
    if let Some(probe) = &plan.probe
      && deno_sandbox::macos::refuses_read(probe) != Some(true)
    {
      return Err(format!(
        "{} is still readable with the profile applied: it doesn't hold here",
        probe.display()
      ));
    }
  }
  // kept open for the life of the process: the descriptor children inherit
  let fd = carrier.into_raw_fd();
  if single_threaded {
    // SAFETY: one thread (the host called before it started any)
    unsafe { std::env::set_var(POLICY_ENV_VAR, fd.to_string()) };
  }
  deno_runtime::deno_process::repin_child_env_var(
    POLICY_ENV_VAR.into(),
    fd.to_string().into(),
  );
  deno_resolver::npmrc::set_kernel_confined();
  Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn apply_desktop_in_place(
  _plan: &EnforcePlan,
  _carried: &kernel_inherit::Carried,
  _worker: bool,
  _engine_namespaces: bool,
  _single_threaded: bool,
) -> Result<(), String> {
  Err("no kernel enforcer for desktop apps on this OS".to_string())
}

/// A compiled program's kernel-sandbox startup, first thing in `main`
/// (one thread, nothing opened): decides the baked mode, and under
/// `enforce` compiles the policy for the baked grant and applies it to this
/// process, in place. `warn` prints its line; an OS without an enforcer and
/// a rule the OS can't hold refuse to start (a mode `auto` chose falls back
/// instead: off with no enforcer, else `warn`'s line, item 472). `broker`,
/// and `enforce` or `auto` where the grant has a rule only the broker
/// holds, fork first: **this function then returns in the child**, the
/// confined worker, and the process that called it stays behind as the
/// supervisor and never returns (it exits as the worker exits).
/// Whether the permission set allows prompting doesn't matter: a
/// compiled program's set is its author's, and once the sandbox applies the
/// caller refuses every prompt (a prompt could only grant what the kernel
/// refuses; on a terminal the first refusal says so once, review D4). A
/// program baked with `-A` and denies under `auto` gets the deny ceiling
/// (D2): only the denies, nothing else changed, and nothing applied where
/// the OS holds none of them.
pub fn start_compiled(
  start: &CompiledStart,
) -> Result<CompiledOutcome, CompiledStartError> {
  let mut resolution =
    compiled_resolution(start.baked, start.dev_build, start.options);
  let mode = resolution.mode;
  match mode {
    KernelSandboxMode::Warn
    | KernelSandboxMode::Enforce
    | KernelSandboxMode::Broker => {}
    KernelSandboxMode::Off | KernelSandboxMode::Auto => {
      return Ok(CompiledOutcome {
        resolution,
        enforcing: false,
        cache_dir: None,
        plan: None,
      });
    }
  }
  let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
  let mut ctx = compiled_context(mode, start.exec_strict, cwd);
  // a headless compiled program forks its own supervisor where a rule
  // needs the broker (never a desktop app: `start_desktop` leaves this
  // off, so what only the broker holds stays a rule the OS can't hold
  // there)
  ctx.compiled_supervisor = cfg!(any(target_os = "linux", target_os = "macos"));
  if mode == KernelSandboxMode::Broker && !ctx.compiled_supervisor {
    return Err(refuse(
      mode,
      "a compiled program has no supervising broker on this OS",
    ));
  }
  // a deny ceiling (D2): the denies alone, every allow unscoped. It never
  // takes the supervisor (the policy compiler keeps a ceiling in Mode A
  // and reports what that can't hold), so the second decision below
  // doesn't change it
  let ceiling = resolution.deny_ceiling();
  ctx.deny_ceiling = ceiling;
  let ceiling_options;
  let options = if ceiling {
    ceiling_options = crate::kernel_policy::deny_ceiling_options(start.options);
    &ceiling_options
  } else {
    start.options
  };
  let mut compiled = compile_policy(options, &ctx);
  // the policy has the broker's shape: `auto` takes the supervisor where it
  // would have enforced (the mode is decided again, now that the policy is
  // known), and an `enforce` baked by name takes it by itself
  if compiled.policy.shape == Shape::ModeB && mode != KernelSandboxMode::Warn {
    resolution = compiled_resolution_with(
      start.baked,
      start.dev_build,
      start.options,
      true,
    );
  }
  let outcome = |enforcing, cache_dir, plan| CompiledOutcome {
    resolution: resolution.clone(),
    enforcing,
    cache_dir,
    plan,
  };
  if ceiling && !compiled.policy.deny_ceiling_held() {
    // the OS holds none of its denies here: nothing applies, as before
    return Ok(outcome(false, None, None));
  }
  // a mode `auto` chose never refuses to start (item 472): where enforce
  // can't hold the grant the program runs as a warn one, and says why
  let auto = resolution.auto_chosen;
  let fall_back = |why: String| {
    #[allow(clippy::print_stderr, reason = "the warn mode's one line")]
    {
      eprintln!(
        "Warning kernel sandbox (warn, auto): nothing is refused; enforce can't hold this program's grant here ({why}), so its rules are held by Deno's checks only."
      );
    }
    outcome(false, None, None)
  };
  if auto
    && (!kernel_inherit::inherited_layers().is_empty()
      || kernel_inherit::grants_unknown())
  {
    // as the CLI: a process an ancestor's kernel sandbox already holds adds
    // nothing of its own when nobody asked it to by name. Its grant is
    // intersected with the inherited one, and inside a brokered run it
    // takes the way to that run's broker (the caller does both).
    return Ok(outcome(false, None, None));
  }
  if mode == KernelSandboxMode::Warn {
    let deno_only = compiled
      .gaps
      .iter()
      .filter(|g| g.resolution == GapResolution::FailOpen)
      .count();
    #[allow(clippy::print_stderr, reason = "the warn mode's one line")]
    {
      eprintln!(
        "Warning kernel sandbox (warn, preview): nothing is refused; {deno_only} rule(s) would be held by Deno's checks only."
      );
    }
    return Ok(outcome(false, None, None));
  }
  if !ctx.caps.enforcer_built {
    if auto {
      // as the CLI: with no enforcer for this OS the program runs as it did
      return Ok(outcome(false, None, None));
    }
    return Err(refuse(
      mode,
      "this runtime has no kernel enforcer for this OS and architecture",
    ));
  }
  // the program's own cache folder, keyed by the policy compiled without
  // it (as the CLI's cache overlay, item 451): used only when it is this
  // user's alone, so both the policy and the caches agree on it
  if let Some(base) = &start.cache_base {
    let dir = base.join(compiled.hash());
    create_cache_overlay(&dir);
    if is_private_dir(base) && is_private_dir(&dir) {
      ctx.cache_overlay = Some(canonical_dir(dir));
      compiled = compile_policy(options, &ctx);
    }
  }
  let fatal = fatal_list(compiled.gaps.iter());
  if !fatal.is_empty() {
    if auto {
      return Ok(fall_back(format!(
        "{} rule(s) the OS can't hold",
        fatal.len()
      )));
    }
    return Err(refuse(
      mode,
      format!(
        "the OS can't hold these rules here:\n{}\nCompile it with other permissions, or with --kernel-sandbox=warn to run with Deno's checks only.",
        fatal.join("\n")
      ),
    ));
  }
  let (plan, gaps) = plan_for(&compiled, &ctx);
  let fatal = fatal_list(gaps.iter());
  if !fatal.is_empty() {
    if auto {
      return Ok(fall_back(format!(
        "{} rule(s) the OS can't hold",
        fatal.len()
      )));
    }
    return Err(refuse(
      mode,
      format!(
        "the OS can't hold these rules here:\n{}\nCompile it with other permissions, or with --kernel-sandbox=warn to run with Deno's checks only.",
        fatal.join("\n")
      ),
    ));
  }
  let Some(mut plan) = plan else {
    if auto {
      return Ok(fall_back("no plan for this OS".to_string()));
    }
    return Err(refuse(mode, "no plan for this OS"));
  };
  #[cfg(any(target_os = "linux", target_os = "macos"))]
  crate::kernel_supervise::fill_broker_net(
    &mut plan,
    &compiled,
    options,
    ctx.inspector_port,
  );
  let mut layers = kernel_inherit::inherited_layers().to_vec();
  layers.push(kernel_inherit::layer_for(
    options,
    &ctx.cwd,
    plan.probe.clone(),
  ));
  if layers.len() > kernel_inherit::MAX_LAYERS {
    if auto {
      return Ok(fall_back("too many nested kernel sandboxes".to_string()));
    }
    return Err(refuse(
      mode,
      format!(
        "{} kernel sandboxes are already nested here",
        layers.len() - 1
      ),
    ));
  }
  let carried = kernel_inherit::Carried {
    format: kernel_inherit::CARRIER_FORMAT,
    layers,
  };
  // Mode B: the supervisor first. From here on this is the worker (the
  // process that called is the supervisor and never comes back); a fork
  // that can't be made leaves the one process as it was
  #[cfg(any(target_os = "linux", target_os = "macos"))]
  let channel = if plan.broker.is_some() && can_apply_in_place() {
    match crate::kernel_supervise::fork_supervised(&plan) {
      Ok(channel) => Some(channel),
      Err(why) if auto => {
        return Ok(fall_back(format!(
          "the supervising broker can't start: {why}"
        )));
      }
      Err(why) => {
        return Err(refuse(
          mode,
          format!("the supervising broker can't start: {why}"),
        ));
      }
    }
  } else {
    None
  };
  // a dial this process inherited belongs to an ancestor's broker, which
  // answers for that ancestor's grant: a process that confines itself
  // gives it up first
  #[cfg(any(target_os = "linux", target_os = "macos"))]
  crate::kernel_supervise::drop_inherited_dial();
  apply_in_place(&plan, &carried).map_err(|why| refuse(mode, why))?;
  #[cfg(any(target_os = "linux", target_os = "macos"))]
  if let Some(channel) = channel {
    let net = plan.broker.as_ref().is_some_and(|b| b.net.is_some());
    crate::kernel_supervise::install_channel(channel, net);
    crate::kernel_supervise::check_broker(&plan)
      .map_err(|why| refuse(mode, why))?;
    // the `deno` / `denort` processes this program starts reach the same
    // broker
    crate::kernel_supervise::offer_dial();
  }
  kernel_sandbox_state::set_enforcing();
  // baked when the program was built: the first refusal names that switch
  kernel_sandbox_state::set_baked();
  deno_runtime::deno_core::error::set_permission_denied_note(
    kernel_inherit::permission_denied_note,
  );
  Ok(outcome(true, ctx.cache_overlay.clone(), Some(plan)))
}

/// Whether [`apply_in_place`] can apply a plan to this process at all, as
/// far as that is known before trying: asked before the supervisor is
/// forked, so a program that can't confine itself (macOS: it already runs
/// inside another Seatbelt sandbox) refuses as one process, with
/// [`apply_in_place`]'s own message, and leaves no supervisor behind.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn can_apply_in_place() -> bool {
  #[cfg(target_os = "macos")]
  {
    deno_sandbox::macos::is_sandboxed() == Some(false)
      && deno_sandbox::macos::sandbox_init_available()
  }
  #[cfg(target_os = "linux")]
  {
    true
  }
}

/// Removes every variable the plan doesn't keep from this process's
/// environment (the CLI hands the re-executed image the kept ones only).
/// Called while the process has one thread.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn scrub_env(plan: &EnforcePlan) {
  if plan.env_keep.is_none() {
    return;
  }
  let drop: Vec<std::ffi::OsString> = std::env::vars_os()
    .map(|(k, _)| k)
    .filter(|k| !k.to_str().is_some_and(|k| plan.keeps_env(k)))
    .collect();
  for name in drop {
    // SAFETY: one thread (the top of `main`); nothing reads the
    // environment concurrently
    unsafe { std::env::remove_var(name) };
  }
}

/// Applies `plan` to this process, in place, and hands `carried` (the
/// layers its children inherit) down: the carrier is sealed before the
/// sandbox applies (macOS writes it through the temp folder), named in the
/// environment children inherit, and pinned for children started with
/// another environment.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn apply_in_place(
  plan: &EnforcePlan,
  carried: &kernel_inherit::Carried,
) -> Result<(), String> {
  use std::os::fd::IntoRawFd;

  use deno_sandbox::inherit::POLICY_ENV_VAR;
  #[cfg(target_os = "macos")]
  {
    match deno_sandbox::macos::is_sandboxed() {
      Some(false) => {}
      Some(true) => {
        return Err(
          "this process already runs inside another Seatbelt sandbox (an app sandbox, an agent's sandbox, sandbox-exec, a confined parent), and macOS doesn't stack a second profile"
            .to_string(),
        );
      }
      None => {
        return Err(
          "whether the process is sandboxed can't be measured here (no sandbox_check)"
            .to_string(),
        );
      }
    }
    if !deno_sandbox::macos::sandbox_init_available() {
      return Err("sandbox_init is not available here".to_string());
    }
  }
  let carrier =
    deno_sandbox::inherit::seal(&carried.to_bytes()).map_err(|err| {
      format!("the policy for child processes can't be handed down: {err}")
    })?;
  scrub_env(plan);
  #[cfg(target_os = "linux")]
  deno_sandbox::enforce_linux::apply_self(plan)?;
  #[cfg(target_os = "macos")]
  {
    let Some(macos) = &plan.macos else {
      return Err("the plan has no macOS part".to_string());
    };
    let profile = deno_sandbox::seatbelt::cli_profile(macos);
    deno_sandbox::macos::apply_profile(&profile)?;
    if deno_sandbox::macos::is_sandboxed() != Some(true) {
      return Err("the profile was applied but doesn't show".to_string());
    }
    if let Some(probe) = &plan.probe
      && deno_sandbox::macos::refuses_read(probe) != Some(true)
    {
      return Err(format!(
        "{} is still readable with the profile applied: it doesn't hold here",
        probe.display()
      ));
    }
  }
  // kept open for the life of the process: the descriptor children inherit
  let fd = carrier.into_raw_fd();
  // SAFETY: one thread (the top of `main`)
  unsafe { std::env::set_var(POLICY_ENV_VAR, fd.to_string()) };
  deno_runtime::deno_process::repin_child_env_var(
    POLICY_ENV_VAR.into(),
    fd.to_string().into(),
  );
  deno_resolver::npmrc::set_kernel_confined();
  Ok(())
}

/// Windows, the small tier: the job object and the lowered integrity
/// level, in place. No carrier is handed down: Windows has no inherited
/// policy for nested `deno` processes yet (they run with their own grant,
/// inside the job and at the lowered level).
#[cfg(windows)]
fn apply_in_place(
  plan: &EnforcePlan,
  _carried: &kernel_inherit::Carried,
) -> Result<(), String> {
  let Some(windows) = &plan.windows else {
    return Err("the plan has no Windows part".to_string());
  };
  deno_sandbox::windows::apply(windows).map(|_| ())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn apply_in_place(
  _plan: &EnforcePlan,
  _carried: &kernel_inherit::Carried,
) -> Result<(), String> {
  Err("no kernel enforcer for this OS".to_string())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
      .join(format!("deno_kernel_apply_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
  }

  fn options(read: &[&str]) -> PermissionsOptions {
    PermissionsOptions {
      allow_read: Some(read.iter().map(|s| s.to_string()).collect()),
      prompt: false,
      ..Default::default()
    }
  }

  #[test]
  fn off_and_auto_apply_nothing() {
    let options = options(&["/tmp"]);
    for baked in [
      None,
      Some(KernelSandboxMode::Off),
      Some(KernelSandboxMode::Auto),
    ] {
      let outcome = start_compiled(&CompiledStart {
        baked,
        exec_strict: false,
        dev_build: false,
        options: &options,
        cache_base: None,
      })
      .unwrap();
      assert!(!outcome.enforcing, "{baked:?}");
      assert_eq!(outcome.resolution.mode, KernelSandboxMode::Off);
      assert!(outcome.plan.is_none() && outcome.cache_dir.is_none());
    }
  }

  #[test]
  fn a_dev_build_is_off() {
    let options = options(&["/tmp"]);
    // a desktop development build never enforces (hot reload rewrites it),
    // whatever was baked
    for baked in [KernelSandboxMode::Enforce, KernelSandboxMode::Broker] {
      let outcome = start_compiled(&CompiledStart {
        baked: Some(baked),
        exec_strict: false,
        dev_build: true,
        options: &options,
        cache_base: None,
      })
      .unwrap();
      assert!(!outcome.enforcing, "{baked}");
    }
  }

  /// A compiled program's policy takes the broker's shape only where the
  /// program can fork a supervisor of its own.
  #[test]
  fn a_compiled_policy_takes_the_broker_where_it_has_a_supervisor() {
    let options = PermissionsOptions {
      allow_net: Some(vec!["example.com:443".to_string()]),
      prompt: false,
      ..Default::default()
    };
    let mut caps = Capabilities::none(TargetOs::Linux);
    caps.landlock_abi = Some(6);
    caps.seccomp = true;
    caps.enforcer_built = true;
    let mut ctx = Context::new(caps, KernelSandboxMode::Enforce, "/".into());
    ctx.compiled = true;
    // no supervisor (a desktop app): the host-scoped grant is a rule the OS
    // can't hold, and the policy stays Mode A
    let alone = compile_policy(&options, &ctx);
    assert_eq!(alone.policy.shape, Shape::ModeA);
    assert!(alone.fatal_gaps().count() > 0);
    // with one: Mode B, nothing left that can't be held
    ctx.compiled_supervisor = true;
    let supervised = compile_policy(&options, &ctx);
    assert_eq!(supervised.policy.shape, Shape::ModeB);
    assert_eq!(supervised.fatal_gaps().count(), 0, "{:?}", supervised.gaps);
  }

  // DENEXT.md item 488: a desktop app whose mode applies nothing reports
  // `none` with the reason, and a development build is off whatever is
  // baked
  #[test]
  fn a_desktop_app_that_applies_nothing_reports_none() {
    let options = options(&["/tmp"]);
    let decide = |baked, dev_build, engine| {
      decide_desktop(
        &CompiledStart {
          baked,
          exec_strict: false,
          dev_build,
          options: &options,
          cache_base: None,
        },
        baked == Some(KernelSandboxMode::Enforce),
        engine,
        DesktopExtras::default(),
      )
    };
    for (baked, dev_build, why) in [
      // not strict: `auto` applies nothing, at any stage
      (None, false, "isn't packaged strict"),
      (Some(KernelSandboxMode::Off), false, "turned off"),
      (
        Some(KernelSandboxMode::Auto),
        false,
        "isn't packaged strict",
      ),
      (
        Some(KernelSandboxMode::Enforce),
        true,
        "a desktop development build",
      ),
    ] {
      for engine in [Engine::Webview, Engine::Cef] {
        let (outcome, report) = decide(baked, dev_build, engine).unwrap();
        assert!(!outcome.enforcing, "{baked:?} {engine:?}");
        assert_eq!(outcome.resolution.mode, KernelSandboxMode::Off);
        assert!(outcome.plan.is_none());
        assert_eq!(report.sandbox, DesktopSandbox::None);
        assert_eq!(report.engine, engine);
        assert!(report.reason.contains(why), "{}", report.reason);
      }
    }
    // a baked broker can't hold a process with a web engine in it
    let err = decide(Some(KernelSandboxMode::Broker), false, Engine::Webview)
      .unwrap_err();
    assert!(err.0.contains("web engine"), "{err}");
  }

  // CEF on macOS is headless-only under a baked enforce: the app starts,
  // unconfined, and says why (nothing is applied, so this runs anywhere a
  // Mac builds it)
  #[cfg(target_os = "macos")]
  #[test]
  fn macos_cef_is_headless_only_and_starts() {
    let options = options(&["/tmp"]);
    let (outcome, report) = decide_desktop(
      &CompiledStart {
        baked: Some(KernelSandboxMode::Enforce),
        exec_strict: false,
        dev_build: false,
        options: &options,
        cache_base: None,
      },
      true,
      Engine::Cef,
      DesktopExtras::default(),
    )
    .unwrap();
    assert!(!outcome.enforcing);
    assert_eq!(report.sandbox, DesktopSandbox::HeadlessOnly);
    assert!(report.reason.contains("CEF on macOS"), "{}", report.reason);
    // the compiled-program guard must not refuse it: the mode it sees is
    // off, for the engine
    assert_eq!(outcome.resolution.mode, KernelSandboxMode::Off);
    assert_eq!(
      outcome.resolution.reason,
      crate::kernel_sandbox::ModeReason::DesktopEngine
    );
  }

  #[cfg(unix)]
  #[test]
  fn a_cache_folder_must_be_this_users_alone() {
    use std::os::unix::fs::PermissionsExt;
    let root = scratch("private");
    let dir = root.join("base").join("hash");
    create_cache_overlay(&dir);
    assert!(is_private_dir(&root.join("base")) && is_private_dir(&dir));
    // open to others: not private
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))
      .unwrap();
    assert!(!is_private_dir(&dir));
    // a symlink to a private folder isn't one
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
      .unwrap();
    std::os::unix::fs::symlink(&dir, root.join("link")).unwrap();
    assert!(!is_private_dir(&root.join("link")));
    assert!(!is_private_dir(&root.join("missing")));
    std::fs::remove_dir_all(&root).unwrap();
  }
}
