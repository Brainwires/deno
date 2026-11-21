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
//! - A plan with a broker (Mode B: `--kernel-sandbox=broker`, an `enforce`
//!   the broker closes a gap for, and from the stage where `auto` enforces
//!   the `broker` it chooses) re-executes as the supervisor instead, which
//!   forks the confined worker (`deno_sandbox::supervise`). The worker
//!   leaves the `deno` / `denort` processes it starts a way to the same
//!   broker (`deno_lib::kernel_supervise`).
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
  // the supervisor image of --kernel-sandbox=broker: it forks the confined
  // worker, stays behind as its broker and never returns
  #[cfg(any(target_os = "linux", target_os = "macos"))]
  match deno_sandbox::supervise::take_handoff() {
    // ... or the supervisor image of an explicit warn (Linux): it observes
    // the run instead of brokering for it
    #[cfg(target_os = "linux")]
    Ok(Some(handoff)) if handoff.trace.is_some() => trace_supervise(handoff),
    Ok(Some(handoff)) => supervise(handoff),
    Ok(None) => {}
    Err(err) => refuse_at_startup(&err),
  }
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
      // this process confines itself: a dial it inherited belongs to an
      // ancestor's broker, which answers for that ancestor's grant
      deno_lib::kernel_supervise::drop_inherited_dial();
    }
    take_broker(
      deno_sandbox::enforce_linux::applied_plan()
        .and_then(|plan| plan.broker.as_ref()),
    );
    if deno_sandbox::enforce_linux::applied_plan().is_some()
      || kernel_reports_confined()
    {
      mark_confined();
    }
    // the policy handed down by a confined ancestor (or, in the image after
    // the re-exec, by the image before it), measured now the plan applies
    inherit::load_at_startup();
    adopt_broker_of_a_parent(
      deno_sandbox::enforce_linux::applied_plan().is_some(),
    );
  }
  #[cfg(target_os = "macos")]
  {
    match deno_sandbox::macos::take_inherited_plan() {
      Ok(Some(plan)) => {
        // (a dial this process inherited is given up before it confines
        // itself, as on Linux)
        deno_lib::kernel_supervise::drop_inherited_dial();
        if let Err(err) = apply_inherited_macos(&plan) {
          refuse_at_startup(&err);
        }
        use_cache_overlay(&plan);
        take_broker(plan.broker.as_ref());
        let _ = INHERITED.set(plan);
        mark_confined();
      }
      Ok(None) => {
        take_broker(None);
        if kernel_reports_confined() {
          mark_confined();
        }
      }
      Err(err) => refuse_at_startup(&err),
    }
    inherit::load_at_startup();
    adopt_broker_of_a_parent(INHERITED.get().is_some());
  }
}

/// A `deno` started inside a brokered run (it applies no plan of its own:
/// `own_plan` is false, and the kernel-verified carrier of a confined
/// ancestor was accepted) takes the way to that run's broker its parent
/// left: its own operations then get what its parent's get, where before
/// they met the kernel's refusals alone (no socket under a host-scoped
/// network, no change directly in a folder that holds a write deny). What
/// it may ask for is still decided by its own checks first, against its
/// grant intersected with the inherited one.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn adopt_broker_of_a_parent(own_plan: bool) {
  if own_plan {
    return;
  }
  deno_lib::kernel_supervise::adopt_inherited_dial(
    !inherit::inherited_layers().is_empty(),
  );
}

/// The worker's end of the broker channel, when the supervisor left one:
/// this process's broker if the plan it runs under has one (`expected`),
/// closed otherwise (a channel nobody compiled is never used). A plan with
/// a broker and no channel is refused later, in [`prepare`].
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn take_broker(expected: Option<&deno_sandbox::plan::BrokerPlan>) {
  match deno_sandbox::supervise::take_broker_fd() {
    Ok(Some(fd)) if expected.is_some() => {
      // the broker holds the network: every socket of this process is
      // asked of it from here on (before any module or user code runs)
      deno_lib::kernel_supervise::install_channel(
        fd,
        expected.is_some_and(|broker| broker.net.is_some()),
      );
      // the `deno` / `denort` processes this run starts reach the same
      // broker
      deno_lib::kernel_supervise::offer_dial();
    }
    Ok(_) => {}
    Err(err) => refuse_at_startup(&err),
  }
}

/// The supervisor image: starts the broker for the plan's policy, forks
/// the worker (which re-executes under the plan exactly as `enforce` does,
/// with its end of the broker channel) and exits as the worker exits.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn supervise(handoff: deno_sandbox::supervise::Handoff) -> ! {
  use deno_sandbox::supervise::BROKER_FD_ENV_VAR;

  let Some(broker) = handoff.plan.broker.clone() else {
    refuse_at_startup("the supervisor's hand-over has no broker policy");
  };
  let exec_worker = |broker_fd: i32| -> std::io::Error {
    let mut env = handoff.env_os();
    env.retain(|(k, _)| k != BROKER_FD_ENV_VAR);
    env.push((BROKER_FD_ENV_VAR.into(), broker_fd.to_string().into()));
    // everything this image holds is what the first image kept for the
    // worker (the launcher's descriptors, the policy carrier) and the
    // channel
    #[cfg(target_os = "linux")]
    {
      deno_sandbox::enforce_linux::reexec(&handoff.plan, env, |_| true)
    }
    #[cfg(target_os = "macos")]
    {
      match &handoff.sandbox_exec_profile {
        None => deno_sandbox::macos::reexec_self(
          &handoff.plan,
          &handoff.exe,
          env,
          |_| true,
        ),
        Some(profile) => deno_sandbox::macos::reexec_under_sandbox_exec(
          profile,
          &handoff.plan,
          env,
          |_| true,
        ),
      }
    }
  };
  let serve = move |sock: std::os::fd::OwnedFd| -> std::io::Result<()> {
    deno_lib::kernel_supervise::serve_broker(&broker, sock)
  };
  deno_sandbox::supervise::run(exec_worker, serve)
}

/// The supervisor image of an explicit `--kernel-sandbox=warn` on Linux:
/// forks the run under a tracing filter and reports what `enforce` would
/// refuse its native code and child processes (`deno_sandbox::trace`).
/// Nothing is refused; the run isn't confined.
#[cfg(target_os = "linux")]
fn trace_supervise(handoff: deno_sandbox::supervise::Handoff) -> ! {
  use deno_sandbox::trace::TRACED_ENV_VAR;
  let Some(trace) = handoff.trace.clone() else {
    refuse_at_startup("the supervisor's hand-over has no trace policy");
  };
  let exec_worker = |traced: bool| -> std::io::Error {
    let mut env = handoff.env_os();
    env.retain(|(k, _)| k != TRACED_ENV_VAR);
    // names the run (this process, once it executes) and says whether the
    // filter is in place; without it the run goes on unobserved and
    // doesn't try for an observer again
    env.push((
      TRACED_ENV_VAR.into(),
      deno_sandbox::trace::traced_env_value(traced).into(),
    ));
    deno_sandbox::supervise::exec_self(&handoff.exe, env)
  };
  deno_sandbox::supervise::run_traced(exec_worker, trace)
}

/// Re-executes this program as the tracing supervisor of an explicit
/// `--kernel-sandbox=warn` (Linux), with the `enforce` plan of the run's
/// grant to report against. The run starts again beneath it, unconfined,
/// with the descriptors it inherited. Returns only on failure (the run
/// then goes on as it is, without an observer).
#[cfg(target_os = "linux")]
pub(super) fn exec_trace_supervisor(
  plan: &EnforcePlan,
  trace: deno_sandbox::supervise::TraceHandoff,
) -> std::io::Error {
  use std::os::unix::ffi::OsStringExt;
  // what this image printed must reach the terminal: the exec discards
  // anything still buffered
  log::logger().flush();
  {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
  }
  let handoff = deno_sandbox::supervise::Handoff {
    plan: plan.clone(),
    env: std::env::vars_os()
      .map(|(k, v)| (k.into_vec(), v.into_vec()))
      .collect(),
    exe: "/proc/self/exe".into(),
    sandbox_exec_profile: None,
    trace: Some(trace),
  };
  deno_sandbox::supervise::exec_supervisor(&handoff, keep_fd)
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

pub use deno_lib::kernel_apply::create_cache_overlay;

/// Creates the folders and files the plan writes on the user's behalf (the
/// coverage and profile folders, `deno test`'s JUnit report) before the
/// re-exec: a Landlock rule needs its path to exist. A report file is
/// created empty when absent (the run writes it over at the end).
fn create_runtime_dirs(ctx: &Context) -> Result<(), AnyError> {
  for dir in ctx.coverage_dir.iter().chain(&ctx.profile_dirs) {
    std::fs::create_dir_all(dir).map_err(|err| {
      deno_core::anyhow::anyhow!(
        "--kernel-sandbox=enforce: creating the output folder {} failed: {err}",
        dir.display()
      )
    })?;
  }
  for file in &ctx.report_files {
    let created = file
      .parent()
      .map_or(Ok(()), std::fs::create_dir_all)
      .and_then(|_| {
        std::fs::OpenOptions::new()
          .write(true)
          .create(true)
          .truncate(false)
          .open(file)
          .map(|_| ())
      });
    created.map_err(|err| {
      deno_core::anyhow::anyhow!(
        "--kernel-sandbox=enforce: creating the report file {} failed: {err}",
        file.display()
      )
    })?;
  }
  Ok(())
}

/// The Seatbelt profile for a plan: its macOS part, with the program's
/// module files readable.
#[cfg(target_os = "macos")]
fn macos_profile(plan: &EnforcePlan) -> Result<String, String> {
  Ok(deno_sandbox::seatbelt::cli_profile(
    &macos_plan_with_modules(plan)?,
  ))
}

/// A plan's macOS part with the program's module files readable: what the
/// enforce profile is written from, and warn mode's reporting profile
/// (item 472).
#[cfg(target_os = "macos")]
pub(super) fn macos_plan_with_modules(
  plan: &EnforcePlan,
) -> Result<deno_sandbox::plan::MacosPlan, String> {
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
  Ok(macos)
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
///
/// Linux: this runtime's sandbox, told by its marker
/// (`deno_sandbox::inherit::kernel_reports_marked`). No-new-privileges and a
/// seccomp filter alone are any container's (Docker's
/// `--security-opt no-new-privileges` with its default seccomp profile,
/// Kubernetes' `allowPrivilegeEscalation: false`), and Linux stacks another
/// domain under them, so they must not turn `auto` or the harnesses' forced
/// enforce off. macOS: any Seatbelt sandbox, as macOS can't stack a second
/// profile under one.
pub fn confined_by_a_parent() -> bool {
  #[cfg(target_os = "linux")]
  {
    !confined_at_startup() && deno_sandbox::inherit::kernel_reports_marked()
  }
  #[cfg(target_os = "macos")]
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
  let Some(mut plan) = plan else {
    bail!("--kernel-sandbox=enforce refuses to start: no plan for this OS");
  };
  // the broker's copy of the network grant, as the flags wrote it (both
  // images run this with the same options, so the plans still compare)
  #[cfg(any(target_os = "linux", target_os = "macos"))]
  deno_lib::kernel_supervise::fill_broker_net(
    &mut plan,
    compiled,
    options,
    ctx.inspector_port,
  );
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
        check_broker(&plan)?;
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
        check_broker(&plan)?;
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

/// A plan with a broker runs only with its broker answering (the plan's
/// kernel rules alone would refuse what Deno's rules allow).
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn check_broker(plan: &EnforcePlan) -> Result<(), AnyError> {
  deno_lib::kernel_supervise::check_broker(plan).map_err(|why| {
    deno_core::anyhow::anyhow!(
      "--kernel-sandbox=broker refuses to start: {why}"
    )
  })
}

/// The local files of the static module graph of `roots`, loaded first (the
/// first image fetches and caches what it needs while it still can).
///
/// Only the modules the program can run: the code dependencies reachable
/// from `roots` (dynamic imports the graph holds included), never a module
/// reached through a type-only edge (`import type`, `@ts-types`, `/// <reference
/// types>`, `compilerOptions.types`, an npm package's `.d.ts`). The image
/// before the re-exec type-checks (its graph holds the types too) and the
/// confined one doesn't (its graph is code only), and both must list the
/// same files: the confined image checks that each file handed over is one
/// of its own.
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
  Ok(code_module_files(&graph, &roots))
}

/// The local files of the modules reachable from `roots` through code
/// edges only (see [`graph_module_files`]), sorted.
fn code_module_files(
  graph: &deno_graph::ModuleGraph,
  roots: &[ModuleSpecifier],
) -> Vec<PathBuf> {
  let mut files: Vec<PathBuf> = graph
    .walk(
      roots.iter(),
      deno_graph::WalkOptions {
        check_js: deno_graph::CheckJsOption::False,
        follow_dynamic: true,
        kind: deno_graph::GraphKind::CodeOnly,
        prefer_fast_check_graph: false,
      },
    )
    .filter_map(|(_, entry)| match entry {
      deno_graph::ModuleEntryRef::Module(m) => Some(m.specifier()),
      _ => None,
    })
    .filter(|specifier| specifier.scheme() == "file")
    .filter_map(|specifier| specifier.to_file_path().ok())
    .collect();
  files.sort();
  files.dedup();
  files
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
    // a dial this process inherited is not the new image's: it confines
    // itself, and the descriptors don't survive the exec
    .filter(|(k, _)| {
      k != deno_runtime::deno_permissions::kernel_broker::dial::DIAL_ENV_VAR
    })
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

/// The plan this process started confined by (the image after the re-exec),
/// if any.
fn applied_plan() -> Option<&'static EnforcePlan> {
  #[cfg(target_os = "linux")]
  {
    deno_sandbox::enforce_linux::applied_plan()
  }
  #[cfg(target_os = "macos")]
  {
    INHERITED.get()
  }
  #[cfg(not(any(target_os = "linux", target_os = "macos")))]
  {
    None
  }
}

pub use deno_sandbox::plan::PlanEntry;

/// The image after the re-exec of a `deno test` / `deno bench`: the modules
/// the image before it found, filtered and type-checked
/// ([`apply_pending_with_entries`]). `None` in any other process: the
/// command finds its modules itself. The caller checks each against its
/// own include patterns before it uses them.
pub fn handed_over_entries() -> Option<Vec<PlanEntry>> {
  let plan = applied_plan()?;
  // a confined image of a plain run carries none: only a test / bench
  // hand-over names entries, and it always names at least one
  (!plan.entries.is_empty()).then(|| plan.entries.clone())
}

/// The local files of `entries` (a doc-test source such as a Markdown file
/// isn't a module of the graph, but the confined image reads it to extract
/// its code blocks).
fn entry_files(entries: &[PlanEntry]) -> Vec<PathBuf> {
  let mut files: Vec<PathBuf> = entries
    .iter()
    .filter_map(|e| ModuleSpecifier::parse(&e.specifier).ok())
    .filter(|url| url.scheme() == "file")
    .filter_map(|url| url.to_file_path().ok())
    .collect();
  files.sort();
  files.dedup();
  files
}

/// The snapshot files `deno test` reads without a permission check beside
/// the modules of `entries` it runs (`__snapshots__/<file name>.snap`, the
/// default location `cli/ops/testing.rs` derives from the test module
/// itself): runner-managed data, as the coverage folder is, derived only
/// from entries the confined image checks against its own patterns.
fn snapshot_files(entries: &[PlanEntry]) -> Vec<PathBuf> {
  let mut files: Vec<PathBuf> = entries
    .iter()
    .filter(|e| e.run)
    .filter_map(|e| ModuleSpecifier::parse(&e.specifier).ok())
    .filter(|url| url.scheme() == "file")
    .filter_map(|url| url.to_file_path().ok())
    .filter_map(|path| {
      let name = path.file_name()?.to_string_lossy().into_owned();
      Some(
        path
          .parent()?
          .join("__snapshots__")
          .join(format!("{name}.snap")),
      )
    })
    .collect();
  files.sort();
  files.dedup();
  files
}

/// Applies the pending plan once the module graph of `roots` is loaded
/// (remote modules cached), before any user code runs. The process
/// re-executes under the plan (Linux, macOS), so this returns only an error
/// there.
pub async fn apply_pending(
  factory: &crate::factory::CliFactory,
  roots: &[ModuleSpecifier],
) -> Result<(), AnyError> {
  apply_pending_with_entries(factory, roots, &[], false).await
}

/// [`apply_pending`] for `deno test` / `deno bench` (`EnforceCoverage::
/// AfterSetup`): after the type check, with the modules the command runs
/// (`entries`), which the confined image runs without finding them again
/// ([`handed_over_entries`]); their local files are readable to it. In the
/// confined image, each module file handed over must be one of the graph's
/// or of the entries'. `snapshots` (`deno test`): the existing snapshot
/// files beside the modules it runs are readable too ([`snapshot_files`];
/// writing them, `--update-snapshots`, isn't covered: `auto` runs that as
/// `warn`).
pub async fn apply_pending_with_entries(
  factory: &crate::factory::CliFactory,
  roots: &[ModuleSpecifier],
  entries: &[PlanEntry],
  snapshots: bool,
) -> Result<(), AnyError> {
  // the permission check that prepares the plan runs on first use
  factory.root_permissions_container()?;
  // warn mode's observer applies where enforce would confine the process:
  // the graph loaded, no user code run yet (item 472)
  if super::warn::is_pending() {
    // a graph that doesn't load is the run's own error to report: no
    // observer then (it would report the program's modules)
    if let Ok(files) = graph_module_files(factory, roots).await {
      super::warn::apply_observer(files);
    }
    return Ok(());
  }
  if let Some(handed_over) = VERIFY_MODULES.get() {
    // the image before this one listed these as the graph's modules: each
    // must be one of this run's own, or the hand-over widened something
    let mut files = graph_module_files(factory, roots).await?;
    files.extend(entry_files(entries));
    if snapshots {
      // each derived from a checked entry; the image before this one handed
      // over only those that existed
      files.extend(snapshot_files(entries));
    }
    files.sort();
    files.dedup();
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
  plan.module_files.extend(entry_files(entries));
  if snapshots {
    // a read rule names an existing file; a snapshot file a test creates
    // needs --update-snapshots, which enforce doesn't cover
    plan
      .module_files
      .extend(snapshot_files(entries).into_iter().filter(|f| f.is_file()));
  }
  plan.module_files.sort();
  plan.module_files.dedup();
  plan.entries = entries.to_vec();
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
  if plan.broker.is_some() {
    let env = reexec_env(plan, deno_dir.as_deref(), carrier_fd);
    let err = exec_supervisor(plan, env, "/proc/self/exe".into(), None, keep);
    drop(carrier);
    bail!("--kernel-sandbox=broker: starting the supervisor failed: {err}");
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
    if plan.broker.is_some() {
      let fallback = if uses_sandbox_exec_fallback() {
        let Some(exe) = exe.to_str() else {
          bail!("--kernel-sandbox=broker: the runtime's path is not UTF-8");
        };
        Some(deno_sandbox::seatbelt::with_self_exec(&profile, exe))
      } else {
        None
      };
      let err = exec_supervisor(plan, env, exe.clone(), fallback, keep);
      drop(carrier);
      bail!("--kernel-sandbox=broker: starting the supervisor failed: {err}");
    }
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

/// Re-executes this program as the supervisor of `plan`'s broker (see
/// `deno_sandbox::supervise`); returns only on failure.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn exec_supervisor(
  plan: &EnforcePlan,
  env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
  exe: PathBuf,
  sandbox_exec_profile: Option<String>,
  keep: impl Fn(i32) -> bool,
) -> std::io::Error {
  use std::os::unix::ffi::OsStringExt;
  let handoff = deno_sandbox::supervise::Handoff {
    plan: plan.clone(),
    env: env
      .into_iter()
      .map(|(k, v)| (k.into_vec(), v.into_vec()))
      .collect(),
    exe,
    sandbox_exec_profile,
    trace: None,
  };
  deno_sandbox::supervise::exec_supervisor(&handoff, keep)
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

#[cfg(test)]
mod tests {
  use super::*;

  fn entry(specifier: &str, run: bool, doc: bool) -> PlanEntry {
    PlanEntry {
      specifier: specifier.to_string(),
      run,
      doc,
    }
  }

  /// The snapshot files are the default location `cli/ops/testing.rs`
  /// derives, beside modules that run, and nothing for a doc-only source or
  /// a remote module.
  #[cfg(unix)]
  #[test]
  fn snapshot_files_beside_run_entries() {
    let files = snapshot_files(&[
      entry("file:///w/a_test.ts", true, false),
      entry("file:///w/sub/b_test.ts", true, true),
      entry("file:///w/README.md", false, true),
      entry("https://example.com/c_test.ts", true, false),
    ]);
    assert_eq!(
      files,
      vec![
        PathBuf::from("/w/__snapshots__/a_test.ts.snap"),
        PathBuf::from("/w/sub/__snapshots__/b_test.ts.snap"),
      ]
    );
  }
}
