// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: what the CLI and `denort` share when the kernel sandbox applies
//! (DENEXT.md items 425 and 901): the machine's mechanisms, the context
//! pieces read from the process, and a compiled program's startup
//! ([`start_compiled`]).
//!
//! A compiled program applies its sandbox in-process, with no re-exec: its
//! grant is baked, its modules are embedded, and it has fetched nothing, so
//! the first image is already the one that may be confined. `denort` calls
//! [`start_compiled`] first thing in `main`, while the process has one
//! thread and has opened nothing (Linux: Landlock and seccomp apply to the
//! calling thread only; macOS: `sandbox_init` is process-wide, but what is
//! already open stays open).

#![allow(
  clippy::disallowed_methods,
  reason = "the sandbox acts on the real file system and environment, never a virtual one"
)]

use std::path::Path;
use std::path::PathBuf;

use deno_runtime::deno_permissions::PermissionsOptions;
use deno_runtime::deno_permissions::kernel_sandbox_state;
use deno_sandbox::plan::EnforcePlan;

use crate::kernel_inherit;
use crate::kernel_plan::plan_for;
use crate::kernel_policy::Capabilities;
use crate::kernel_policy::CompiledPolicy;
use crate::kernel_policy::Context;
use crate::kernel_policy::GapResolution;
use crate::kernel_policy::ImplicitSet;
use crate::kernel_policy::KernelPolicy;
use crate::kernel_policy::TargetOs;
use crate::kernel_sandbox::Engine;
use crate::kernel_sandbox::KernelSandboxMode;
use crate::kernel_sandbox::ModeResolution;
use crate::kernel_sandbox::compiled_resolution;

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

/// A packaged desktop app's kernel-sandbox startup: in place, before the
/// web engine starts and before any thread. `strict` is
/// `desktop.permissions: "strict"`, `engine` the engine baked into the
/// package ([`Engine`]).
///
/// A stub until WP-C (denext.30): it applies nothing and returns the
/// not-enforced outcome a desktop app gets today (the baked mode resolved as
/// for any compiled program, `enforcing: false`, no cache folder, no plan).
/// Nothing calls it yet.
pub fn start_desktop(
  start: &CompiledStart,
  strict: bool,
  engine: Engine,
) -> Result<CompiledOutcome, CompiledStartError> {
  let _ = (strict, engine);
  Ok(CompiledOutcome {
    resolution: compiled_resolution(
      start.baked,
      start.dev_build,
      start.options,
    ),
    enforcing: false,
    cache_dir: None,
    plan: None,
  })
}

/// A compiled program's kernel-sandbox startup, first thing in `main`
/// (one thread, nothing opened): decides the baked mode, and under
/// `enforce` compiles the policy for the baked grant and applies it to this
/// process, in place. `warn` prints its line; `broker`, an OS without an
/// enforcer and a rule the OS can't hold refuse to start (a mode `auto`
/// chose falls back instead: off with no enforcer, else `warn`'s line,
/// item 472). Whether the permission set allows prompting doesn't matter: a
/// compiled program's set is its author's, and once the sandbox applies the
/// caller turns prompting off (a prompt could only grant what the kernel
/// refuses).
pub fn start_compiled(
  start: &CompiledStart,
) -> Result<CompiledOutcome, CompiledStartError> {
  let resolution =
    compiled_resolution(start.baked, start.dev_build, start.options);
  let mode = resolution.mode;
  let outcome = |enforcing, cache_dir, plan| CompiledOutcome {
    resolution: resolution.clone(),
    enforcing,
    cache_dir,
    plan,
  };
  match mode {
    KernelSandboxMode::Warn | KernelSandboxMode::Enforce => {}
    KernelSandboxMode::Broker => {
      return Err(refuse(
        mode,
        "the supervising broker isn't built into this runtime",
      ));
    }
    KernelSandboxMode::Off | KernelSandboxMode::Auto => {
      return Ok(outcome(false, None, None));
    }
  }
  let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
  let mut ctx = compiled_context(mode, start.exec_strict, cwd);
  let mut compiled = compile_policy(start.options, &ctx);
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
      compiled = compile_policy(start.options, &ctx);
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
  let Some(plan) = plan else {
    if auto {
      return Ok(fall_back("no plan for this OS".to_string()));
    }
    return Err(refuse(mode, "no plan for this OS"));
  };
  let mut layers = kernel_inherit::inherited_layers().to_vec();
  layers.push(kernel_inherit::layer_for(
    start.options,
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
  apply_in_place(&plan, &carried).map_err(|why| refuse(mode, why))?;
  kernel_sandbox_state::set_enforcing();
  deno_runtime::deno_core::error::set_permission_denied_note(
    kernel_inherit::permission_denied_note,
  );
  Ok(outcome(true, ctx.cache_overlay.clone(), Some(plan)))
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
  fn broker_refuses_and_a_dev_build_is_off() {
    let options = options(&["/tmp"]);
    let err = start_compiled(&CompiledStart {
      baked: Some(KernelSandboxMode::Broker),
      exec_strict: false,
      dev_build: false,
      options: &options,
      cache_base: None,
    })
    .unwrap_err();
    assert!(
      err.0.contains("--kernel-sandbox=broker")
        && err.0.contains("broker isn't built"),
      "{err}"
    );
    // a desktop development build never enforces (hot reload rewrites it)
    let outcome = start_compiled(&CompiledStart {
      baked: Some(KernelSandboxMode::Enforce),
      exec_strict: false,
      dev_build: true,
      options: &options,
      cache_base: None,
    })
    .unwrap();
    assert!(!outcome.enforcing);
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
