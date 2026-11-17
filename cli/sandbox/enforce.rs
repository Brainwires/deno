// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: applying the compiled kernel policy (`--kernel-sandbox=enforce`,
//! Mode A; DENEXT.md item 425).
//!
//! - [`plan_for`] translates the policy model's [`CompiledPolicy`] into the
//!   enforcers' [`EnforcePlan`] (Landlock rules with the read expansion on
//!   Linux, the Seatbelt profile's inputs on macOS). It adds no decision of
//!   its own; anything it can't write exactly is a fail-open gap, and
//!   `enforce` refuses to start.
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
use deno_runtime::deno_permissions::PermissionsOptions;
use deno_runtime::deno_permissions::kernel_sandbox_state;
use deno_sandbox::plan::EnforcePlan;
use deno_sandbox::plan::NetPlan;
use deno_sandbox::plan::PLAN_FORMAT;
use deno_sandbox::plan::Ports;

use super::inherit;
use super::policy::CompiledPolicy;
use super::policy::Context;
use super::policy::DenyEnforcement;
use super::policy::ExactWith;
use super::policy::GapClass;
use super::policy::GapResolution;
use super::policy::NetPolicy;
use super::policy::PathScope;
use super::policy::PolicyGap;
use super::policy::PortScope;
use super::policy::TargetOs;
use super::policy::resolve_prefix;

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
    env_keep: None,
    probe: None,
    module_files: Vec::new(),
    expanded: Vec::new(),
    config_env: Vec::new(),
    cache_overlay: ctx.cache_overlay.clone(),
  };
  match p.os {
    #[cfg(any(target_os = "linux", test))]
    TargetOs::Linux => match linux::plan(compiled) {
      Ok((linux, probe, expanded)) => {
        plan.linux = Some(linux);
        plan.probe = probe;
        plan.expanded = expanded;
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
    _ => {
      let _ = ctx;
      gaps.push(fatal("--kernel-sandbox", "no Mode A enforcer for this OS"));
      return (None, gaps);
    }
  }
  #[allow(unreachable_code, reason = "every arm returns on some targets")]
  (Some(plan), gaps)
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
    NetPolicy::Broker { .. } => {
      return Err(fatal(
        "--allow-net",
        "host-scoped network needs the broker (Mode B), which is not built in",
      ));
    }
  })
}

#[cfg(any(target_os = "linux", test))]
mod linux {
  use std::collections::BTreeMap;

  use deno_sandbox::plan::ExpandRoot;
  use deno_sandbox::plan::FsRule;
  use deno_sandbox::plan::Handled;
  use deno_sandbox::plan::LinuxPlan;
  use deno_sandbox::plan::SeccompPlan;

  use super::*;
  use crate::sandbox::policy::ExecScope;
  use crate::sandbox::policy::UnixPolicy;

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
  fn deny_paths(rules: &[crate::sandbox::policy::DenyRule]) -> Vec<PathBuf> {
    rules
      .iter()
      .filter(|r| r.enforcement == DenyEnforcement::Expand)
      .map(|r| resolve_prefix(&r.path).full())
      .collect()
  }

  /// The Linux plan, the probe path and the read expansion's entries.
  pub fn plan(
    compiled: &CompiledPolicy,
  ) -> Result<(LinuxPlan, Option<PathBuf>, Vec<PathBuf>), PolicyGap> {
    let p = &compiled.policy;
    for rule in p.fs.deny_read.iter().chain(&p.fs.deny_write) {
      match rule.enforcement {
        DenyEnforcement::Implied | DenyEnforcement::Expand => {}
        // a Deno-only deny carries a fail-open gap (or, for a read deny
        // under enforce, is expanded); anything else isn't Mode A
        DenyEnforcement::DenoOnly => {}
        other => {
          return Err(fatal(
            rule.path.display().to_string(),
            format!("a deny held as {other:?} has no Mode A enforcer"),
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
    match &p.fs.write {
      PathScope::All => {}
      PathScope::Paths { paths } => {
        handled.write = true;
        for rule in paths {
          add(&mut rules, rule.path.clone(), false, true);
        }
      }
    }
    // no run grant: nothing may execute; a strict scope: the listed files
    handled.exec = p.fs.exec != ExecScope::Unrestricted;
    let net = net_plan(&p.net)?;
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
      net,
      scope_signal: p.scopes.signal,
      scope_abstract_unix: p.scopes.abstract_unix,
      seccomp: SeccompPlan {
        attack_surface: p.syscalls.attack_surface,
        clone3_enosys: p.syscalls.clone3_enosys,
        refuse_exec: p.syscalls.refuse_exec,
        refuse_listen: p.syscalls.refuse_listen,
        refuse_io_uring: p.syscalls.refuse_io_uring,
        refuse_path_metadata: p.syscalls.refuse_path_metadata,
        refuse_path_truncate: p.syscalls.refuse_path_truncate,
        signal_self: p.syscalls.signal_self,
        unix_sockets: p.unix != UnixPolicy::Denied,
        netlink_route: p.sys.netlink_route,
        vsock: p.syscalls.vsock,
      },
    };
    Ok((plan, probe, entries))
  }
}

#[cfg(any(target_os = "macos", test))]
mod macos {
  use deno_sandbox::plan::MacosPlan;
  use deno_sandbox::plan::SbFilter;
  use deno_sandbox::plan::SbScope;
  use deno_sandbox::plan::SbUnix;
  use deno_sandbox::seatbelt::regex_escape;

  use super::*;
  use crate::sandbox::policy::ExecScope;
  use crate::sandbox::policy::UnixPolicy;

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
            == Some(crate::sandbox::policy::ImplicitNeed::ProjectConfig)
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
      .filter(|e| e.access == crate::sandbox::policy::ImplicitAccess::ListDir)
      .map(|e| e.path.clone())
      .collect();
    let read = scope(&p.fs.read, "read", &listed, &mut gaps);
    let write = scope(&p.fs.write, "write", &listed, &mut gaps);
    let map_exec = scope(&p.fs.map_exec, "load code", &listed, &mut gaps);
    let denies = |rules: &[crate::sandbox::policy::DenyRule],
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
    let inside = |rules: &[crate::sandbox::policy::DenyRule]| {
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
      },
      gaps,
    )
  }

  /// A path the profile must refuse to read: `/` when the read scope
  /// doesn't hold it (listing it needs `file-read-data`).
  pub fn probe(plan: &MacosPlan) -> Option<PathBuf> {
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
  if std::fs::create_dir_all(overlay).is_err() {
    return;
  }
  #[cfg(unix)]
  {
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

/// Whether macOS enforcement would use the `sandbox-exec` fallback (no
/// `sandbox_init`, or the test hook); always false elsewhere.
#[cfg(any(target_os = "macos", test))]
fn uses_sandbox_exec_fallback() -> bool {
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
      paths: vec![crate::sandbox::policy::PathRule {
        path: lock.clone(),
        implicit: Some(crate::sandbox::policy::ImplicitNeed::ProjectConfig),
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
