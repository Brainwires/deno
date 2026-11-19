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
//! - [`enforce`]: applying the policy under `--kernel-sandbox=enforce`
//!   (Mode A on Linux and macOS, DENEXT.md item 425).
//! - [`inherit`]: the policy a confined process hands the `deno` processes
//!   it starts, and the intersection they apply (DENEXT.md item 449).
//!
//! Opt-in only: `auto` applies nothing in this release line, `warn`
//! reports, `enforce` applies the policy (or refuses to start when a rule
//! can't be held), and `broker` refuses to start (Mode B isn't built yet).

pub mod enforce;
pub mod inherit;
pub mod policy;
pub mod warn;

use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Once;

use deno_core::anyhow::bail;
use deno_core::error::AnyError;
use deno_core::serde_json;
use deno_lib::kernel_sandbox::AutoPlan;
use deno_lib::kernel_sandbox::AutoStage;
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

/// The mechanisms this machine offers, and whether this runtime has a Mode A
/// enforcer for them (Linux on x86_64 / aarch64, macOS).
pub fn detect_capabilities() -> Capabilities {
  let os = TargetOs::current();
  #[allow(unused_mut, reason = "set per OS")]
  let mut caps = Capabilities::none(os);
  #[cfg(target_os = "linux")]
  {
    caps.landlock_abi = crate::supply_chain::sandbox_linux::landlock_abi().ok();
    // the seccomp programs are assembled for these architectures (the frame
    // kills other architectures' syscalls); whether this process may
    // install one (an outer profile may refuse it) is probed
    let arch = cfg!(any(target_arch = "x86_64", target_arch = "aarch64"));
    caps.seccomp =
      arch && deno_sandbox::enforce_linux::seccomp_available().is_ok();
    caps.user_namespaces =
      crate::supply_chain::sandbox_linux::netns_available().is_ok();
    caps.enforcer_built = arch;
  }
  #[cfg(target_os = "macos")]
  {
    caps.seatbelt = deno_sandbox::macos::sandbox_init_available()
      || crate::supply_chain::sandbox_macos::available().is_ok();
    caps.enforcer_built = true;
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

/// Whether `enforce` covers the command: `run` of a file and `serve` (the
/// others refuse to start under an `enforce` asked for by name; one `auto`
/// chose runs them as `warn`).
fn enforce_covers(subcommand: &DenoSubcommand) -> bool {
  match subcommand {
    DenoSubcommand::Run(run) => !run.is_stdin(),
    DenoSubcommand::Serve(_) => true,
    _ => false,
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
/// fails closed is refused while compiled programs have no enforcer, so
/// no binary is made that can't start.
pub fn compile_mode(
  cli_options: &CliOptions,
) -> Result<Option<KernelSandboxMode>, AnyError> {
  let mode = match flag_mode(cli_options)? {
    Some(mode) => Some(mode),
    None => config_mode(cli_options)?,
  };
  // compiled programs get their enforcer in a later phase: until then
  // a binary baked with a failing-closed mode couldn't start
  if let Some(mode) = mode
    && mode.fails_closed()
  {
    bail!(
      "--kernel-sandbox={mode}: compiled programs have no kernel enforcer yet, so a program compiled with it could not start. Use --kernel-sandbox=warn, auto or off."
    );
  }
  Ok(mode)
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
    .filter(|p| p.is_file())
    .collect();
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
      // `npm:` / `jsr:` entrypoint, a folder or a Node binary main: none
      let plain = script.starts_with('.')
        || std::path::Path::new(script).is_absolute()
        || ["file:", "http:", "https:"]
          .iter()
          .any(|scheme| script.starts_with(scheme));
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

/// The certificate files and folders the environment names (read only with
/// network; each variable is kept through the re-exec's scrub).
fn tls_files() -> Vec<PathBuf> {
  #[allow(clippy::disallowed_methods, reason = "the real environment")]
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

/// The policy-keyed cache overlay for a policy compiled without one:
/// `DENO_DIR/sandboxed/<hash>`. Confined runs of that policy keep their
/// SQLite caches and new emits there; every other run's cache paths are
/// outside `sandboxed/`, so none reads what a confined run wrote (item 451).
pub fn cache_overlay_for(deno_dir: &std::path::Path, hash: &str) -> PathBuf {
  deno_dir
    .join(deno_resolver::cache::SANDBOX_OVERLAY_DIR)
    .join(hash)
}

/// The terminals the standard streams are (`ttyname` of fds 0 to 2),
/// deduplicated.
fn std_stream_ttys() -> Vec<PathBuf> {
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
/// the `warn` notice, refuses to start under `enforce` / `broker` when a
/// rule can't be held, and otherwise prepares the enforcement plan
/// ([`enforce::prepare`]; applied before user code runs).
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
  // a mode `auto` chose never refuses to start (item 957): where enforce
  // can't hold the grant, the run is a warn one, and says why
  let covered = enforce_covers(&cli_options.flags().subcommand);
  let mut degraded: Option<String> = None;
  if resolution.auto_chosen && resolution.mode.fails_closed() {
    if enforce::confined_by_a_parent() || !detect_capabilities().enforcer_built
    {
      // the parent's sandbox already holds this process and its children;
      // or this runtime has no enforcer for the OS: the run stays as it was
      resolution.mode = KernelSandboxMode::Off;
      resolution.auto_chosen = false;
    } else if !covered {
      degraded = Some("enforce doesn't cover this command yet".to_string());
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
  let options = &policy_options(cli_options, options);
  let mut ctx = ctx;
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
    if report {
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
        let implicit = ImplicitSet::build(options, &ctx);
        enforced = (KernelPolicy::compile(options, &implicit, &ctx), ctx);
        (&enforced.0, &enforced.1)
      };
      warn::start(warn::WarnRun {
        resolution: &resolution,
        compiled,
        ctx,
        options,
        observable: covered,
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
    if resolution.deny_only {
      "; --allow-all with denies: only the denies apply"
    } else {
      ""
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
  } else if !policy::BROKER_BUILT {
    warnings.push(
      "kernel sandbox: the supervising broker (Mode B) is not built in yet; --kernel-sandbox=broker refuses to start, and enforce refuses rules only the broker can hold".to_string(),
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
    os_sandbox: os_sandbox(&caps),
    landlock_abi: caps.landlock_abi,
    capabilities: caps,
    features,
    warnings,
  }
}
