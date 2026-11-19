// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: the kernel sandbox's warn mode (DENEXT.md item 472):
//! `--kernel-sandbox=warn` computes what `enforce` would refuse and reports
//! it, each line naming the grant that would allow the access; nothing is
//! refused.
//!
//! What reports (every source hands a
//! [`deno_sandbox::would_deny::WouldDeny`] to the one reporter):
//!
//! 1. The policy, at startup ([`start`]): each rule the OS can't hold here,
//!    over which `enforce` would refuse to start.
//! 2. The runtime's own decision points while the program runs: an advisory
//!    for each program it starts that is known to need a grant or to fail
//!    inside a sandbox (`deno_permissions::kernel_sandbox_state::note_spawn`),
//!    and whatever the supervisor, the network connector and the desktop host
//!    refuse under `enforce`.
//! 3. The kernel itself, for native code and child processes, where it can
//!    report without refusing ([`apply_observer`]): on macOS the run applies
//!    a Seatbelt profile that allows everything and reports exactly what the
//!    enforce profile for the same plan refuses; the lines are read back from
//!    the unified log when the process ends ([`finish`]). Linux has no
//!    permissive mode for Landlock, so there is no observer there: what
//!    native code and children would be refused shows only under `enforce`.
//!
//! The observer is applied only for a `warn` someone asked for by name (the
//! flag, deno.json, `DENO_KERNEL_SANDBOX`): a Seatbelt profile, even one
//! that allows everything, makes a program that applies a sandbox of its own
//! (a browser, `sandbox-exec`, a nested `--kernel-sandbox=enforce`) fail, as
//! it would under `enforce`. The `warn` that `auto` chooses for a run with
//! no terminal ([`AutoStage::R3`](deno_lib::kernel_sandbox::AutoStage))
//! applies nothing to the process: it reports from the policy and the
//! runtime only, behind one notice per policy.

use std::path::PathBuf;
use std::sync::OnceLock;

#[cfg(target_os = "macos")]
use deno_core::serde_json;
use deno_lib::kernel_sandbox::ModeResolution;
use deno_runtime::deno_permissions::PermissionsOptions;
use deno_runtime::deno_permissions::kernel_sandbox_state;
use deno_sandbox::plan::EnforcePlan;
use deno_sandbox::would_deny;
use deno_sandbox::would_deny::Observer;
use deno_sandbox::would_deny::Record;
use deno_sandbox::would_deny::WouldDeny;

use super::policy::CompiledPolicy;
use super::policy::Context;
use super::policy::PolicyGap;

/// What [`start`] is told about the run.
pub struct WarnRun<'a> {
  pub resolution: &'a ModeResolution,
  pub compiled: &'a CompiledPolicy,
  pub ctx: &'a Context,
  pub options: &'a PermissionsOptions,
  /// `run` / `serve` (the commands whose user code starts after
  /// [`apply_observer`]).
  pub observable: bool,
  /// `auto` chose `enforce` but it can't hold here: why.
  pub degraded: Option<String>,
  /// Print the mode's line (not when `--kernel-sandbox-report` printed the
  /// report instead).
  pub header: bool,
  /// The `.npmrc` files the policy leaves unreadable on purpose.
  pub skipped_npmrcs: Vec<PathBuf>,
}

/// A warn run waiting for [`apply_observer`].
#[cfg_attr(
  not(target_os = "macos"),
  allow(dead_code, reason = "read by the macOS observer")
)]
struct Pending {
  plan: EnforcePlan,
  policy: String,
  /// Read and written silently by the reporting profile.
  quiet: Vec<String>,
  /// Paths this process reads itself that an enforced run doesn't.
  own_quiet: Vec<PathBuf>,
}

static PENDING: OnceLock<Pending> = OnceLock::new();

/// The observer applied to this process.
#[cfg(target_os = "macos")]
struct Observed {
  tag: String,
  /// The reader saw the line of sentinel number N (the last access of the
  /// run that matters has been read by then).
  seen_sentinel: std::sync::Mutex<u32>,
  seen: std::sync::Condvar,
}

#[cfg(target_os = "macos")]
static OBSERVED: OnceLock<Observed> = OnceLock::new();

/// The mach service name a sentinel looks up, before its number: no such
/// service exists, the reporting profile reports the lookup like any other
/// it doesn't allow, and the reader takes the line as "read up to here".
#[cfg(target_os = "macos")]
const SENTINEL_SERVICE: &str = "denext.kernel-sandbox.warn.sentinel.";

/// Whether the observer is applied to this process.
fn observer_applied() -> bool {
  #[cfg(target_os = "macos")]
  {
    OBSERVED.get().is_some()
  }
  #[cfg(not(target_os = "macos"))]
  {
    false
  }
}

/// The gaps `enforce` would refuse to start over: the policy's own, and the
/// ones translating it for this OS adds. One per rule.
pub fn fatal_gaps(compiled: &CompiledPolicy, ctx: &Context) -> Vec<PolicyGap> {
  let mut gaps: Vec<PolicyGap> = compiled.fatal_gaps().cloned().collect();
  if gaps.is_empty() && cfg!(any(target_os = "linux", target_os = "macos")) {
    // the translation is only worth making for a policy that would get
    // that far
    gaps.extend(
      deno_lib::kernel_plan::plan_for(compiled, ctx)
        .1
        .into_iter()
        .filter(|g| g.is_fatal_under_enforce()),
    );
  }
  let mut seen = std::collections::HashSet::new();
  gaps.retain(|g| seen.insert((g.rule.clone(), g.detail.clone())));
  gaps
}

/// Whether the notice for `policy` was already printed on this machine (a
/// marker in DENO_DIR); records it otherwise. Without a DENO_DIR to write,
/// every run prints.
fn notice_seen(ctx: &Context, policy: &str) -> bool {
  let Some(deno_dir) = &ctx.deno_dir else {
    return false;
  };
  let dir = deno_dir.join("kernel_sandbox_notices");
  let marker = dir.join(policy);
  #[allow(clippy::disallowed_methods, reason = "the real file system")]
  {
    if marker.exists() {
      return true;
    }
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&marker, b"");
  }
  false
}

/// The notice of a `warn` that `auto` chose: what will change, and what to
/// do about it now.
fn auto_notice(run: &WarnRun, fatal: usize) -> Option<String> {
  let o = run.options;
  let native = o.allow_run.is_some() || o.allow_ffi.is_some();
  if let Some(why) = &run.degraded {
    return Some(format!(
      "kernel sandbox: auto can't enforce this run's permissions with the kernel here ({why}); it runs with Deno's checks only."
    ));
  }
  if !run.ctx.caps.enforcer_built {
    // no enforcer for this OS in this runtime: nothing is coming for this
    // run to prepare for
    return None;
  }
  if !native && fatal == 0 {
    // nothing native runs under this grant and every rule can be held:
    // enforcement would be invisible to this program
    return None;
  }
  let mut out = String::from(
    "kernel sandbox: this run has no terminal, so its permissions are fixed, and a later release holds them with the kernel",
  );
  if native {
    out.push_str(
      ": the programs it starts and the native code it loads will get the same file and network grants as the program",
    );
  }
  out.push_str(". Nothing is refused now. See what would be with --kernel-sandbox=warn, choose a mode in deno.json (\"permissions\": { \"kernelSandbox\": … }), or set DENO_KERNEL_SANDBOX=off.");
  Some(out)
}

/// Starts warn-mode reporting for a run whose mode is `warn`: installs the
/// reporter, prints the mode's line, reports the rules `enforce` would
/// refuse to start over, and leaves the plan for [`apply_observer`].
pub fn start(run: WarnRun) {
  let fatal = fatal_gaps(run.compiled, run.ctx);
  let policy = run.compiled.hash();
  let auto = run.resolution.auto_chosen || run.degraded.is_some();
  if auto {
    // one notice per policy on this machine, and nothing at all for a run
    // enforcement wouldn't change
    let Some(notice) = auto_notice(&run, fatal.len()) else {
      return;
    };
    // a run that fell back from enforce has a notice of its own
    let key = match &run.degraded {
      Some(_) => format!("{policy}-fallback"),
      None => policy.clone(),
    };
    if notice_seen(run.ctx, &key) {
      return;
    }
    log::warn!("{} {notice}", crate::colors::yellow("Warning"));
  } else if run.header {
    #[allow(clippy::print_stderr, reason = "the warn mode's line")]
    {
      eprintln!(
        "{} kernel sandbox (warn, preview): nothing is refused; {} rule(s) would be held by Deno's checks only. Run with --kernel-sandbox-report for the policy.",
        crate::colors::yellow("Warning"),
        fatal.len(),
      );
    }
  }
  #[allow(clippy::disallowed_methods, reason = "the process's own environment")]
  let log = std::env::var_os(would_deny::WARN_LOG_ENV_VAR)
    .filter(|v| !v.is_empty())
    .map(PathBuf::from);
  if !would_deny::install(would_deny::ReporterConfig { log, print: true }) {
    return;
  }
  kernel_sandbox_state::set_warning();
  for gap in &fatal {
    would_deny::report(WouldDeny::startup(&gap.rule, &gap.detail));
  }
  deno_signals::before_exit(finish);
  // the observer: only for a warn asked for by name, on a command whose
  // user code starts after `apply_observer`, with a plan to report against
  #[allow(clippy::disallowed_methods, reason = "the process's own environment")]
  let observe =
    std::env::var_os(would_deny::WARN_OBSERVE_ENV_VAR).is_none_or(|v| v != "0");
  let plan = (!auto && observe && run.observable && cfg!(target_os = "macos"))
    .then(|| deno_lib::kernel_plan::plan_for(run.compiled, run.ctx).0)
    .flatten();
  let Some(plan) = plan else {
    would_deny::record_run(Record::Run {
      v: would_deny::WOULD_DENY_FORMAT,
      pid: std::process::id(),
      os: std::env::consts::OS.to_string(),
      policy: Some(policy),
      observer: Observer::None,
      tag: None,
      since: None,
    });
    return;
  };
  let quiet = run
    .ctx
    .deno_dir
    .iter()
    .filter_map(|d| {
      #[allow(clippy::disallowed_methods, reason = "the real folder")]
      let d = d.canonicalize().unwrap_or_else(|_| d.clone());
      d.to_str().map(|s| s.to_string())
    })
    .collect();
  let _ = PENDING.set(Pending {
    plan,
    policy,
    quiet,
    own_quiet: run.skipped_npmrcs,
  });
}

/// Whether [`apply_observer`] has something to apply.
pub fn is_pending() -> bool {
  PENDING.get().is_some() && !observer_applied()
}

/// Applies the observer once the module graph is loaded and before user
/// code runs (where `enforce` confines the process): on macOS, the
/// reporting Seatbelt profile for the plan, with the graph's local files
/// `module_files` silent as under `enforce`. Never an error: an observer
/// that can't be applied is said once, and the run goes on.
pub fn apply_observer(module_files: Vec<PathBuf>) {
  let Some(pending) = PENDING.get() else {
    return;
  };
  if observer_applied() {
    return;
  }
  #[cfg(target_os = "macos")]
  {
    let applied = apply_seatbelt_report(pending, module_files);
    let (observer, tag, since) = match applied {
      Ok((tag, since)) => (Observer::SeatbeltReport, Some(tag), Some(since)),
      Err(why) => {
        log::warn!(
          "{} kernel sandbox (warn): what native code and child processes would be refused isn't observed in this run: {why}",
          crate::colors::yellow("Warning"),
        );
        (Observer::None, None, None)
      }
    };
    would_deny::record_run(Record::Run {
      v: would_deny::WOULD_DENY_FORMAT,
      pid: std::process::id(),
      os: std::env::consts::OS.to_string(),
      policy: Some(pending.policy.clone()),
      observer,
      tag,
      since,
    });
  }
  #[cfg(not(target_os = "macos"))]
  {
    let _ = (pending, module_files);
  }
}

/// The program that streams this run's report lines: `log stream` with the
/// run's tag as its filter, and a shell around it that ends it within a few
/// seconds of this process's end, however it ends (`$1`: the predicate,
/// `$2`: this process).
#[cfg(target_os = "macos")]
const STREAM_SCRIPT: &str = r#"/usr/bin/log stream --style ndjson --predicate "$1" &
L=$!
while kill -0 "$2" 2>/dev/null; do sleep 2; done
sleep 1
kill "$L" 2>/dev/null"#;

#[cfg(target_os = "macos")]
fn apply_seatbelt_report(
  pending: &Pending,
  module_files: Vec<PathBuf>,
) -> Result<(String, u64), String> {
  use std::io::BufRead;
  if deno_sandbox::macos::is_sandboxed() != Some(false) {
    return Err(
      "this process already runs inside a Seatbelt sandbox, and macOS doesn't stack a second profile".to_string(),
    );
  }
  if !deno_sandbox::macos::sandbox_init_available() {
    return Err("sandbox_init isn't available here".to_string());
  }
  let mut plan = pending.plan.clone();
  plan.module_files = module_files;
  let macos = super::enforce::macos_plan_with_modules(&plan)?;
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_err(|err| err.to_string())?;
  let tag = format!(
    "denext-ks:{}-{:x}",
    std::process::id(),
    now.subsec_nanos() ^ (now.as_secs() as u32)
  );
  let profile =
    deno_sandbox::seatbelt::cli_report_profile(&macos, &tag, &pending.quiet)?;
  // The reader starts first, outside the profile: macOS's `log` refuses to
  // run inside any sandbox, a reporting one included, and a process started
  // before the profile applies stays outside it.
  #[allow(clippy::disallowed_methods, reason = "the system's own log tool")]
  let mut child = std::process::Command::new("/bin/sh")
    .args([
      "-c",
      STREAM_SCRIPT,
      "sh",
      &format!("sender == \"Sandbox\" AND eventMessage CONTAINS \"{tag}\""),
      &std::process::id().to_string(),
    ])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::null())
    .spawn()
    .map_err(|err| format!("the log stream can't start: {err}"))?;
  let stdout = child.stdout.take().ok_or("the log stream has no output")?;
  let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
  let own_quiet = pending.own_quiet.clone();
  let own_pid = std::process::id();
  let reader_tag = tag.clone();
  std::thread::Builder::new()
    .name("kernel-sandbox-warn".to_string())
    .spawn(move || {
      let mut ready = Some(ready_tx);
      for line in std::io::BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        // the stream's first line says its filter is in place
        if let Some(tx) = ready.take() {
          let _ = tx.send(());
        }
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(&line) else {
          continue;
        };
        let Some(message) = entry.get("eventMessage").and_then(|m| m.as_str())
        else {
          continue;
        };
        let Some(event) =
          would_deny::parse_seatbelt_report(message, &reader_tag)
        else {
          continue;
        };
        let own = event.process.as_ref().map(|p| p.pid) == Some(own_pid);
        if own && let Some(n) = event.target.strip_prefix(SENTINEL_SERVICE) {
          if let (Ok(n), Some(observed)) = (n.parse::<u32>(), OBSERVED.get())
            && let Ok(mut seen) = observed.seen_sentinel.lock()
          {
            *seen = (*seen).max(n);
            observed.seen.notify_all();
          }
          continue;
        }
        // the runtime sizes itself from system information at startup
        // and does without it under enforce; `sys` is held by Deno's own
        // checks for the program
        if own && event.class == would_deny::AccessClass::Sys {
          continue;
        }
        // this process's own reads of files an enforced run never opens
        if own
          && own_quiet
            .iter()
            .any(|p| p.to_str() == Some(event.target.as_str()))
        {
          continue;
        }
        would_deny::report(event);
      }
      let _ = child.wait();
    })
    .map_err(|err| err.to_string())?;
  if ready_rx
    .recv_timeout(std::time::Duration::from_secs(5))
    .is_err()
  {
    return Err(
      "the system log stream didn't start (reading it may need an administrator account)".to_string(),
    );
  }
  deno_sandbox::macos::apply_profile(&profile)?;
  let _ = OBSERVED.set(Observed {
    tag: tag.clone(),
    seen_sentinel: std::sync::Mutex::new(0),
    seen: std::sync::Condvar::new(),
  });
  // `log stream` prints its first line before it is attached: wait, before
  // any user code runs, until it delivers a report of this process's own
  if let Some(observed) = OBSERVED.get()
    && !sync_seatbelt_reports(
      observed,
      ATTACH_TIMEOUT,
      std::time::Duration::from_millis(100),
    )
  {
    log::warn!(
      "{} kernel sandbox (warn): the system log stream delivered nothing yet; reports of native code and child processes may be missing from this run. Read them with: log show --last 10m --predicate 'eventMessage CONTAINS \"{tag}\"'",
      crate::colors::yellow("Warning"),
    );
  }
  Ok((tag, now.as_secs()))
}

#[cfg(target_os = "macos")]
#[allow(non_camel_case_types, reason = "the system's names")]
mod mach {
  pub type mach_port_t = std::ffi::c_uint;
  unsafe extern "C" {
    pub static bootstrap_port: mach_port_t;
    pub fn bootstrap_look_up(
      bp: mach_port_t,
      service_name: *const std::ffi::c_char,
      sp: *mut mach_port_t,
    ) -> std::ffi::c_int;
  }
}

/// The longest the end of a warn run waits for its last report lines (it
/// waits only until they are in: a fraction of a second on an idle machine).
#[cfg(target_os = "macos")]
const DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The longest a warn run waits, before user code starts, for the log
/// stream to deliver its first line.
#[cfg(target_os = "macos")]
const ATTACH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// Waits until the reader has read everything this process's tree did so
/// far: looks up a mach service that doesn't exist (the profile reports the
/// lookup) and waits for that line to come back; a sentinel the stream
/// didn't deliver (it wasn't attached yet, or dropped a line) is followed by
/// another every `step`, until `timeout`. Returns whether one came back.
#[cfg(target_os = "macos")]
fn sync_seatbelt_reports(
  observed: &Observed,
  timeout: std::time::Duration,
  step: std::time::Duration,
) -> bool {
  static NEXT: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(1);
  let deadline = std::time::Instant::now() + timeout;
  loop {
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let Ok(name) = std::ffi::CString::new(format!("{SENTINEL_SERVICE}{n}"))
    else {
      return false;
    };
    let mut port: mach::mach_port_t = 0;
    // SAFETY: a lookup of a name in this task's bootstrap port; the name is
    // a NUL-terminated string that outlives the call and `port` a valid out
    // pointer. The lookup fails (no such service); only its report matters.
    unsafe {
      mach::bootstrap_look_up(mach::bootstrap_port, name.as_ptr(), &mut port);
    }
    let Ok(seen) = observed.seen_sentinel.lock() else {
      return false;
    };
    let (seen, _) = observed
      .seen
      .wait_timeout_while(seen, step, |seen| *seen < n)
      .unwrap_or_else(|e| e.into_inner());
    if *seen >= n {
      return true;
    }
    drop(seen);
    if std::time::Instant::now() >= deadline {
      return false;
    }
  }
}

/// The end of a warn run (an exit hook): waits for the observer's last
/// lines, then prints the summary: how many accesses `enforce` would refuse
/// and the grants that would allow them.
fn finish() {
  static ONCE: std::sync::Once = std::sync::Once::new();
  ONCE.call_once(|| {
    #[cfg(target_os = "macos")]
    if let Some(observed) = OBSERVED.get()
      && !sync_seatbelt_reports(
        observed,
        DRAIN_TIMEOUT,
        std::time::Duration::from_millis(500),
      )
    {
      log::warn!(
        "{} kernel sandbox (warn): the kernel's last report lines didn't come back in time; read them with: log show --last 10m --predicate 'eventMessage CONTAINS \"{}\"'",
        crate::colors::yellow("Warning"),
        observed.tag
      );
    }
    if let Some(line) = would_deny::summary_line(&would_deny::summary()) {
      log::warn!("{} {line}", crate::colors::yellow("Warning"));
    }
    log::logger().flush();
  });
}

#[cfg(test)]
mod tests {
  use deno_lib::kernel_sandbox::KernelSandboxMode;
  use deno_lib::kernel_sandbox::ModeInputs;
  use deno_lib::kernel_sandbox::RunKind;
  use deno_lib::kernel_sandbox::kernel_sandbox_mode;

  use super::super::policy::Capabilities;
  use super::super::policy::ImplicitSet;
  use super::super::policy::KernelPolicy;
  use super::*;

  fn compiled_for(options: &PermissionsOptions) -> (CompiledPolicy, Context) {
    // what `enforce` would do with the grant is what warn reports
    let ctx = Context::new(
      Capabilities {
        landlock_abi: Some(4),
        seccomp: true,
        enforcer_built: true,
        ..Capabilities::none(super::super::policy::TargetOs::Linux)
      },
      KernelSandboxMode::Enforce,
      PathBuf::from("/work"),
    );
    let implicit = ImplicitSet::build(options, &ctx);
    (KernelPolicy::compile(options, &implicit, &ctx), ctx)
  }

  fn resolution(auto: bool) -> ModeResolution {
    kernel_sandbox_mode(&ModeInputs {
      kind: Some(RunKind::Run),
      prompt: true,
      flag: (!auto).then_some(KernelSandboxMode::Warn),
      stage: auto.then_some(deno_lib::kernel_sandbox::AutoStage::R3),
      ..Default::default()
    })
    .unwrap()
  }

  // the notice of a warn `auto` chose: nothing for a run enforcement would
  // not change, the children and native code named when granted, and the
  // reason when enforce was the choice but can't hold
  #[test]
  fn auto_notice_only_where_enforcement_would_show() {
    let quiet = PermissionsOptions {
      allow_read: Some(vec!["/work".to_string()]),
      ..Default::default()
    };
    let (compiled, ctx) = compiled_for(&quiet);
    let r = resolution(true);
    assert!(r.auto_chosen);
    let run = |options, degraded| WarnRun {
      resolution: &r,
      compiled: &compiled,
      ctx: &ctx,
      options,
      observable: true,
      degraded,
      header: true,
      skipped_npmrcs: Vec::new(),
    };
    assert_eq!(auto_notice(&run(&quiet, None), 0), None);
    // a rule the OS can't hold is worth a notice even with nothing native
    assert!(auto_notice(&run(&quiet, None), 1).is_some());
    let native = PermissionsOptions {
      allow_run: Some(vec!["git".to_string()]),
      ..quiet.clone()
    };
    let notice = auto_notice(&run(&native, None), 0).unwrap();
    assert!(notice.contains("no terminal"), "{notice}");
    assert!(notice.contains("the programs it starts"), "{notice}");
    assert!(notice.contains("--kernel-sandbox=warn"), "{notice}");
    assert!(notice.contains("DENO_KERNEL_SANDBOX=off"), "{notice}");
    let notice = auto_notice(
      &run(&quiet, Some("2 rule(s) the OS can't hold here".to_string())),
      2,
    )
    .unwrap();
    assert!(
      notice.contains("auto can't enforce")
        && notice.contains("2 rule(s) the OS can't hold here"),
      "{notice}"
    );
  }

  // the rules enforce would refuse to start over, each once
  #[test]
  fn fatal_gaps_are_the_fail_open_ones() {
    // a host name needs the broker on Linux: fail-open under Mode A
    let options = PermissionsOptions {
      allow_net: Some(vec!["example.com:443".to_string()]),
      ..Default::default()
    };
    let (compiled, ctx) = compiled_for(&options);
    let gaps = fatal_gaps(&compiled, &ctx);
    assert!(!gaps.is_empty());
    assert!(gaps.iter().all(|g| g.is_fatal_under_enforce()));
    let mut rules: Vec<_> = gaps.iter().map(|g| (&g.rule, &g.detail)).collect();
    let before = rules.len();
    rules.sort();
    rules.dedup();
    assert_eq!(rules.len(), before);
    // nothing granted: nothing to refuse to start over on a machine whose
    // mechanisms exist (the translation runs only on its own OS)
    let (compiled, _) = compiled_for(&PermissionsOptions::default());
    assert_eq!(compiled.fatal_gaps().count(), 0);
  }
}
