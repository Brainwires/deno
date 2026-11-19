// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: applying the compiled kernel policy (`--kernel-sandbox=enforce`,
//! Mode A; DENEXT.md item 425).
//!
//! - `deno_lib::kernel_plan::plan_for` (shared with `denort`) translates the
//!   policy model's [`CompiledPolicy`] into the enforcers' [`EnforcePlan`].
//! - [`prepare`] (from `check_run`) builds the plan and records it as
//!   pending, or accepts the plan this process already runs under.
//! - [`apply_pending`] runs once the module graph is loaded, before any user
//!   code: the process re-executes itself with the plan (Linux: in a sealed
//!   memfd; macOS: in an unlinked file), and the new image applies it first
//!   thing in `main` ([`startup`]: Landlock and seccomp; `sandbox_init`),
//!   before it opens anything; where `sandbox_init` is missing, macOS
//!   re-executes under `sandbox-exec` instead.
//! - [`guard_user_code`] refuses to create a main worker while a plan is
//!   pending (a code path that runs user code without the graph-first apply
//!   fails closed).
//! - [`permission_denied_note`]: the note on a permission error the kernel
//!   sandbox probably caused.

#![allow(
  clippy::disallowed_methods,
  reason = "the sandbox acts on the real file system and environment, never a virtual one"
)]
#![cfg_attr(
  not(any(target_os = "linux", target_os = "macos")),
  allow(unused_imports, dead_code, reason = "no Mode A enforcer on this OS")
)]

use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

use deno_core::ModuleSpecifier;
use deno_core::anyhow::bail;
use deno_core::error::AnyError;
use deno_lib::kernel_plan::plan_for;
pub use deno_lib::kernel_plan::report_gaps;
#[cfg(target_os = "macos")]
use deno_lib::kernel_plan::uses_sandbox_exec_fallback;
use deno_runtime::deno_permissions::PermissionsOptions;
use deno_runtime::deno_permissions::kernel_sandbox_state;
use deno_sandbox::plan::EnforcePlan;

use super::inherit;
use super::policy::CompiledPolicy;
use super::policy::Context;
use super::policy::resolve_prefix;

// ---------------------------------------------------------------------------
// The startup hand-over, the pending plan, applying it
// ---------------------------------------------------------------------------

/// The plan waiting for [`apply_pending`].
static PENDING: OnceLock<EnforcePlan> = OnceLock::new();

/// What the processes this run starts inherit once it is confined: the
/// layers it inherited and its own ([`inherit`]); handed to the re-executed
/// image in a carrier descriptor with the plan.
static PENDING_CARRIED: OnceLock<inherit::Carried> = OnceLock::new();

/// The carried layers for a run with grant `options` and kernel plan
/// `plan`: every layer this process inherited, then its own.
fn carried_for(
  options: &PermissionsOptions,
  cwd: &Path,
  plan: &EnforcePlan,
) -> Result<inherit::Carried, AnyError> {
  let mut layers = inherit::inherited_layers().to_vec();
  layers.push(inherit::layer_for(options, cwd, plan.probe.clone()));
  if layers.len() > inherit::MAX_LAYERS {
    bail!(
      "--kernel-sandbox=enforce refuses to start: {} kernel sandboxes are already nested here",
      layers.len() - 1
    );
  }
  Ok(inherit::Carried {
    format: inherit::CARRIER_FORMAT,
    layers,
  })
}

/// The image after the re-exec: the carrier it inherited must end with this
/// run's own layer (the image before it made it from the same grant), so
/// the processes it starts are told exactly the grant it runs under.
fn check_handed_down(
  options: &PermissionsOptions,
  cwd: &Path,
  plan: &EnforcePlan,
) -> Result<(), AnyError> {
  let own = inherit::layer_for(options, cwd, plan.probe.clone());
  if inherit::inherited_layers().last() != Some(&own) {
    bail!(
      "--kernel-sandbox=enforce refuses to start: the policy handed down to child processes isn't the one this run compiles"
    );
  }
  Ok(())
}

/// The module files of the plan this process started under: each must be a
/// local module of this run's own graph ([`apply_pending`] checks once the
/// graph is loaded).
static VERIFY_MODULES: OnceLock<Vec<PathBuf>> = OnceLock::new();

/// The policy's read denies, resolved: no module file the kernel is told to
/// let the program read may lie inside one.
static READ_DENIES: OnceLock<Vec<PathBuf>> = OnceLock::new();

/// Refuses module files that lie inside a read deny (the module rules are
/// added beside the read scope, so a deny there would not hold).
fn check_module_files(files: &[PathBuf]) -> Result<(), AnyError> {
  let Some(denies) = READ_DENIES.get() else {
    return Ok(());
  };
  for file in files {
    let resolved = resolve_prefix(file).full();
    if denies
      .iter()
      .any(|d| file.starts_with(d) || resolved.starts_with(d))
    {
      bail!(
        "--kernel-sandbox=enforce refuses to start: the module {} lies inside a --deny-read path",
        file.display()
      );
    }
  }
  Ok(())
}

/// macOS: the plan handed over through the `sandbox-exec` fallback.
#[cfg(target_os = "macos")]
static INHERITED: OnceLock<EnforcePlan> = OnceLock::new();

/// Called first thing in `main`, while the process has one thread: a plan
/// handed over by the image before this one is applied (Linux) or recorded
/// (macOS, already applied by `sandbox-exec`). A variable without a valid
/// plan, or a plan that can't be applied, ends the process: the hand-over
/// never fails open.
pub fn startup() {
  #[cfg(target_os = "linux")]
  {
    let result =
      deno_sandbox::enforce_linux::take_inherited_plan().and_then(|plan| {
        match plan {
          Some(plan) => deno_sandbox::enforce_linux::apply_self(&plan),
          None => Ok(()),
        }
      });
    if let Err(err) = result {
      refuse_at_startup(&err);
    }
    if let Some(plan) = deno_sandbox::enforce_linux::applied_plan() {
      use_cache_overlay(plan);
    }
    if deno_sandbox::enforce_linux::applied_plan().is_some()
      || kernel_reports_confined()
    {
      mark_confined();
    }
    // the policy handed down by a confined ancestor (or, in the image after
    // the re-exec, by the image before it), measured now the plan applies
    inherit::load_at_startup();
  }
  #[cfg(target_os = "macos")]
  {
    match deno_sandbox::macos::take_inherited_plan() {
      Ok(Some(plan)) => {
        if let Err(err) = apply_inherited_macos(&plan) {
          refuse_at_startup(&err);
        }
        use_cache_overlay(&plan);
        let _ = INHERITED.set(plan);
        mark_confined();
      }
      Ok(None) => {
        if kernel_reports_confined() {
          mark_confined();
        }
      }
      Err(err) => refuse_at_startup(&err),
    }
    inherit::load_at_startup();
  }
}

/// The image after the re-exec, confined by `plan`: its caches move to the
/// plan's cache overlay (item 451). Only an applied plan sets it, never the
/// environment, so an unconfined run never reads an overlay; one without an
/// overlay's shape refuses (the hand-over never widens).
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn use_cache_overlay(plan: &EnforcePlan) {
  if let Some(overlay) = &plan.cache_overlay
    && !deno_resolver::cache::set_sandbox_overlay(overlay.clone())
  {
    refuse_at_startup(&format!("{} is not a cache overlay", overlay.display()));
  }
}

/// Creates the cache overlay folder, the user's alone (0700), in the image
/// that isn't confined yet. A failure leaves it absent: the policy then has
/// no overlay and the caches stay in memory.
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

/// Creates the folders the plan writes on the user's behalf (the coverage
/// and profile folders) before the re-exec: a Landlock rule needs its path to
/// exist.
fn create_runtime_dirs(ctx: &Context) -> Result<(), AnyError> {
  for dir in ctx.coverage_dir.iter().chain(&ctx.profile_dirs) {
    std::fs::create_dir_all(dir).map_err(|err| {
      deno_core::anyhow::anyhow!(
        "--kernel-sandbox=enforce: creating the output folder {} failed: {err}",
        dir.display()
      )
    })?;
  }
  Ok(())
}

/// The Seatbelt profile for a plan: its macOS part, with the program's
/// module files readable.
#[cfg(target_os = "macos")]
fn macos_profile(plan: &EnforcePlan) -> Result<String, String> {
  let Some(macos) = &plan.macos else {
    return Err("the plan has no macOS part".to_string());
  };
  let mut macos = macos.clone();
  if let deno_sandbox::plan::SbScope::Filters { filters } = &mut macos.read {
    for file in &plan.module_files {
      let file = resolve_prefix(file).full();
      if let Some(text) = file.to_str() {
        filters.push(deno_sandbox::plan::SbFilter::Literal(text.to_string()));
      }
    }
  }
  Ok(deno_sandbox::seatbelt::cli_profile(&macos))
}

/// macOS, the image after the re-exec, first thing in `main` (before it
/// opens a cache or any other file): applies the handed-over plan with
/// `sandbox_init`, unless the `sandbox-exec` fallback already applied it,
/// and measures that it holds. Anything that can't be measured refuses.
#[cfg(target_os = "macos")]
fn apply_inherited_macos(plan: &EnforcePlan) -> Result<(), String> {
  match deno_sandbox::macos::is_sandboxed() {
    // the sandbox-exec fallback: applied by the exec (prepare compares the
    // plan with its own and measures again)
    Some(true) => return Ok(()),
    Some(false) => {}
    None => {
      return Err(
        "whether the process is sandboxed can't be measured here (no sandbox_check)"
          .to_string(),
      );
    }
  }
  let profile = macos_profile(plan)?;
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
  Ok(())
}

/// Tells the config loaders this process runs confined: an unreadable
/// `.npmrc` then counts as absent, as the policy never makes it readable
/// (`deno_resolver::npmrc::set_kernel_confined`). Decided from the plan this
/// process applied or the kernel's own report, never from a variable a
/// parent could set; nothing in the environment says so either.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn mark_confined() {
  deno_resolver::npmrc::set_kernel_confined();
}

/// Whether the kernel reports this process as confined without a plan of
/// its own: a child of a confined process (Linux: no-new-privileges and a
/// seccomp filter, which a sandboxed parent hands every child; macOS: a
/// Seatbelt sandbox).
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn kernel_reports_confined() -> bool {
  #[cfg(target_os = "linux")]
  {
    deno_sandbox::enforce_linux::status_confined()
  }
  #[cfg(target_os = "macos")]
  {
    deno_sandbox::macos::is_sandboxed() == Some(true)
  }
}

fn refuse_at_startup(err: &str) -> ! {
  #[allow(clippy::print_stderr, reason = "nothing else is set up yet")]
  {
    eprintln!(
      "error: --kernel-sandbox=enforce: the sandbox handed over from the starting process can't be applied: {err}"
    );
  }
  std::process::exit(1);
}

/// Whether this process started confined by a handed-over plan (Linux: the
/// re-executed image): remote modules are then read from the cache only.
pub fn confined_at_startup() -> bool {
  #[cfg(target_os = "linux")]
  {
    deno_sandbox::enforce_linux::applied_plan().is_some()
  }
  #[cfg(target_os = "macos")]
  {
    INHERITED.get().is_some()
  }
  #[cfg(not(any(target_os = "linux", target_os = "macos")))]
  {
    false
  }
}

/// Whether the kernel reports this process confined by a sandbox it didn't
/// apply itself (a child of a confined process).
pub fn confined_by_a_parent() -> bool {
  #[cfg(any(target_os = "linux", target_os = "macos"))]
  {
    !confined_at_startup() && kernel_reports_confined()
  }
  #[cfg(not(any(target_os = "linux", target_os = "macos")))]
  {
    false
  }
}

fn start_enforcing() {
  kernel_sandbox_state::set_enforcing();
  deno_core::error::set_permission_denied_note(permission_denied_note);
}

/// Builds the plan for a run whose mode is `enforce` and whose policy has
/// no fatal gap, then: accepts the plan this process already runs under
/// (equal plan, measured), or records it for [`apply_pending`]. Errors
/// mean refusing to start.
pub fn prepare(
  compiled: &CompiledPolicy,
  ctx: &Context,
  options: &PermissionsOptions,
) -> Result<(), AnyError> {
  // before the plan is built (it names only folders that exist), and only
  // in the image that isn't confined yet (the next one finds them)
  if !confined_at_startup() {
    create_runtime_dirs(ctx)?;
  }
  let (plan, gaps) = plan_for(compiled, ctx);
  let fatal: Vec<String> = gaps
    .iter()
    .filter(|g| g.is_fatal_under_enforce())
    .map(|g| format!("  - {}: {}", g.rule, g.detail))
    .collect();
  if !fatal.is_empty() {
    bail!(
      "--kernel-sandbox=enforce refuses to start: the OS can't hold these rules here:\n{}\nUse --kernel-sandbox=warn to run with Deno's checks only, or --kernel-sandbox-report for the whole policy.",
      fatal.join("\n")
    );
  }
  let Some(plan) = plan else {
    bail!("--kernel-sandbox=enforce refuses to start: no plan for this OS");
  };
  let _ = READ_DENIES.set(
    compiled
      .policy
      .fs
      .deny_read
      .iter()
      .flat_map(|d| [d.path.clone(), resolve_prefix(&d.path).full()])
      .collect(),
  );
  #[cfg(target_os = "linux")]
  {
    if let Some(applied) = deno_sandbox::enforce_linux::applied_plan() {
      // the plan applied at startup must be exactly this run's, and the
      // kernel must say so too; anything else refuses rather than run under
      // a policy this run didn't compile
      if applied.without_measured_lists() == plan.without_measured_lists()
        && deno_sandbox::enforce_linux::status_confined()
      {
        check_handed_down(options, &ctx.cwd, &plan)?;
        let _ = VERIFY_MODULES.set(applied.module_files.clone());
        start_enforcing();
        return Ok(());
      }
      bail!(
        "--kernel-sandbox=enforce refuses to start: the sandbox this process started under isn't the one this run compiles"
      );
    }
  }
  #[cfg(target_os = "macos")]
  {
    if let Some(inherited) = INHERITED.get() {
      let measured = deno_sandbox::macos::is_sandboxed() == Some(true)
        && plan.probe.as_deref().is_none_or(|probe| {
          deno_sandbox::macos::refuses_read(probe) == Some(true)
        });
      if inherited.without_measured_lists() == plan.without_measured_lists()
        && measured
      {
        check_handed_down(options, &ctx.cwd, &plan)?;
        let _ = VERIFY_MODULES.set(inherited.module_files.clone());
        start_enforcing();
        return Ok(());
      }
      bail!(
        "--kernel-sandbox=enforce refuses to start: the sandbox this process started under isn't the one this run compiles"
      );
    }
    if deno_sandbox::macos::is_sandboxed() == Some(true) {
      bail!(
        "--kernel-sandbox=enforce refuses to start: this process already runs inside another Seatbelt sandbox (an app sandbox, an agent's sandbox, sandbox-exec), and macOS doesn't stack a second profile"
      );
    }
  }
  let carried = carried_for(options, &ctx.cwd, &plan)?;
  if PENDING.set(plan).is_err() || PENDING_CARRIED.set(carried).is_err() {
    bail!("--kernel-sandbox=enforce: the plan was prepared twice");
  }
  Ok(())
}

/// The local files of the static module graph of `roots`, loaded first (the
/// first image fetches and caches what it needs while it still can).
async fn graph_module_files(
  factory: &crate::factory::CliFactory,
  roots: &[ModuleSpecifier],
) -> Result<Vec<PathBuf>, AnyError> {
  use crate::graph_container::ModuleGraphContainer;
  // an `npm:` entrypoint of a project that manages its own node_modules
  // ("manual") can't be a graph root (the run resolves it in node_modules,
  // which the policy reads; there is nothing to fetch): its own imports load
  // from node_modules at run time (item 451)
  let byonm = factory.npm_resolver().await?.is_byonm();
  let roots: Vec<ModuleSpecifier> = roots
    .iter()
    .filter(|r| !(byonm && r.scheme() == "npm"))
    .cloned()
    .collect();
  if roots.is_empty() {
    return Ok(Vec::new());
  }
  let container = factory.main_module_graph_container().await?;
  container
    .check_specifiers(&roots, Default::default())
    .await?;
  let graph = container.graph();
  let mut files: Vec<PathBuf> = graph
    .modules()
    .filter_map(|m| {
      let specifier = m.specifier();
      (specifier.scheme() == "file")
        .then(|| specifier.to_file_path().ok())
        .flatten()
    })
    .collect();
  files.sort();
  files.dedup();
  Ok(files)
}

/// Whether a plan waits for [`apply_pending`].
pub fn is_pending() -> bool {
  PENDING.get().is_some() && !kernel_sandbox_state::is_enforcing()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
/// The environment the re-executed image gets: scrubbed to the plan's list
/// when it has one, with `DENO_DIR` pinned to the one this image resolved
/// (so both compile the same policy).
fn reexec_env(
  plan: &EnforcePlan,
  deno_dir: Option<&Path>,
  carrier_fd: i32,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
  use deno_sandbox::inherit::POLICY_ENV_VAR;
  #[allow(clippy::disallowed_methods, reason = "the process's own environment")]
  let mut env: Vec<_> = std::env::vars_os()
    .filter(|(k, _)| k.to_str().is_some_and(|k| plan.keeps_env(k)))
    .filter(|(k, _)| k != POLICY_ENV_VAR)
    .collect();
  if let Some(dir) = deno_dir {
    env.retain(|(k, _)| k != "DENO_DIR");
    env.push(("DENO_DIR".into(), dir.as_os_str().to_os_string()));
  }
  // the carrier of the layers this run's children inherit (it replaces one
  // this process inherited: the new one holds those layers too)
  env.push((POLICY_ENV_VAR.into(), carrier_fd.to_string().into()));
  env
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
/// The carrier the re-executed image and its children inherit.
fn seal_carrier() -> Result<std::os::fd::OwnedFd, AnyError> {
  let Some(carried) = PENDING_CARRIED.get() else {
    bail!("--kernel-sandbox=enforce: no policy to hand down was prepared");
  };
  deno_sandbox::inherit::seal(&carried.to_bytes()).map_err(|err| {
    deno_core::anyhow::anyhow!(
      "--kernel-sandbox=enforce: the policy for child processes can't be handed down: {err}"
    )
  })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
/// Descriptors the re-executed image keeps: those this process inherited
/// (the launcher's capabilities) and the ones runtime variables name.
fn keep_fd(fd: i32) -> bool {
  if deno_runtime::deno_io::is_inherited_fd(fd) {
    return true;
  }
  #[allow(clippy::disallowed_methods, reason = "the process's own environment")]
  [
    "NODE_CHANNEL_FD",
    deno_runtime::deno_process::NPM_RESOLUTION_STATE_FD_ENV_VAR_NAME,
  ]
  .iter()
  .filter_map(|name| std::env::var(name).ok())
  .filter_map(|v| v.trim().parse::<i32>().ok())
  .any(|named| named == fd)
}

/// Applies the pending plan once the module graph of `roots` is loaded
/// (remote modules cached), before any user code runs. The process
/// re-executes under the plan (Linux, macOS), so this returns only an error
/// there.
pub async fn apply_pending(
  factory: &crate::factory::CliFactory,
  roots: &[ModuleSpecifier],
) -> Result<(), AnyError> {
  // the permission check that prepares the plan runs on first use
  factory.root_permissions_container()?;
  if let Some(handed_over) = VERIFY_MODULES.get() {
    // the image before this one listed these as the graph's modules: each
    // must be one of this run's own, or the hand-over widened something
    let files = graph_module_files(factory, roots).await?;
    check_module_files(handed_over)?;
    if let Some(stray) =
      handed_over.iter().find(|f| files.binary_search(f).is_err())
    {
      bail!(
        "--kernel-sandbox=enforce refuses to continue: {} was handed over as a module of this program, but it isn't one",
        stray.display()
      );
    }
    return Ok(());
  }
  let Some(pending) = PENDING.get() else {
    return Ok(());
  };
  if kernel_sandbox_state::is_enforcing() {
    return Ok(());
  }
  // graph first: the static graph is fetched and cached while the network
  // and the caches are still open; its local files stay readable
  let mut plan = pending.clone();
  plan.module_files = graph_module_files(factory, roots).await?;
  check_module_files(&plan.module_files)?;
  // the lockfile the graph load updated is written now: the confined image
  // can't write it (unless a grant covers it), and the re-exec runs no
  // exit hook (item 451)
  if let Some(lockfile) = factory.maybe_lockfile().await? {
    lockfile.write_if_changed()?;
  }
  let plan = &plan;
  let deno_dir = factory.deno_dir().ok().map(|d| d.root.clone());
  #[cfg(any(target_os = "linux", target_os = "macos"))]
  let carrier = seal_carrier()?;
  #[cfg(any(target_os = "linux", target_os = "macos"))]
  let carrier_fd = {
    use std::os::fd::AsRawFd;
    carrier.as_raw_fd()
  };
  #[cfg(any(target_os = "linux", target_os = "macos"))]
  let keep = |fd: i32| fd == carrier_fd || keep_fd(fd);
  // what this image printed (the type check's `Check`, downloads, warnings)
  // must reach the terminal: the exec discards anything still buffered
  // (item 451)
  log::logger().flush();
  {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
  }
  #[cfg(target_os = "linux")]
  {
    let env = reexec_env(plan, deno_dir.as_deref(), carrier_fd);
    let err = deno_sandbox::enforce_linux::reexec(plan, env, keep);
    drop(carrier);
    bail!(
      "--kernel-sandbox=enforce: re-executing under the sandbox failed: {err}"
    );
  }
  #[cfg(target_os = "macos")]
  {
    // re-executed, as on Linux: the new image applies the profile before it
    // opens anything, and nothing this image opened read-write (the caches)
    // or read (.npmrc, the environment) carries over
    let profile = macos_profile(plan).map_err(|err| {
      deno_core::anyhow::anyhow!("--kernel-sandbox=enforce: {err}")
    })?;
    let exe = std::env::current_exe()
      .and_then(|exe| exe.canonicalize())
      .map_err(|err| {
        deno_core::anyhow::anyhow!("--kernel-sandbox=enforce: {err}")
      })?;
    let env = reexec_env(plan, deno_dir.as_deref(), carrier_fd);
    if !uses_sandbox_exec_fallback() {
      let err = deno_sandbox::macos::reexec_self(plan, &exe, env, keep);
      drop(carrier);
      bail!(
        "--kernel-sandbox=enforce: re-executing under the sandbox failed: {err}"
      );
    }
    // sandbox-exec execs this binary under the profile, so the profile
    // lets it run exactly this binary (the process stays confined)
    let Some(exe) = exe.to_str() else {
      bail!("--kernel-sandbox=enforce: the runtime's path is not UTF-8");
    };
    let profile = deno_sandbox::seatbelt::with_self_exec(&profile, exe);
    let err =
      deno_sandbox::macos::reexec_under_sandbox_exec(&profile, plan, env, keep);
    drop(carrier);
    bail!(
      "--kernel-sandbox=enforce: re-executing under sandbox-exec failed: {err}"
    );
  }
  #[cfg(not(any(target_os = "linux", target_os = "macos")))]
  {
    let _ = (plan, deno_dir);
    bail!("--kernel-sandbox=enforce: no Mode A enforcer for this OS");
  }
}

/// Refuses to create a main worker while a plan is pending: a path that
/// would run user code without applying it fails closed.
pub fn guard_user_code() -> Result<(), std::io::Error> {
  if is_pending() {
    return Err(std::io::Error::other(
      "--kernel-sandbox=enforce is not supported for this command yet: it can't apply the sandbox before user code runs, so it refuses to start",
    ));
  }
  Ok(())
}

pub use deno_lib::kernel_inherit::permission_denied_note;
