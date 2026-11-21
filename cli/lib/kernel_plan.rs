// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: the kernel policy's translation into the enforcers' plan
//! (DENEXT.md item 425), shared by the CLI (`--kernel-sandbox=enforce`) and
//! `denort` (a compiled program baked with `enforce`, DENEXT.md item 473).
//!
//! [`plan_for`] translates the policy model's [`CompiledPolicy`] into the
//! enforcers' [`EnforcePlan`] (Landlock rules with the read expansion on
//! Linux, the Seatbelt profile's inputs on macOS). It adds no decision of
//! its own; anything it can't write exactly is a fail-open gap, and
//! `enforce` refuses to start.

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

use deno_sandbox::plan::EnforcePlan;
use deno_sandbox::plan::NetPlan;
use deno_sandbox::plan::PLAN_FORMAT;
use deno_sandbox::plan::Ports;

use crate::kernel_policy::CompiledPolicy;
use crate::kernel_policy::Context;
use crate::kernel_policy::DenyEnforcement;
use crate::kernel_policy::ExactWith;
use crate::kernel_policy::GapClass;
use crate::kernel_policy::GapResolution;
use crate::kernel_policy::NetPolicy;
use crate::kernel_policy::PathScope;
use crate::kernel_policy::PolicyGap;
use crate::kernel_policy::PortScope;
use crate::kernel_policy::TargetOs;
use crate::kernel_policy::resolve_prefix;

/// How many Landlock rules a read expansion may produce before `enforce`
/// refuses to start (the walk is work at every start, and each entry is a
/// rule the kernel keeps).
#[cfg(any(target_os = "linux", test))]
pub const MAX_EXPANSION_RULES: usize = 20_000;
/// How deep a read expansion walks toward a denied path.
#[cfg(any(target_os = "linux", test))]
pub const MAX_EXPANSION_DEPTH: usize = 64;

/// The variables the re-executed image keeps beside the policy's list: the
/// runtime's own (internal descriptors, its configuration), never a secret
/// such as `DENO_AUTH_TOKENS`.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
const RUNTIME_INTERNAL_ENV: &[&str] = &[
  "NODE_CHANNEL_FD",
  "NODE_CHANNEL_SERIALIZATION_MODE",
  "DENO_DONT_USE_INTERNAL_NODE_COMPAT_STATE_FD",
  "DENO_NO_UPDATE_CHECK",
  "DENO_NO_PROMPT",
  "DENO_NO_DEPRECATION_WARNINGS",
  "DENO_TLS_CA_STORE",
  "DENO_CERT",
  "DENO_V8_FLAGS",
  "DENO_JOBS",
  "FORCE_COLOR",
  "CLICOLOR_FORCE",
];

/// The runtime configuration the re-executed image must see as the first
/// one did (registries, module resolution, unstable features, diagnostics,
/// telemetry): kept through the scrub, and their values recorded in the
/// plan, so a difference refuses to start instead of resolving differently
/// (as the config files are). Never a secret: no tokens, no telemetry
/// headers or client keys. A trailing `*` matches a prefix.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
const RUNTIME_CONFIG_ENV: &[&str] = &[
  "NPM_CONFIG_REGISTRY",
  "NPM_CONFIG_REPLACE_REGISTRY_HOST",
  "NPM_CONFIG_MIN_RELEASE_AGE",
  "NPM_CONFIG_TRUST_POLICY",
  "NPM_CONFIG_ADVISORY_CHECK",
  "JSR_URL",
  "JSR_NPM_URL",
  "DENO_NO_PACKAGE_JSON",
  "DENO_NO_SLOPPY_IMPORTS",
  "DENO_NO_DETECT_CJS",
  "DENO_NO_DENO_CONDITION",
  "DENO_NO_TSCONFIG_PATHS",
  "DENO_CONDITIONS",
  "DENO_COMPAT",
  "DENO_UNSTABLE_*",
  "DENO_EMIT_CACHE_MODE",
  "DENO_DISABLE_VERBATIM_MODULE_SYNTAX",
  // runtime behaviour switches the forced beds found scrubbed (item 451)
  "DENO_DISABLE_OFFSCREEN_CANVAS",
  "DENO_PATCH_REACT_CVE",
  "DENO_COVERAGE_DIR",
  "DENO_TRACE_PERMISSIONS",
  "DENO_AUDIT_PERMISSIONS",
  "DENO_KERNEL_SANDBOX",
  // the harnesses' forced enforce: both images decide the same mode
  "DENO_INTERNAL_KERNEL_SANDBOX_FORCE",
  // and the harnesses' rollout stage, for the same reason (item 472)
  "DENO_INTERNAL_KERNEL_SANDBOX_STAGE",
  "DENO_SERVE_ADDRESS",
  "DENO_AUTO_SERVE",
  "DENO_USE_CGROUPS",
  "DENO_TOKIO_*",
  "DENO_IDLE_TRIM_*",
  "DENO_USR2_MEMORY_TRIM",
  "DENO_NO_FILTER_FRAMES",
  "DENO_LOG",
  "DENO_LOG_STYLE",
  "NODE_OPTIONS",
  "NODE_EXTRA_CA_CERTS",
  // certificate locations (the policy reads them with network)
  "SSL_CERT_FILE",
  "SSL_CERT_DIR",
  "NODE_USE_ENV_PROXY",
  "RUST_LOG",
  "RUST_BACKTRACE",
  "OTEL_DENO*",
  "OTEL_SDK_DISABLED",
  "OTEL_PROPAGATORS",
  "OTEL_TRACES_SAMPLER",
  "OTEL_TRACES_SAMPLER_ARG",
  "OTEL_METRIC_EXPORT_INTERVAL",
  "OTEL_ATTRIBUTE_COUNT_LIMIT",
  "OTEL_ATTRIBUTE_VALUE_LENGTH_LIMIT",
  "OTEL_SPAN_ATTRIBUTE_COUNT_LIMIT",
  "OTEL_SPAN_ATTRIBUTE_VALUE_LENGTH_LIMIT",
  "OTEL_SPAN_EVENT_COUNT_LIMIT",
  "OTEL_EXPORTER_OTLP_ENDPOINT",
  "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
  "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
  "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
  "OTEL_EXPORTER_OTLP_PROTOCOL",
  "OTEL_EXPORTER_OTLP_TIMEOUT",
  "OTEL_EXPORTER_OTLP_COMPRESSION",
  "OTEL_EXPORTER_OTLP_INSECURE",
  "OTEL_EXPORTER_OTLP_CERTIFICATE",
  "OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE",
];

/// Whether `name` matches an entry of `list` (a trailing `*`: a prefix).
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn env_listed(list: &[&str], name: &str) -> bool {
  list.iter().any(|k| match k.strip_suffix('*') {
    Some(prefix) => name.starts_with(prefix),
    None => *k == name,
  })
}

/// The runtime configuration variables set in this process, sorted, for the
/// plan (both images compare them).
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn config_env() -> Vec<(String, String)> {
  #[allow(clippy::disallowed_methods, reason = "the process's own environment")]
  let mut out: Vec<(String, String)> = std::env::vars_os()
    .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
    .filter(|(k, _)| env_listed(RUNTIME_CONFIG_ENV, k))
    .collect();
  out.sort();
  out
}

/// The environment a scrubbed re-exec keeps: the policy's list plus the
/// runtime's own variables and its configuration.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn env_keep(scrub: &Option<Vec<String>>) -> Option<Vec<String>> {
  scrub.as_ref().map(|keep| {
    let mut keep: Vec<String> = keep
      .iter()
      .cloned()
      .chain(RUNTIME_INTERNAL_ENV.iter().map(|s| s.to_string()))
      .chain(RUNTIME_CONFIG_ENV.iter().map(|s| s.to_string()))
      .collect();
    keep.sort();
    keep.dedup();
    keep
  })
}

// ---------------------------------------------------------------------------
// Translation: CompiledPolicy -> EnforcePlan
// ---------------------------------------------------------------------------

fn fatal(rule: impl Into<String>, detail: impl Into<String>) -> PolicyGap {
  PolicyGap {
    class: GapClass::Enforcer,
    rule: rule.into(),
    detail: detail.into(),
    resolution: GapResolution::FailOpen,
    exact_with: Some(ExactWith::Never),
  }
}

/// The plan for `compiled` on this OS, and the gaps the translation adds
/// (a fail-open one means `enforce` refuses to start). Pure but for reading
/// the file system (the read expansion, macOS path canonicalisation).
pub fn plan_for(
  compiled: &CompiledPolicy,
  ctx: &Context,
) -> (Option<EnforcePlan>, Vec<PolicyGap>) {
  let p = &compiled.policy;
  let mut gaps = Vec::new();
  // filled in by the Linux and macOS arms only
  #[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", test)),
    allow(unused_mut, unused_variables, reason = "no Mode A enforcer here")
  )]
  let mut plan = EnforcePlan {
    format: PLAN_FORMAT,
    linux: None,
    macos: None,
    windows: None,
    env_keep: None,
    probe: None,
    module_files: Vec::new(),
    expanded: Vec::new(),
    config_env: Vec::new(),
    cache_overlay: ctx.cache_overlay.clone(),
    broker: None,
    expanded_write: Vec::new(),
    entries: Vec::new(),
  };
  match p.os {
    #[cfg(any(target_os = "linux", test))]
    TargetOs::Linux => match linux::plan(compiled) {
      Ok((linux, measured)) => {
        plan.linux = Some(linux);
        plan.probe = measured.probe;
        plan.expanded = measured.expanded;
        plan.expanded_write = measured.expanded_write;
        plan.env_keep = env_keep(&p.env_scrub);
        plan.config_env = config_env();
      }
      Err(gap) => {
        gaps.push(gap);
        return (None, gaps);
      }
    },
    #[cfg(any(target_os = "macos", test))]
    TargetOs::Macos => {
      let (macos, more) = macos::plan(compiled, ctx);
      gaps.extend(more);
      if uses_sandbox_exec_fallback() && !macos.exec {
        gaps.push(PolicyGap {
          class: GapClass::Run,
          rule: "--kernel-sandbox=enforce (sandbox-exec fallback)".to_string(),
          detail: "documented: sandbox_init isn't available here, so the runtime re-executes itself under sandbox-exec, whose profile must let it exec its own binary; native code without --allow-run can start this runtime again, which runs under the same profile".to_string(),
          resolution: GapResolution::Documented,
          exact_with: Some(ExactWith::Never),
        });
      }
      plan.probe = macos::probe(&macos);
      plan.macos = Some(macos);
      plan.env_keep = env_keep(&p.env_scrub);
      plan.config_env = config_env();
    }
    #[cfg(any(windows, test))]
    TargetOs::Windows => {
      let _ = ctx;
      plan.windows = Some(windows::plan(compiled));
    }
    _ => {
      let _ = ctx;
      gaps.push(fatal("--kernel-sandbox", "no Mode A enforcer for this OS"));
      return (None, gaps);
    }
  }
  // Mode B: the broker's copy of the run's own read and write rules
  #[allow(unreachable_code, reason = "every arm returns on some targets")]
  if p.shape == crate::kernel_policy::Shape::ModeB {
    plan.broker = Some(broker_plan(compiled));
  }
  #[allow(unreachable_code, reason = "every arm returns on some targets")]
  (Some(plan), gaps)
}

/// What the supervising broker holds for `compiled`: the user's read and
/// write grants (never the runtime's implicit set, which Deno's checks
/// don't grant either) and every deny, each as written and resolved.
fn broker_plan(compiled: &CompiledPolicy) -> deno_sandbox::plan::BrokerPlan {
  fn both(path: &Path, out: &mut Vec<PathBuf>) {
    let resolved = resolve_prefix(path).full();
    out.push(path.to_path_buf());
    if resolved != path {
      out.push(resolved);
    }
  }
  fn scope(scope: &PathScope) -> Option<Vec<PathBuf>> {
    match scope {
      PathScope::All => None,
      PathScope::Paths { paths } => {
        let mut out = Vec::new();
        for rule in paths.iter().filter(|r| r.implicit.is_none()) {
          both(&rule.path, &mut out);
        }
        out.sort();
        out.dedup();
        Some(out)
      }
    }
  }
  fn denies(rules: &[crate::kernel_policy::DenyRule]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for rule in rules {
      both(&rule.path, &mut out);
    }
    out.sort();
    out.dedup();
    out
  }
  let fs = &compiled.policy.fs;
  deno_sandbox::plan::BrokerPlan {
    read: scope(&fs.read),
    deny_read: denies(&fs.deny_read),
    write: scope(&fs.write),
    deny_write: denies(&fs.deny_write),
    // filled in by `prepare`, which has the flags' lists
    net: None,
  }
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn net_plan(net: &NetPolicy) -> Result<NetPlan, PolicyGap> {
  let ports = |scope: &PortScope| match scope {
    PortScope::Any => Ports::Any,
    PortScope::None => Ports::List { ports: Vec::new() },
    PortScope::Ports { ports } => Ports::List {
      ports: ports.clone(),
    },
  };
  Ok(match net {
    NetPolicy::Denied => NetPlan::Denied,
    NetPolicy::Open => NetPlan::Open,
    NetPolicy::Ports { connect, bind } => NetPlan::Ports {
      connect: ports(connect),
      bind: ports(bind),
    },
    // the broker holds the network: the confined process has no IP
    // socket of its own (its sockets come from the broker, connected or
    // listening)
    NetPolicy::Broker { .. } if crate::kernel_policy::BROKER_NET_BUILT => {
      NetPlan::Denied
    }
    NetPolicy::Broker { .. } => {
      return Err(fatal(
        "--allow-net",
        "host-scoped network needs the broker's network side (Mode B), which is not built in",
      ));
    }
  })
}

#[cfg(any(target_os = "linux", test))]
pub mod linux {
  use std::collections::BTreeMap;

  use deno_sandbox::plan::ExpandRoot;
  use deno_sandbox::plan::FsRule;
  use deno_sandbox::plan::Handled;
  use deno_sandbox::plan::LinuxPlan;
  use deno_sandbox::plan::SeccompPlan;

  use super::*;
  use crate::kernel_policy::ExecScope;
  use crate::kernel_policy::UnixPolicy;

  /// The read expansion: every entry of `root` on the way down
  /// to a denied path is allowed, but not the denied path or the
  /// directories that lead to it. Symlinks get no rule (a rule follows the
  /// link, so it would grant wherever the link leads, inside the root or
  /// not); one leading to an allowed entry still works. Fails (enforce
  /// refuses) past
  /// [`MAX_EXPANSION_RULES`] rules or [`MAX_EXPANSION_DEPTH`] levels; never
  /// widens.
  pub fn expand_read(
    root: &Path,
    denies: &[PathBuf],
    out: &mut Vec<PathBuf>,
  ) -> Result<(), String> {
    fn walk(
      dir: &Path,
      denies: &[PathBuf],
      depth: usize,
      out: &mut Vec<PathBuf>,
    ) -> Result<(), String> {
      if denies.iter().any(|d| d == dir) {
        return Ok(());
      }
      if !denies.iter().any(|d| d.starts_with(dir)) {
        out.push(dir.to_path_buf());
        return check_size(out);
      }
      if depth > MAX_EXPANSION_DEPTH {
        return Err(format!(
          "a denied path lies more than {MAX_EXPANSION_DEPTH} levels below the allowed folder it is in"
        ));
      }
      // unreadable or gone: nothing to allow there (fails closed)
      let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
      };
      for entry in entries.flatten() {
        let path = entry.path();
        let is_symlink =
          entry.file_type().map(|t| t.is_symlink()).unwrap_or(true);
        if !is_symlink {
          walk(&path, denies, depth + 1, out)?;
        }
      }
      Ok(())
    }
    fn check_size(out: &[PathBuf]) -> Result<(), String> {
      if out.len() > MAX_EXPANSION_RULES {
        return Err(format!(
          "holding it needs more than {MAX_EXPANSION_RULES} Landlock rules"
        ));
      }
      Ok(())
    }
    walk(root, denies, 0, out)
  }

  /// The expanded denies, resolved: the part that exists canonical, then
  /// the rest (a deny that doesn't exist yet behind a symlinked folder
  /// names the real folder, which the walk must then not allow whole).
  fn deny_paths(rules: &[crate::kernel_policy::DenyRule]) -> Vec<PathBuf> {
    rules
      .iter()
      .filter(|r| r.enforcement == DenyEnforcement::Expand)
      .map(|r| resolve_prefix(&r.path).full())
      .collect()
  }

  /// What [`plan`] returns beside the plan itself.
  pub struct Measured {
    pub probe: Option<PathBuf>,
    /// The read expansion's entries.
    pub expanded: Vec<PathBuf>,
    /// The write expansion's entries.
    pub expanded_write: Vec<PathBuf>,
  }

  /// The write denies the broker holds, resolved like the expanded read
  /// denies.
  fn broker_deny_paths(
    rules: &[crate::kernel_policy::DenyRule],
  ) -> Vec<PathBuf> {
    rules
      .iter()
      .filter(|r| r.enforcement == DenyEnforcement::Broker)
      .map(|r| resolve_prefix(&r.path).full())
      .collect()
  }

  /// The Linux plan, the probe path and the expansions' entries.
  pub fn plan(
    compiled: &CompiledPolicy,
  ) -> Result<(LinuxPlan, Measured), PolicyGap> {
    let p = &compiled.policy;
    // a deny ceiling holds only what an overmount holds on Linux (no
    // no-new-privileges, no seccomp list): Mode A's Landlock plan can't
    // carry it, so one that holds nothing is never applied (`check_run`
    // turns it off first) and anything else isn't built
    if p.deny_ceiling {
      return Err(fatal(
        "--deny-* (deny ceiling)",
        "Linux holds a deny ceiling only by overmounting, which this runtime doesn't do",
      ));
    }
    for rule in &p.fs.deny_read {
      match rule.enforcement {
        DenyEnforcement::Implied | DenyEnforcement::Expand => {}
        // a Deno-only deny carries a fail-open gap (or, for a read deny
        // under enforce, is expanded); anything else isn't Mode A
        DenyEnforcement::DenoOnly => {}
        other => {
          return Err(fatal(
            rule.path.display().to_string(),
            format!("a read deny held as {other:?} has no enforcer here"),
          ));
        }
      }
    }
    for rule in &p.fs.deny_write {
      match rule.enforcement {
        DenyEnforcement::Implied | DenyEnforcement::DenoOnly => {}
        // under the broker: the allow is expanded for writes (the kernel
        // refuses changes directly in the folders that lead to the deny)
        // and the broker makes those changes, holding the deny exactly
        DenyEnforcement::Broker
          if p.shape == crate::kernel_policy::Shape::ModeB => {}
        other => {
          return Err(fatal(
            rule.path.display().to_string(),
            format!("a write deny held as {other:?} has no enforcer here"),
          ));
        }
      }
    }
    let expand = deny_paths(&p.fs.deny_read);
    let mut rules: BTreeMap<PathBuf, (bool, bool)> = BTreeMap::new();
    let mut handled = Handled::default();
    let add = |rules: &mut BTreeMap<PathBuf, (bool, bool)>,
               path: PathBuf,
               read: bool,
               write: bool| {
      let entry = rules.entry(path).or_insert((false, false));
      entry.0 |= read;
      entry.1 |= write;
    };
    // each expanded folder: its root and denies in the plan (both images
    // compare them), its walked entries in the measured list
    let mut roots: Vec<ExpandRoot> = Vec::new();
    let mut entries: Vec<PathBuf> = Vec::new();
    let mut expand_root = |root: &Path| -> Result<(), PolicyGap> {
      let canonical = resolve_prefix(root).full();
      let deny: Vec<PathBuf> = expand
        .iter()
        .filter(|d| d.starts_with(&canonical))
        .cloned()
        .collect();
      expand_read(&canonical, &deny, &mut entries).map_err(|why| {
        fatal(
          format!("--deny-read inside {}", root.display()),
          format!("can't expand the allow to hold it: {why}"),
        )
      })?;
      roots.push(ExpandRoot {
        root: canonical,
        deny,
      });
      Ok(())
    };
    match &p.fs.read {
      PathScope::All => {
        if !expand.is_empty() {
          handled.read = true;
          expand_root(Path::new("/"))?;
        }
      }
      PathScope::Paths { paths } => {
        handled.read = true;
        for rule in paths {
          let canonical = resolve_prefix(&rule.path).full();
          if expand.iter().any(|d| d.starts_with(&canonical)) {
            expand_root(&rule.path)?;
          } else {
            add(&mut rules, rule.path.clone(), true, false);
          }
        }
      }
    }
    roots.sort();
    roots.dedup();
    entries.sort();
    entries.dedup();
    // the new image applies the expanded rules with openat2: where it is
    // refused (an outer seccomp profile) they would all be missing
    #[cfg(target_os = "linux")]
    if !entries.is_empty()
      && let Err(why) = deno_sandbox::enforce_linux::openat2_available()
    {
      return Err(fatal(
        "--deny-read",
        format!("the read expansion that holds it can't be applied: {why}"),
      ));
    }
    if entries.len() > deno_sandbox::plan::MAX_EXPANDED {
      return Err(fatal(
        "--deny-read",
        format!(
          "holding the read denies needs more than {} Landlock rules",
          deno_sandbox::plan::MAX_EXPANDED
        ),
      ));
    }
    // the write expansion, for the denies the broker holds: the same walk
    // as the read expansion, with write rights on what it allows
    let broker_denies = broker_deny_paths(&p.fs.deny_write);
    let mut write_roots: Vec<ExpandRoot> = Vec::new();
    let mut write_entries: Vec<PathBuf> = Vec::new();
    let mut expand_write_root = |root: &Path| -> Result<(), PolicyGap> {
      let canonical = resolve_prefix(root).full();
      let deny: Vec<PathBuf> = broker_denies
        .iter()
        .filter(|d| d.starts_with(&canonical))
        .cloned()
        .collect();
      expand_read(&canonical, &deny, &mut write_entries).map_err(|why| {
        fatal(
          format!("--deny-write inside {}", root.display()),
          format!("can't expand the allow to hold it: {why}"),
        )
      })?;
      write_roots.push(ExpandRoot {
        root: canonical,
        deny,
      });
      Ok(())
    };
    match &p.fs.write {
      PathScope::All => {
        if !broker_denies.is_empty() {
          handled.write = true;
          expand_write_root(Path::new("/"))?;
        }
      }
      PathScope::Paths { paths } => {
        handled.write = true;
        for rule in paths {
          let canonical = resolve_prefix(&rule.path).full();
          if broker_denies.iter().any(|d| d.starts_with(&canonical)) {
            expand_write_root(&rule.path)?;
          } else {
            add(&mut rules, rule.path.clone(), false, true);
          }
        }
      }
    }
    write_roots.sort();
    write_roots.dedup();
    write_entries.sort();
    write_entries.dedup();
    #[cfg(target_os = "linux")]
    if !write_entries.is_empty()
      && let Err(why) = deno_sandbox::enforce_linux::openat2_available()
    {
      return Err(fatal(
        "--deny-write",
        format!("the write expansion that holds it can't be applied: {why}"),
      ));
    }
    if write_entries.len() > deno_sandbox::plan::MAX_EXPANDED {
      return Err(fatal(
        "--deny-write",
        format!(
          "holding the write denies needs more than {} Landlock rules",
          deno_sandbox::plan::MAX_EXPANDED
        ),
      ));
    }
    // no run grant: nothing may execute; a strict scope: the listed files
    handled.exec = p.fs.exec != ExecScope::Unrestricted;
    let net = net_plan(&p.net)?;
    let broker_net = matches!(p.net, NetPolicy::Broker { .. });
    let probe = (handled.read && !rules.contains_key(Path::new("/")))
      .then(|| PathBuf::from("/"));
    let plan = LinuxPlan {
      handled,
      rules: rules
        .into_iter()
        .map(|(path, (read, write))| FsRule {
          path,
          read,
          write,
          exec: false,
        })
        .chain(p.fs.exec_programs.iter().map(|path| FsRule {
          path: path.clone(),
          read: false,
          write: false,
          exec: true,
        }))
        .collect(),
      expand: roots,
      expand_write: write_roots,
      net,
      prebound: p.prebound.clone(),
      scope_signal: p.scopes.signal,
      scope_abstract_unix: p.scopes.abstract_unix,
      seccomp: SeccompPlan {
        attack_surface: p.syscalls.attack_surface,
        clone3_enosys: p.syscalls.clone3_enosys,
        refuse_exec: p.syscalls.refuse_exec,
        // under a broker that holds the network the broker listens and
        // connects: the confined tree does neither
        refuse_listen: p.syscalls.refuse_listen || broker_net,
        refuse_connect: broker_net,
        refuse_io_uring: p.syscalls.refuse_io_uring,
        refuse_path_metadata: p.syscalls.refuse_path_metadata,
        refuse_path_truncate: p.syscalls.refuse_path_truncate,
        signal_self: p.syscalls.signal_self,
        unix_sockets: p.unix != UnixPolicy::Denied,
        netlink_route: p.sys.netlink_route,
        vsock: p.syscalls.vsock,
      },
    };
    Ok((
      plan,
      Measured {
        probe,
        expanded: entries,
        expanded_write: write_entries,
      },
    ))
  }
}

/// Windows, the small tier (DENEXT.md item 474): the job object's process
/// limit and the write roots a lowered integrity level needs labelled.
/// Reads and the network aren't in the plan (the model reports them as
/// documented Deno-only rules).
#[cfg(any(windows, test))]
pub mod windows {
  use deno_sandbox::plan::WindowsPlan;

  use super::*;
  use crate::kernel_policy::ExecScope;

  pub fn plan(compiled: &CompiledPolicy) -> WindowsPlan {
    let p = &compiled.policy;
    WindowsPlan {
      no_children: p.fs.exec == ExecScope::None,
      write_roots: match &p.fs.write {
        PathScope::All => None,
        PathScope::Paths { paths } => {
          let mut roots: Vec<PathBuf> =
            paths.iter().map(|rule| rule.path.clone()).collect();
          roots.sort();
          roots.dedup();
          Some(roots)
        }
      },
    }
  }
}

#[cfg(any(target_os = "macos", test))]
pub mod macos {
  use deno_sandbox::plan::MacosPlan;
  use deno_sandbox::plan::SbFilter;
  use deno_sandbox::plan::SbScope;
  use deno_sandbox::plan::SbUnix;
  use deno_sandbox::seatbelt::regex_escape;

  use super::*;
  use crate::kernel_policy::ExecScope;
  use crate::kernel_policy::UnixPolicy;

  /// The existing part of `path`, canonical, and the rest (components that
  /// don't exist yet).
  fn canonical_prefix(path: &Path) -> (PathBuf, PathBuf) {
    let r = resolve_prefix(path);
    (r.existing, r.rest)
  }

  /// Whether the volume holding `path` matches names case-insensitively
  /// (`None`: unknown; treated as insensitive).
  #[cfg(target_os = "macos")]
  fn case_insensitive(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
      return true;
    };
    // SAFETY: a valid path; _PC_CASE_SENSITIVE answers 0 or 1
    let res = unsafe { libc::pathconf(c.as_ptr(), libc::_PC_CASE_SENSITIVE) };
    res != 1
  }

  #[cfg(not(target_os = "macos"))]
  fn case_insensitive(_path: &Path) -> bool {
    false
  }

  /// An allow's filter: the canonical path (Seatbelt checks resolved
  /// paths), a subpath for a folder or a path that doesn't exist yet, a
  /// literal for a file. `None` for a path that isn't UTF-8 (an allow that
  /// can't be written fails closed).
  pub fn allow_filter(path: &Path) -> Option<SbFilter> {
    // never `prefix.join(rest)` with an empty rest: that appends a separator
    // ("/a/"), which a literal never matches
    let resolved = resolve_prefix(path);
    let full = resolved.full();
    let text = full.to_str()?.to_string();
    if resolved.rest.as_os_str().is_empty() && full.is_file() {
      Some(SbFilter::Literal(text))
    } else {
      Some(SbFilter::Subpath(text))
    }
  }

  /// A deny's filter, written so it can't miss: the canonical spelling
  /// (on-disk case) of the part that exists, and for the part that doesn't
  /// exist yet on a case-insensitive volume a case-folded regex. `Err`: it
  /// can't be written exactly (a fail-open gap).
  pub fn deny_filter(path: &Path) -> Result<SbFilter, String> {
    let (prefix, rest) = canonical_prefix(path);
    let Some(prefix_text) = prefix.to_str() else {
      return Err("the path is not UTF-8".to_string());
    };
    if rest.as_os_str().is_empty() {
      return Ok(SbFilter::Subpath(prefix_text.to_string()));
    }
    let Some(rest_text) = rest.to_str() else {
      return Err("the path is not UTF-8".to_string());
    };
    if !case_insensitive(&prefix) {
      return Ok(SbFilter::Subpath(
        prefix
          .join(&rest)
          .to_str()
          .unwrap_or(prefix_text)
          .to_string(),
      ));
    }
    let mut regex =
      format!("^{}", regex_escape(prefix_text.trim_end_matches('/')));
    for part in rest_text.split('/').filter(|p| !p.is_empty()) {
      regex.push('/');
      for c in part.chars() {
        if c.is_ascii_alphabetic() {
          regex.push_str(&format!(
            "[{}{}]",
            c.to_ascii_lowercase(),
            c.to_ascii_uppercase()
          ));
        } else if !c.is_ascii() && c.is_alphabetic() {
          return Err(
            "a part that doesn't exist yet has non-ASCII letters, and the volume ignores case".to_string(),
          );
        } else {
          regex.push_str(&regex_escape(&c.to_string()));
        }
      }
    }
    regex.push_str("(/|$)");
    Ok(SbFilter::Regex(regex))
  }

  pub fn scope(
    scope: &PathScope,
    what: &str,
    listed: &[PathBuf],
    gaps: &mut Vec<PolicyGap>,
  ) -> SbScope {
    match scope {
      PathScope::All => SbScope::All,
      PathScope::Paths { paths } => {
        let mut filters = Vec::new();
        for rule in paths {
          let filter = if rule.implicit.is_some() && listed.contains(&rule.path)
          {
            // a listed folder (ImplicitAccess::ListDir): the folder itself,
            // never what is beneath it
            resolve_prefix(&rule.path)
              .full()
              .to_str()
              .map(|p| SbFilter::Literal(p.to_string()))
          } else if rule.implicit
            == Some(crate::kernel_policy::ImplicitNeed::ProjectConfig)
          {
            // a config file or the lockfile: a file whether or not it exists
            // yet (the first image writes a new lockfile before the re-exec,
            // item 451), so both images write the same filter
            resolve_prefix(&rule.path)
              .full()
              .to_str()
              .map(|p| SbFilter::Literal(p.to_string()))
          } else {
            allow_filter(&rule.path)
          };
          match filter {
            Some(f) => filters.push(f),
            None => gaps.push(PolicyGap {
              class: GapClass::Read,
              rule: format!("{what} {}", rule.path.display()),
              detail: "fail closed: Seatbelt profiles take UTF-8 paths; this one is left out".to_string(),
              resolution: GapResolution::FailClosed,
              exact_with: Some(ExactWith::Never),
            }),
          }
        }
        filters.sort();
        filters.dedup();
        SbScope::Filters { filters }
      }
    }
  }

  pub fn plan(
    compiled: &CompiledPolicy,
    ctx: &Context,
  ) -> (MacosPlan, Vec<PolicyGap>) {
    let p = &compiled.policy;
    let mut gaps = Vec::new();
    // the implicit folders that are listed, never read beneath
    let listed: Vec<PathBuf> = compiled
      .implicit
      .entries
      .iter()
      .filter(|e| e.access == crate::kernel_policy::ImplicitAccess::ListDir)
      .map(|e| e.path.clone())
      .collect();
    let read = scope(&p.fs.read, "read", &listed, &mut gaps);
    let write = scope(&p.fs.write, "write", &listed, &mut gaps);
    let map_exec = scope(&p.fs.map_exec, "load code", &listed, &mut gaps);
    let denies = |rules: &[crate::kernel_policy::DenyRule],
                  flag: &str,
                  gaps: &mut Vec<PolicyGap>| {
      let mut out = Vec::new();
      for rule in rules {
        if rule.enforcement != DenyEnforcement::Exact {
          continue;
        }
        match deny_filter(&rule.path) {
          Ok(f) => out.push(f),
          Err(why) => gaps.push(PolicyGap {
            class: GapClass::Read,
            rule: format!("{flag}={}", rule.path.display()),
            detail: format!(
              "Deno only: the Seatbelt rule can't be written exactly ({why})"
            ),
            resolution: GapResolution::FailOpen,
            exact_with: Some(ExactWith::Never),
          }),
        }
      }
      out.sort();
      out.dedup();
      out
    };
    let deny_read = denies(&p.fs.deny_read, "--deny-read", &mut gaps);
    let deny_write = denies(&p.fs.deny_write, "--deny-write", &mut gaps);
    // implicit entries inside a user's deny (each already a gap)
    let inside = |rules: &[crate::kernel_policy::DenyRule]| {
      let mut out: Vec<SbFilter> = compiled
        .implicit
        .entries
        .iter()
        .filter(|e| rules.iter().any(|r| e.path.starts_with(&r.path)))
        .filter_map(|e| allow_filter(&e.path))
        .collect();
      out.sort();
      out.dedup();
      out
    };
    let reallow_read = inside(&p.fs.deny_read);
    let reallow_write = inside(&p.fs.deny_write)
      .into_iter()
      .filter(|f| match &write {
        SbScope::All => true,
        SbScope::Filters { filters } => filters.contains(f),
      })
      .collect();
    let net = match net_plan(&p.net) {
      Ok(net) => net,
      Err(gap) => {
        gaps.push(gap);
        NetPlan::Denied
      }
    };
    let unix = match &p.unix {
      UnixPolicy::Denied => SbUnix::Denied,
      UnixPolicy::Open => SbUnix::Open,
      UnixPolicy::Paths { paths } => SbUnix::Filters {
        filters: paths.iter().filter_map(|p| allow_filter(p)).collect(),
      },
    };
    #[allow(clippy::disallowed_methods, reason = "the process's own setting")]
    let system_ca = std::env::var("DENO_TLS_CA_STORE")
      .is_ok_and(|v| v.split(',').any(|s| s.trim() == "system"));
    (
      MacosPlan {
        read,
        write,
        deny_read,
        deny_write,
        reallow_read,
        reallow_write,
        map_exec,
        exec: p.fs.exec != ExecScope::None,
        exec_only: if p.fs.exec == ExecScope::Listed {
          p.fs
            .exec_programs
            .iter()
            .filter_map(|p| p.to_str().map(str::to_string))
            .collect()
        } else {
          Vec::new()
        },
        net,
        unix,
        denied_sysctls: p.sys.denied_sysctls.clone(),
        system_ca,
        ttys: {
          // the controlling terminal and the standard streams' own, never
          // every terminal
          let mut ttys: Vec<String> = ctx
            .tty_parent
            .then(|| "/dev/tty".to_string())
            .into_iter()
            .chain(ctx.ttys.iter().filter_map(|t| {
              resolve_prefix(t).full().to_str().map(|s| s.to_string())
            }))
            .collect();
          ttys.sort();
          ttys.dedup();
          ttys
        },
        deny_only: p.deny_ceiling,
      },
      gaps,
    )
  }

  /// A path the profile must refuse to read: `/` when the read scope
  /// doesn't hold it (listing it needs `file-read-data`).
  pub fn probe(plan: &MacosPlan) -> Option<PathBuf> {
    // a deny ceiling reads everything else: its first read deny is the
    // path the profile must refuse
    if plan.deny_only {
      return plan.deny_read.iter().find_map(|f| match f {
        SbFilter::Subpath(p) | SbFilter::Literal(p) => Some(PathBuf::from(p)),
        SbFilter::Regex(_) => None,
      });
    }
    let SbScope::Filters { filters } = &plan.read else {
      return None;
    };
    let covered = |candidate: &str| {
      filters.iter().any(|f| match f {
        SbFilter::Literal(p) => p == candidate,
        SbFilter::Subpath(p) => Path::new(candidate).starts_with(p),
        // a regex can't be judged here: treat it as covering
        SbFilter::Regex(_) => true,
      })
    };
    // a folder that exists on every Mac, listing it needs file-read-data
    ["/", "/Library", "/private/var/root", "/Users"]
      .into_iter()
      .find(|c| !covered(c) && Path::new(c).is_dir())
      .map(PathBuf::from)
  }
}

/// Whether macOS enforcement would use the `sandbox-exec` fallback (no
/// `sandbox_init`, or the test hook); always false elsewhere.
#[cfg(any(target_os = "macos", test))]
pub fn uses_sandbox_exec_fallback() -> bool {
  #[cfg(target_os = "macos")]
  {
    !deno_sandbox::macos::sandbox_init_available() || force_sandbox_exec()
  }
  #[cfg(not(target_os = "macos"))]
  {
    false
  }
}

/// The gaps the OS translation adds, for `--kernel-sandbox-report` (macOS:
/// deny paths that can't be written exactly, the `sandbox-exec` fallback);
/// none elsewhere (Linux's translation gaps only ever refuse to start, which
/// `enforce` prints).
pub fn report_gaps(compiled: &CompiledPolicy, ctx: &Context) -> Vec<PolicyGap> {
  if compiled.policy.os != TargetOs::Macos {
    return Vec::new();
  }
  plan_for(compiled, ctx).1
}

/// macOS test hook, in debug builds only (release builds never read it):
/// use the `sandbox-exec` fallback even where `sandbox_init` exists (it
/// enforces the same profile, plus the fallback's own self-exec allowance).
#[cfg(all(target_os = "macos", debug_assertions))]
fn force_sandbox_exec() -> bool {
  #[allow(clippy::disallowed_methods, reason = "a test hook")]
  std::env::var_os("DENO_INTERNAL_KERNEL_SANDBOX_EXEC").is_some()
}

#[cfg(all(target_os = "macos", not(debug_assertions)))]
fn force_sandbox_exec() -> bool {
  false
}

#[cfg(test)]
mod tests {
  use super::*;

  fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
      .join(format!("deno_kernel_enforce_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
  }

  #[test]
  fn expansion_allows_all_but_the_denied_path() {
    let root = scratch("expand");
    std::fs::create_dir_all(root.join("a/b")).unwrap();
    for f in ["a/b/.env", "a/b/main.ts", "a/x", "top"] {
      std::fs::write(root.join(f), "").unwrap();
    }
    #[cfg(unix)]
    {
      std::os::unix::fs::symlink(root.join("a/b/.env"), root.join("link"))
        .unwrap();
      std::os::unix::fs::symlink(root.join("top"), root.join("ok-link"))
        .unwrap();
      std::os::unix::fs::symlink("/etc", root.join("out-link")).unwrap();
    }
    let deny = vec![root.join("a/b/.env")];
    let mut out = Vec::new();
    linux::expand_read(&root, &deny, &mut out).unwrap();
    for allowed in ["a/b/main.ts", "a/x", "top"] {
      assert!(out.contains(&root.join(allowed)), "{allowed}: {out:?}");
    }
    // not the denied file, nor a folder holding it, nor a link to it
    assert!(out.iter().all(|p| !deny[0].starts_with(p)), "{out:?}");
    // no symlink gets a rule: a rule follows the link, so one to /etc would
    // grant /etc (one to an allowed entry still works through its target)
    #[cfg(unix)]
    for link in ["link", "ok-link", "out-link"] {
      assert!(!out.contains(&root.join(link)), "{link}: {out:?}");
    }
    std::fs::remove_dir_all(&root).unwrap();
  }

  #[test]
  fn expansion_refuses_past_its_bounds() {
    let root = scratch("expand_bounds");
    let mut deep = root.clone();
    for _ in 0..MAX_EXPANSION_DEPTH + 2 {
      deep.push("d");
    }
    std::fs::create_dir_all(&deep).unwrap();
    let mut out = Vec::new();
    let err =
      linux::expand_read(&root, &[deep.join("secret")], &mut out).unwrap_err();
    assert!(err.contains("levels"), "{err}");
    let wide = scratch("expand_wide");
    std::fs::create_dir(wide.join("d")).unwrap();
    for i in 0..MAX_EXPANSION_RULES + 1 {
      std::fs::write(wide.join(format!("f{i}")), "").unwrap();
    }
    let mut out = Vec::new();
    let err = linux::expand_read(&wide, &[wide.join("d/secret")], &mut out)
      .unwrap_err();
    assert!(err.contains("rules"), "{err}");
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::remove_dir_all(&wide).unwrap();
  }

  // an existing folder or file is written without a trailing separator (a
  // literal with one never matches: the working directory's own listing,
  // which resolving it needs)
  #[test]
  fn allow_filters_name_the_path_itself() {
    let root = scratch("allow_filter");
    std::fs::write(root.join("f.txt"), "").unwrap();
    let text = |p: &Path| p.to_str().unwrap().to_string();
    assert_eq!(
      macos::allow_filter(&root),
      Some(deno_sandbox::plan::SbFilter::Subpath(text(&root)))
    );
    assert_eq!(
      macos::allow_filter(&root.join("f.txt")),
      Some(deno_sandbox::plan::SbFilter::Literal(text(
        &root.join("f.txt")
      )))
    );
    std::fs::remove_dir_all(&root).unwrap();
  }

  #[test]
  fn deny_filters_never_miss() {
    let root = scratch("deny_filter");
    std::fs::write(root.join("Secret"), "").unwrap();
    // an existing path: its canonical spelling, as a subpath
    let f = macos::deny_filter(&root.join("Secret")).unwrap();
    assert_eq!(
      f,
      deno_sandbox::plan::SbFilter::Subpath(
        root.join("Secret").to_str().unwrap().to_string()
      )
    );
    // a path that doesn't exist yet: a subpath on a case-sensitive volume,
    // a case-folded regex on a case-insensitive one (macOS's default)
    match macos::deny_filter(&root.join("later/x")).unwrap() {
      // compared as paths: Windows spells the separator differently
      deno_sandbox::plan::SbFilter::Subpath(p) => {
        assert!(Path::new(&p).ends_with(Path::new("later").join("x")), "{p}")
      }
      deno_sandbox::plan::SbFilter::Regex(r) => {
        assert!(r.ends_with("/[lL][aA][tT][eE][rR]/[xX](/|$)"), "{r}")
      }
      other => panic!("{other:?}"),
    }
    std::fs::remove_dir_all(&root).unwrap();
  }

  /// Item 451: a project config file (the lockfile the first image may
  /// create before the re-exec) is the same literal filter before and after
  /// it exists, so both images write the same macOS plan.
  #[test]
  fn config_files_filter_the_same_before_they_exist() {
    let root = scratch("config_literal");
    let lock = root.join("deno.lock");
    let rules = PathScope::Paths {
      paths: vec![crate::kernel_policy::PathRule {
        path: lock.clone(),
        implicit: Some(crate::kernel_policy::ImplicitNeed::ProjectConfig),
      }],
    };
    let mut gaps = Vec::new();
    let before = macos::scope(&rules, "read", &[], &mut gaps);
    std::fs::write(&lock, "{}").unwrap();
    let after = macos::scope(&rules, "read", &[], &mut gaps);
    assert_eq!(before, after);
    assert!(gaps.is_empty());
    match after {
      deno_sandbox::plan::SbScope::Filters { filters } => assert!(matches!(
        &filters[..],
        [deno_sandbox::plan::SbFilter::Literal(p)] if p.ends_with("deno.lock")
      )),
      other => panic!("{other:?}"),
    }
    std::fs::remove_dir_all(&root).unwrap();
  }
}
