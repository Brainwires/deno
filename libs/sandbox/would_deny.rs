// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: the kernel sandbox's warn mode: one event for "the sandbox would
//! refuse this", one place it is reported.
//!
//! Under `--kernel-sandbox=warn` nothing is refused. Every part of the
//! sandbox that would refuse something under `enforce` builds a
//! [`WouldDeny`] and hands it to [`report`] instead, then lets the access go
//! ahead as Deno's own checks decide:
//!
//! - the policy compiler, at startup: a rule the OS can't hold, over which
//!   `enforce` would refuse to start ([`Source::Policy`]);
//! - a decision point in the runtime or its supervisor: the broker's file
//!   and network requests, the network connector, a desktop host
//!   ([`Source::Runtime`]);
//! - the kernel's own record of what the applied policy would have refused,
//!   read back after the run (macOS: Seatbelt's report lines,
//!   [`parse_seatbelt_report`]; [`Source::Os`]);
//! - an advisory about a program the run is about to start
//!   ([`Source::Advisory`], [`program_advisory`]).
//!
//! Each event names the access, who made it when that is known, and the
//! grant that would allow it ([`Grant`]), or says that no grant can.
//!
//! The contract for code that refuses: ask [`is_active`] (one atomic load);
//! when it is true, `report(WouldDeny::…)` and allow, otherwise refuse as
//! before. `report` is a no-op while no reporter is installed, so calling it
//! unconditionally is fine where the event is cheap to build.
//!
//! Events go two ways: a line on stderr through `log::warn!` (each distinct
//! event once, the first [`MAX_PRINTED`] of them), and, when
//! [`WARN_LOG_ENV_VAR`] names a file, one JSON object per line appended to
//! it ([`Record`]). The file is what tools read; several processes of one
//! run append to the same file (each line is one `write`).

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use serde::Deserialize;
use serde::Serialize;

/// Bumped whenever a serialized [`Record`] changes shape; readers skip lines
/// of a format they don't know.
pub const WOULD_DENY_FORMAT: u32 = 1;

/// Names the file warn mode appends its [`Record`]s to (JSON lines). Read
/// by every process of a run, so the processes it starts append to the same
/// file. Unset: stderr only.
pub const WARN_LOG_ENV_VAR: &str = "DENO_KERNEL_SANDBOX_WARN_LOG";

/// `0` keeps a warn run from applying its observer (macOS: the reporting
/// Seatbelt profile): it then reports from the policy and the runtime only,
/// and nothing at all is applied to the process. For a warn run of a program
/// that applies a sandbox of its own, and for tools that only want the
/// policy's own findings.
pub const WARN_OBSERVE_ENV_VAR: &str = "DENO_KERNEL_SANDBOX_WARN_OBSERVE";

/// The most distinct events printed to stderr; the rest are counted (and
/// still written to the log file).
pub const MAX_PRINTED: usize = 20;

/// The most distinct events kept for the summary and the duplicate check.
pub const MAX_DISTINCT: usize = 10_000;

/// What kind of access would be refused.
#[derive(
  Debug,
  Clone,
  Copy,
  PartialEq,
  Eq,
  Hash,
  PartialOrd,
  Ord,
  Serialize,
  Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub enum AccessClass {
  /// Reading a file or listing a folder.
  Read,
  /// Writing, creating, removing or renaming.
  Write,
  /// Starting a program.
  Exec,
  /// Loading native code (a shared library, a Node-API addon).
  LoadCode,
  /// An outbound connection.
  NetConnect,
  /// Binding or listening on a port.
  NetListen,
  /// Connecting to a Unix socket by path.
  Unix,
  /// System information (a sysctl, a `/proc` file).
  Sys,
  /// The environment.
  Env,
  /// Signalling or inspecting a process outside the sandbox.
  Signal,
  /// An OS service (a macOS mach service, launching an app, Apple events).
  Service,
  /// A system call on the refused list.
  Syscall,
  /// A rule the OS can't hold here: `enforce` would refuse to start.
  Startup,
  /// Anything else the OS reported.
  Other,
}

impl AccessClass {
  /// "would refuse <this>".
  fn verb(self) -> &'static str {
    match self {
      Self::Read => "reading",
      Self::Write => "writing",
      Self::Exec => "starting",
      Self::LoadCode => "loading native code from",
      Self::NetConnect => "connecting to",
      Self::NetListen => "listening on",
      Self::Unix => "connecting to the Unix socket",
      Self::Sys => "reading system information",
      Self::Env => "the environment variable",
      Self::Signal => "signalling or inspecting",
      Self::Service => "the OS service",
      Self::Syscall => "the system call",
      Self::Startup => "to start:",
      Self::Other => "",
    }
  }
}

/// Where the event comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
  /// The compiled policy, at startup.
  Policy,
  /// A decision point in the runtime or its supervisor.
  Runtime,
  /// The kernel's own record of what the policy would have refused.
  Os,
  /// An advisory about a program the run starts.
  Advisory,
}

/// The process that made the access.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessRef {
  pub pid: u32,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub name: Option<String>,
}

/// The grant that would allow an access: a permission flag and, when the
/// grant can be scoped, the value to add to its list.
#[derive(
  Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub struct Grant {
  /// `--allow-read`, `--allow-net`, …
  pub flag: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub value: Option<String>,
}

impl Grant {
  pub fn new(flag: &str, value: impl Into<String>) -> Self {
    Self {
      flag: flag.to_string(),
      value: Some(value.into()),
    }
  }

  pub fn unscoped(flag: &str) -> Self {
    Self {
      flag: flag.to_string(),
      value: None,
    }
  }
}

impl std::fmt::Display for Grant {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match &self.value {
      Some(value) => write!(f, "{}={}", self.flag, value),
      None => f.write_str(&self.flag),
    }
  }
}

/// One access the kernel sandbox would refuse under `enforce`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WouldDeny {
  pub class: AccessClass,
  /// What was accessed: a path, `host:port`, a program, a service name, or
  /// for [`AccessClass::Startup`] the rule as a flag.
  pub target: String,
  /// The operation as the reporter saw it (`file-read-data`, `openat`,
  /// `connect`), when it adds something to the class.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub op: Option<String>,
  pub source: Source,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub process: Option<ProcessRef>,
  /// The grant that would allow it; `None` when no grant can (the `note`
  /// says why).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub grant: Option<Grant>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub note: Option<String>,
  /// The OS's own libraries made the access, in whatever program loaded
  /// them, and do without it when it is refused (see [`system_noise`]): it
  /// is written to the log, never printed, and never counted or turned into
  /// a grant to add.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub benign: bool,
}

/// Names in a home folder that usually hold credentials: a suggestion to
/// grant one carries a caution.
const CREDENTIAL_NAMES: &[&str] = &[
  ".ssh",
  ".aws",
  ".azure",
  ".gnupg",
  ".netrc",
  ".npmrc",
  ".yarnrc",
  ".yarnrc.yml",
  ".pypirc",
  ".docker",
  ".kube",
  ".git-credentials",
  ".config/gh",
  ".config/gcloud",
  ".cargo/credentials",
  ".cargo/credentials.toml",
];

const CREDENTIAL_NOTE: &str = "this path usually holds credentials: grant it only if you accept that native code and child processes can read it";

/// Whether `path` is, or lies in, a folder that usually holds credentials
/// (matched on the path's text: a component sequence such as `.ssh` or
/// `.config/gh` anywhere in it).
pub fn looks_like_credentials(path: &str) -> bool {
  let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
  CREDENTIAL_NAMES.iter().any(|name| {
    let want: Vec<&str> = name.split('/').collect();
    parts.windows(want.len()).any(|w| w == want.as_slice())
  })
}

impl WouldDeny {
  fn new(class: AccessClass, target: impl Into<String>) -> Self {
    Self {
      class,
      target: target.into(),
      op: None,
      source: Source::Runtime,
      process: None,
      grant: None,
      note: None,
      benign: false,
    }
  }

  fn path(class: AccessClass, flag: &str, path: &Path) -> Self {
    let text = path.to_string_lossy().into_owned();
    let mut event = Self::new(class, text.clone());
    if looks_like_credentials(&text) {
      event.note = Some(CREDENTIAL_NOTE.to_string());
    }
    event.grant = Some(Grant::new(flag, text));
    event
  }

  /// Reading `path` (a file, or listing a folder).
  pub fn read(path: &Path) -> Self {
    Self::path(AccessClass::Read, "--allow-read", path)
  }

  /// Writing, creating, removing or renaming `path`.
  pub fn write(path: &Path) -> Self {
    Self::path(AccessClass::Write, "--allow-write", path)
  }

  /// Starting the program at `path` (or named `path`).
  pub fn exec(program: &str) -> Self {
    let mut event = Self::new(AccessClass::Exec, program);
    event.grant = Some(Grant::new("--allow-run", program));
    event
  }

  /// Loading native code from `path`.
  pub fn load_code(path: &Path) -> Self {
    Self::path(AccessClass::LoadCode, "--allow-ffi", path)
  }

  /// Connecting to `host:port`. A host the reporter couldn't see is `*`:
  /// the grant then names a placeholder to fill in.
  pub fn connect(host: &str, port: u16) -> Self {
    let mut event =
      Self::new(AccessClass::NetConnect, format!("{host}:{port}"));
    let shown = if host == "*" { "<host>" } else { host };
    event.grant = Some(Grant::new("--allow-net", format!("{shown}:{port}")));
    event
  }

  /// Binding or listening on `port`.
  pub fn listen(port: u16) -> Self {
    let mut event = Self::new(AccessClass::NetListen, format!(":{port}"));
    event.grant = Some(Grant::new("--allow-net", format!("0.0.0.0:{port}")));
    event
  }

  /// Connecting to the Unix socket at `path`.
  pub fn unix(path: &Path) -> Self {
    let text = path.to_string_lossy().into_owned();
    let mut event = Self::new(AccessClass::Unix, text.clone());
    event.grant = Some(Grant::new("--allow-net", format!("unix:{text}")));
    event
  }

  /// A rule `enforce` would refuse to start over on this machine: `rule`
  /// as a flag, `detail` what the OS does instead.
  pub fn startup(rule: &str, detail: &str) -> Self {
    let mut event = Self::new(AccessClass::Startup, rule);
    event.source = Source::Policy;
    event.note = Some(detail.to_string());
    event
  }

  /// Something no grant allows (`why` says so).
  pub fn ungrantable(class: AccessClass, target: &str, why: &str) -> Self {
    let mut event = Self::new(class, target);
    event.note = Some(why.to_string());
    event
  }

  pub fn from_source(mut self, source: Source) -> Self {
    self.source = source;
    self
  }

  pub fn by(mut self, pid: u32, name: Option<&str>) -> Self {
    self.process = Some(ProcessRef {
      pid,
      name: name.map(|n| n.to_string()),
    });
    self
  }

  pub fn with_op(mut self, op: &str) -> Self {
    self.op = Some(op.to_string());
    self
  }

  pub fn with_note(mut self, note: &str) -> Self {
    self.note = Some(note.to_string());
    self
  }

  /// What makes two events the same for the duplicate check and the
  /// summary: the access and who made it by name, never the pid.
  fn key(&self) -> (AccessClass, String, Option<String>) {
    (
      self.class,
      self.target.clone(),
      self.process.as_ref().and_then(|p| p.name.clone()),
    )
  }

  /// The line printed for the event (without the `Warning` prefix).
  pub fn message(&self) -> String {
    if self.source == Source::Advisory {
      // nothing was refused or would be: what is known about a program
      return format!(
        "kernel sandbox (warn): {} inside the sandbox: {}",
        self.target,
        self.note.as_deref().unwrap_or("see the documentation")
      );
    }
    let mut out = String::from("kernel sandbox (warn): enforce would refuse ");
    let verb = self.class.verb();
    if !verb.is_empty() {
      out.push_str(verb);
      out.push(' ');
    }
    if self.class == AccessClass::Other
      && let Some(op) = &self.op
    {
      out.push_str(op);
      out.push(' ');
    }
    out.push_str(&self.target);
    if let Some(process) = &self.process {
      match &process.name {
        Some(name) => out.push_str(&format!(" ({name}, pid {})", process.pid)),
        None => out.push_str(&format!(" (pid {})", process.pid)),
      }
    }
    match (&self.grant, &self.note) {
      (Some(grant), Some(note)) => {
        out.push_str(&format!(". Grant: {grant} ({note})"))
      }
      (Some(grant), None) => out.push_str(&format!(". Grant: {grant}")),
      (None, Some(note)) => out.push_str(&format!(". {note}")),
      (None, None) => {}
    }
    out
  }
}

/// How the run observes what native code and child processes do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Observer {
  /// Nothing observes them: only the policy and the runtime's own decision
  /// points report.
  None,
  /// macOS: a Seatbelt profile that allows everything and reports what the
  /// enforce profile would refuse to the unified log, each line tagged.
  SeatbeltReport,
  /// A supervising process that sees the tree's system calls and lets them
  /// continue.
  SupervisorTrace,
}

/// One line of the warn log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Record {
  /// A process of the run started reporting.
  #[serde(rename_all = "camelCase")]
  Run {
    v: u32,
    pid: u32,
    /// `linux`, `macos`, `windows`.
    os: String,
    /// The compiled policy's hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    policy: Option<String>,
    observer: Observer,
    /// The tag every OS report line of this run carries (macOS).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
    /// Seconds since the Unix epoch when the observer was applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    since: Option<u64>,
  },
  #[serde(rename_all = "camelCase")]
  WouldDeny {
    v: u32,
    #[serde(flatten)]
    event: WouldDeny,
  },
}

impl Record {
  pub fn to_line(&self) -> String {
    let mut line = serde_json::to_string(self).expect("a record serializes");
    line.push('\n');
    line
  }

  /// Parses one line of a warn log; `None` for a blank line, a line that
  /// isn't a record, or a record of another format.
  pub fn from_line(line: &str) -> Option<Self> {
    let record: Self = serde_json::from_str(line.trim()).ok()?;
    let v = match &record {
      Self::Run { v, .. } | Self::WouldDeny { v, .. } => *v,
    };
    (v == WOULD_DENY_FORMAT).then_some(record)
  }
}

/// What a run reported, for its last line and for tools.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
  /// Distinct events.
  pub distinct: usize,
  /// Events reported, duplicates included.
  pub total: u64,
  /// Distinct events not printed (past [`MAX_PRINTED`]).
  pub unprinted: usize,
  pub by_class: BTreeMap<AccessClass, usize>,
  /// The grants that would allow them, each once, sorted.
  pub grants: Vec<Grant>,
  /// Distinct events no grant allows.
  pub ungrantable: usize,
  /// Advisories about started programs (not counted in `distinct`).
  pub advisories: usize,
  /// Accesses by the OS's own libraries that do without them (logged only,
  /// not counted in `distinct`).
  pub benign: usize,
}

struct Reporter {
  seen: HashSet<(AccessClass, String, Option<String>)>,
  events: Vec<WouldDeny>,
  total: u64,
  /// Distinct benign events (logged only).
  benign: usize,
  printed: usize,
  log: Option<std::fs::File>,
  print: bool,
}

impl Reporter {
  fn write(&mut self, record: &Record) {
    if let Some(log) = &mut self.log {
      // one write per line: several processes append to the same file
      let _ = log.write_all(record.to_line().as_bytes());
    }
  }

  fn report(&mut self, event: WouldDeny) {
    self.total += 1;
    if self.seen.len() >= MAX_DISTINCT || !self.seen.insert(event.key()) {
      return;
    }
    self.write(&Record::WouldDeny {
      v: WOULD_DENY_FORMAT,
      event: event.clone(),
    });
    if event.benign {
      self.benign += 1;
      return;
    }
    if self.print && self.printed < MAX_PRINTED {
      self.printed += 1;
      log::warn!("Warning {}", event.message());
    }
    self.events.push(event);
  }

  fn summary(&self) -> Summary {
    let mut by_class = BTreeMap::new();
    let mut grants: Vec<Grant> = Vec::new();
    let mut ungrantable = 0;
    let mut advisories = 0;
    for event in &self.events {
      if event.source == Source::Advisory {
        advisories += 1;
        continue;
      }
      *by_class.entry(event.class).or_insert(0) += 1;
      match &event.grant {
        Some(grant) => grants.push(grant.clone()),
        None => ungrantable += 1,
      }
    }
    grants.sort();
    grants.dedup();
    Summary {
      distinct: self.events.len() - advisories,
      total: self.total,
      unprinted: self.events.len().saturating_sub(self.printed),
      by_class,
      grants,
      ungrantable,
      advisories,
      benign: self.benign,
    }
  }
}

static ACTIVE: AtomicBool = AtomicBool::new(false);
static REPORTER: OnceLock<Mutex<Reporter>> = OnceLock::new();

/// How [`install`] sets the reporter up.
#[derive(Debug, Clone, Default)]
pub struct ReporterConfig {
  /// The file to append [`Record`]s to (the value of
  /// [`WARN_LOG_ENV_VAR`]); `None`: stderr only.
  pub log: Option<PathBuf>,
  /// Print each distinct event (the first [`MAX_PRINTED`]) through
  /// `log::warn!`.
  pub print: bool,
}

/// Turns warn-mode reporting on for this process. The first call wins;
/// returns whether this one did. A log file that can't be opened for append
/// leaves the file sink off (the events are still printed).
pub fn install(config: ReporterConfig) -> bool {
  #[allow(clippy::disallowed_methods, reason = "the real file system")]
  let log = config.log.as_ref().and_then(|path| {
    std::fs::OpenOptions::new()
      .create(true)
      .append(true)
      .open(path)
      .ok()
  });
  let installed = REPORTER
    .set(Mutex::new(Reporter {
      seen: HashSet::new(),
      events: Vec::new(),
      total: 0,
      benign: 0,
      printed: 0,
      log,
      print: config.print,
    }))
    .is_ok();
  if installed {
    ACTIVE.store(true, Ordering::SeqCst);
  }
  installed
}

/// Whether warn mode reports in this process: code that would refuse under
/// `enforce` reports and allows instead.
pub fn is_active() -> bool {
  ACTIVE.load(Ordering::Relaxed)
}

/// Reports one access the sandbox would refuse. A no-op unless a reporter
/// is installed.
pub fn report(event: WouldDeny) {
  if !is_active() {
    return;
  }
  if let Some(reporter) = REPORTER.get()
    && let Ok(mut reporter) = reporter.lock()
  {
    reporter.report(event);
  }
}

/// Appends a [`Record::Run`] to the log file (no-op without one).
pub fn record_run(record: Record) {
  if let Some(reporter) = REPORTER.get()
    && let Ok(mut reporter) = reporter.lock()
  {
    reporter.write(&record);
  }
}

/// What this process reported so far.
pub fn summary() -> Summary {
  REPORTER
    .get()
    .and_then(|r| r.lock().ok().map(|r| r.summary()))
    .unwrap_or_default()
}

/// The last line of a warn run: the rules `enforce` would refuse to start
/// over, how many accesses it would refuse, and the grants that would allow
/// them. `None` when nothing was reported (advisories aside).
pub fn summary_line(summary: &Summary) -> Option<String> {
  if summary.distinct == 0 {
    return None;
  }
  let startup = summary
    .by_class
    .get(&AccessClass::Startup)
    .copied()
    .unwrap_or(0);
  let accesses = summary.distinct - startup;
  let mut out = String::from("kernel sandbox (warn): enforce would refuse ");
  if startup > 0 {
    out.push_str(&format!(
      "to start over {startup} rule(s) the OS can't hold here"
    ));
  }
  if accesses > 0 {
    if startup > 0 {
      out.push_str(", and would refuse ");
    }
    out.push_str(&format!("{accesses} access(es)"));
  }
  if summary.unprinted > 0 {
    out.push_str(&format!(" ({} line(s) not shown above)", summary.unprinted));
  }
  if !summary.grants.is_empty() {
    const SHOWN: usize = 12;
    let grants: Vec<String> = summary
      .grants
      .iter()
      .take(SHOWN)
      .map(|g| g.to_string())
      .collect();
    out.push_str(&format!(
      ". Grants that would allow them: {}",
      grants.join(" ")
    ));
    if summary.grants.len() > SHOWN {
      out.push_str(&format!(" (+{} more)", summary.grants.len() - SHOWN));
    }
  }
  // a startup rule has no grant by nature: only accesses count here
  let ungrantable = summary.ungrantable.saturating_sub(startup);
  if ungrantable > 0 {
    out.push_str(&format!(". {ungrantable} can't be granted (see each line)"));
  }
  Some(out)
}

/// Whether `tag` can mark a run's OS report lines: letters, digits, `-`,
/// `_`, `:` and `.`, at most 64 (it is written into a Seatbelt profile and
/// a log predicate).
pub fn valid_tag(tag: &str) -> bool {
  !tag.is_empty()
    && tag.len() <= 64
    && tag.bytes().all(|b| {
      b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'.')
    })
}

/// The sysctl names the macOS profile refuses without `--allow-sys`, with
/// the `--allow-sys` name that grants each.
const SYSCTL_SYS_NAMES: &[(&str, &str)] = &[
  ("vm.loadavg", "loadavg"),
  ("hw.memsize", "systemMemoryInfo"),
  ("vm.swapusage", "systemMemoryInfo"),
  ("kern.boottime", "osUptime"),
  ("kern.hostname", "hostname"),
  ("kern.osrelease", "osRelease"),
  ("kern.osproductversion", "osRelease"),
];

/// Parses one Seatbelt report message from the macOS unified log, as the
/// reporting profile of a warn run produces it:
///
/// ```text
/// Sandbox: git(4312) allow file-read-data /Users/me/.gitconfig
/// denext-ks:5f3a…
/// ```
///
/// (the kernel also writes `Sandbox: 3 duplicate reports for git(4312) allow
/// …`). `tag` is the run's tag: a message without it on a line of its own
/// belongs to another process and gives `None`, as does anything that isn't
/// a report. The reporting profile reports exactly what the enforce profile
/// refuses, so each parsed line is an access `enforce` would refuse.
pub fn parse_seatbelt_report(message: &str, tag: &str) -> Option<WouldDeny> {
  let mut lines = message.lines();
  let first = lines.next()?.trim();
  if !lines.any(|l| l.trim() == tag) {
    return None;
  }
  let rest = first.strip_prefix("Sandbox: ")?;
  let rest = match rest.split_once(" duplicate reports for ") {
    Some((count, rest)) if count.bytes().all(|b| b.is_ascii_digit()) => rest,
    _ => rest,
  };
  // name(pid) allow op detail; the name may hold spaces and parentheses
  let (who, what) = rest.split_once(") allow ")?;
  let (name, pid) = who.rsplit_once('(')?;
  let pid: u32 = pid.parse().ok()?;
  let (op, detail) = match what.split_once(' ') {
    Some((op, detail)) => (op, detail.trim()),
    None => (what, ""),
  };
  let path =
    |text: &str| PathBuf::from(text.strip_prefix("path:").unwrap_or(text));
  let none = "no permission grants this: the kernel sandbox refuses it for native code and child processes";
  let net = "a network grant (--allow-net) for this address would allow it";
  let event = if op == "file-map-executable" {
    WouldDeny::load_code(&path(detail))
  } else if op == "file-ioctl" {
    // path:/dev/ttys003 ioctl-command:(…)
    let target = detail.split(" ioctl-command:").next().unwrap_or(detail);
    WouldDeny::ungrantable(
      AccessClass::Other,
      target.strip_prefix("path:").unwrap_or(target),
      "terminal ioctls are refused (a terminal can be made to type commands)",
    )
  } else if (op.starts_with("file-read") || op.starts_with("file-write"))
    && is_terminal_device(detail)
  {
    WouldDeny::ungrantable(
      AccessClass::Other,
      detail,
      "a terminal other than the run's own is refused, whatever is granted",
    )
  } else if op.starts_with("file-read") {
    WouldDeny::read(&path(detail))
  } else if op.starts_with("file-write") || op == "file-link" {
    WouldDeny::write(&path(detail))
  } else if op.starts_with("process-exec") {
    WouldDeny::exec(detail)
  } else if op == "network-outbound" {
    if let Some(port) = port_of(detail, "remote:") {
      WouldDeny::connect("*", port)
    } else if detail.starts_with('/') {
      WouldDeny::unix(Path::new(detail))
    } else {
      WouldDeny::ungrantable(AccessClass::NetConnect, detail, net)
    }
  } else if op == "network-bind" || op == "network-inbound" {
    match port_of(detail, "local:") {
      Some(port) => WouldDeny::listen(port),
      None if detail.starts_with('/') => WouldDeny::unix(Path::new(detail)),
      None => WouldDeny::ungrantable(AccessClass::NetListen, detail, net),
    }
  } else if op.starts_with("network") {
    WouldDeny::ungrantable(AccessClass::NetConnect, detail, net)
  } else if op == "sysctl-read" {
    let mut event = WouldDeny::new(AccessClass::Sys, detail);
    match SYSCTL_SYS_NAMES.iter().find(|(name, _)| *name == detail) {
      Some((_, sys)) => event.grant = Some(Grant::new("--allow-sys", *sys)),
      None => event.note = Some(none.to_string()),
    }
    event
  } else if op == "mach-lookup"
    || op == "lsopen"
    || op == "appleevent-send"
    || op == "job-creation"
  {
    let target = if detail.is_empty() { op } else { detail };
    WouldDeny::ungrantable(AccessClass::Service, target, none)
  } else if op == "signal" || op.starts_with("process-info") {
    WouldDeny::ungrantable(
      AccessClass::Signal,
      if detail.is_empty() { op } else { detail },
      "a confined process tree can signal and inspect only its own processes",
    )
  } else {
    WouldDeny::ungrantable(AccessClass::Other, detail, none)
  };
  let mut event = event
    .with_op(op)
    .from_source(Source::Os)
    .by(pid, Some(name));
  event.benign = system_noise(op, detail);
  Some(event)
}

/// What macOS's own libraries do in any process that loads them, refused
/// by the cli profile and done without: the logging and network libraries
/// reading their preferences and looking up their daemons when they first
/// initialise, the text-encoding hint in the home folder. An access here is
/// not the program's doing and needs no grant. Kept short and exact on
/// purpose: an entry is added only for an access seen to be refused under
/// `enforce` with the program carrying on.
const SYSTEM_NOISE_PATHS: &[&str] = &[
  "/Library/Preferences/com.apple.networkd.plist",
  "/Library/Preferences/Logging/",
  "/private/var/run/mDNSResponder",
  "/dev/autofs_nowait",
];
const SYSTEM_NOISE_SERVICES: &[&str] = &[
  "com.apple.diagnosticd",
  "com.apple.logd",
  "com.apple.system.opendirectoryd.libinfo",
];

/// Whether a Seatbelt report (`op`, with its detail) is one of the accesses
/// in [`SYSTEM_NOISE_PATHS`] / [`SYSTEM_NOISE_SERVICES`], or a read of the
/// network configuration (`system-info`).
pub fn system_noise(op: &str, detail: &str) -> bool {
  if op == "system-info" {
    return true;
  }
  if op == "mach-lookup" {
    return SYSTEM_NOISE_SERVICES.contains(&detail);
  }
  let path = detail.strip_prefix("path:").unwrap_or(detail);
  (op.starts_with("file-read") || op == "network-outbound")
    && (SYSTEM_NOISE_PATHS
      .iter()
      .any(|p| match p.strip_suffix('/') {
        Some(_) => path.starts_with(p),
        None => path == *p,
      })
      || path.ends_with("/.CFUserTextEncoding"))
}

/// Whether `path` is a terminal device the cli profile refuses unless it is
/// one of the run's own (`/dev/tty`, `/dev/ttysNNN`, `/dev/pty…`).
fn is_terminal_device(path: &str) -> bool {
  path == "/dev/tty"
    || path.starts_with("/dev/pty")
    || path
      .strip_prefix("/dev/ttys")
      .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The port of a Seatbelt network detail such as `remote:*:443` or
/// `local:*:8000` (the address may be an IPv6 literal with colons).
fn port_of(detail: &str, prefix: &str) -> Option<u16> {
  let rest = detail.strip_prefix(prefix)?;
  rest.rsplit_once(':')?.1.parse().ok()
}

/// What to know before a confined run starts `program` (its name, or the
/// last part of its path): the programs that are known to need a grant of
/// their own, or that can't work inside a kernel sandbox whatever is
/// granted. `None` for a program with nothing known.
pub fn program_advisory(program: &str) -> Option<&'static str> {
  let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
  let name = name.strip_suffix(".exe").unwrap_or(name);
  let lower = name.to_ascii_lowercase();
  Some(match lower.as_str() {
    "sudo" | "su" | "doas" | "pkexec" | "newuidmap" | "newgidmap"
    | "fusermount" | "fusermount3" => {
      "it gains privileges through a setuid bit, which a confined process tree never does: it can't work inside the kernel sandbox"
    }
    "chrome"
    | "chromium"
    | "chromium-browser"
    | "google-chrome"
    | "google chrome"
    | "google chrome for testing"
    | "msedge"
    | "brave"
    | "chrome-headless-shell"
    | "headless_shell"
    | "electron" => {
      "a browser's own sandbox can't start inside another one: start it with --no-sandbox, as in a container"
    }
    "docker" | "podman" | "nerdctl" | "kubectl" => {
      "it talks to a daemon over a Unix socket: grant the socket with --allow-net=unix:<path> (reaching a container daemon is usually root on the host)"
    }
    "git" => {
      "it reads its configuration (grant --allow-read=<home>/.gitconfig) and, for SSH remotes, the agent socket (--allow-net=unix:$SSH_AUTH_SOCK)"
    }
    "ssh" | "scp" | "sftp" | "rsync" => {
      "it reads <home>/.ssh and talks to the agent socket: grant --allow-read=<home>/.ssh and --allow-net=unix:$SSH_AUTH_SOCK if it should use them"
    }
    "npm" | "pnpm" | "yarn" | "npx" | "bun" => {
      "it reads <home>/.npmrc (never readable unless granted: it may hold registry tokens) and writes its cache folder: grant both if it should use them"
    }
    "strace" | "ltrace" | "gdb" | "lldb" | "dtrace" | "perf" => {
      "tracing another process is refused inside the kernel sandbox"
    }
    "bwrap" | "flatpak" | "unshare" | "nsenter" | "firejail"
    | "sandbox-exec" => {
      "it builds a sandbox of its own (namespaces, or a second Seatbelt profile), which a confined process can't"
    }
    _ => return None,
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  const TAG: &str = "denext-ks:5f3a";

  fn seatbelt(line: &str) -> Option<WouldDeny> {
    parse_seatbelt_report(&format!("{line}\n{TAG}"), TAG)
  }

  #[test]
  fn events_name_the_grant() {
    let read = WouldDeny::read(Path::new("/data/in.csv"));
    assert_eq!(read.grant.unwrap().to_string(), "--allow-read=/data/in.csv");
    let write = WouldDeny::write(Path::new("/out/x"));
    assert_eq!(write.grant.unwrap().to_string(), "--allow-write=/out/x");
    assert_eq!(
      WouldDeny::connect("api.example.com", 443)
        .grant
        .unwrap()
        .to_string(),
      "--allow-net=api.example.com:443"
    );
    // a host the reporter couldn't see is a placeholder, never a wildcard
    assert_eq!(
      WouldDeny::connect("*", 5432).grant.unwrap().to_string(),
      "--allow-net=<host>:5432"
    );
    assert_eq!(
      WouldDeny::listen(8000).grant.unwrap().to_string(),
      "--allow-net=0.0.0.0:8000"
    );
    assert_eq!(
      WouldDeny::unix(Path::new("/var/run/docker.sock"))
        .grant
        .unwrap()
        .to_string(),
      "--allow-net=unix:/var/run/docker.sock"
    );
    assert_eq!(
      WouldDeny::exec("/usr/bin/git").grant.unwrap().to_string(),
      "--allow-run=/usr/bin/git"
    );
    assert_eq!(
      WouldDeny::load_code(Path::new("/opt/lib/libfoo.dylib"))
        .grant
        .unwrap()
        .to_string(),
      "--allow-ffi=/opt/lib/libfoo.dylib"
    );
    let startup = WouldDeny::startup("--allow-net=example.com", "needs Mode B");
    assert_eq!(startup.grant, None);
    assert_eq!(startup.source, Source::Policy);
  }

  #[test]
  fn credential_paths_carry_a_caution() {
    for path in [
      "/Users/me/.ssh/id_ed25519",
      "/home/me/.npmrc",
      "/home/me/.config/gh/hosts.yml",
      "/home/me/.aws",
    ] {
      assert!(looks_like_credentials(path), "{path}");
      assert_eq!(
        WouldDeny::read(Path::new(path)).note.as_deref(),
        Some(CREDENTIAL_NOTE),
        "{path}"
      );
    }
    for path in ["/home/me/project/src", "/home/me/.config/app", "/ssh/x"] {
      assert!(!looks_like_credentials(path), "{path}");
      assert_eq!(WouldDeny::read(Path::new(path)).note, None, "{path}");
    }
  }

  #[test]
  fn messages_say_what_who_and_the_grant() {
    let event = WouldDeny::read(Path::new("/Users/me/notes.txt"))
      .by(4312, Some("cat"))
      .from_source(Source::Os);
    assert_eq!(
      event.message(),
      "kernel sandbox (warn): enforce would refuse reading /Users/me/notes.txt (cat, pid 4312). Grant: --allow-read=/Users/me/notes.txt"
    );
    let event = WouldDeny::startup(
      "--allow-net=example.com",
      "a host name needs the supervising broker",
    );
    assert_eq!(
      event.message(),
      "kernel sandbox (warn): enforce would refuse to start: --allow-net=example.com. a host name needs the supervising broker"
    );
    let event = WouldDeny::read(Path::new("/home/me/.npmrc"));
    assert!(event.message().ends_with(&format!(
      "Grant: --allow-read=/home/me/.npmrc ({CREDENTIAL_NOTE})"
    )));
  }

  #[test]
  fn records_round_trip_and_unknown_lines_are_skipped() {
    let run = Record::Run {
      v: WOULD_DENY_FORMAT,
      pid: 7,
      os: "macos".to_string(),
      policy: Some("1775641b2a5ce963".to_string()),
      observer: Observer::SeatbeltReport,
      tag: Some(TAG.to_string()),
      since: Some(1_800_000_000),
    };
    let line = run.to_line();
    assert!(line.ends_with('\n'));
    assert!(line.contains("\"type\":\"run\""));
    assert!(line.contains("\"observer\":\"seatbelt-report\""));
    assert_eq!(Record::from_line(&line), Some(run));

    let event = Record::WouldDeny {
      v: WOULD_DENY_FORMAT,
      event: WouldDeny::connect("*", 443)
        .by(9, Some("curl"))
        .with_op("network-outbound")
        .from_source(Source::Os),
    };
    let line = event.to_line();
    // the event's fields are the record's own (one flat object per line)
    assert_eq!(
      line.trim(),
      r#"{"type":"wouldDeny","v":1,"class":"netConnect","target":"*:443","op":"network-outbound","source":"os","process":{"pid":9,"name":"curl"},"grant":{"flag":"--allow-net","value":"<host>:443"}}"#
    );
    assert_eq!(Record::from_line(&line), Some(event));

    assert_eq!(Record::from_line(""), None);
    assert_eq!(Record::from_line("not json"), None);
    assert_eq!(Record::from_line(r#"{"type":"other","v":1}"#), None);
    // another format is skipped, never misread
    assert_eq!(
      Record::from_line(
        r#"{"type":"wouldDeny","v":2,"class":"read","target":"/x","source":"os"}"#
      ),
      None
    );
  }

  #[test]
  fn the_reporter_counts_duplicates_once() {
    let mut reporter = Reporter {
      seen: HashSet::new(),
      events: Vec::new(),
      total: 0,
      benign: 0,
      printed: 0,
      log: None,
      print: false,
    };
    reporter.report(WouldDeny::read(Path::new("/a")).by(1, Some("cat")));
    // the same access by the same program under another pid
    reporter.report(WouldDeny::read(Path::new("/a")).by(2, Some("cat")));
    // by another program: distinct
    reporter.report(WouldDeny::read(Path::new("/a")).by(3, Some("git")));
    reporter.report(WouldDeny::connect("*", 443));
    reporter.report(WouldDeny::ungrantable(
      AccessClass::Service,
      "com.apple.windowserver.active",
      "no",
    ));
    let summary = reporter.summary();
    assert_eq!(summary.total, 5);
    assert_eq!(summary.distinct, 4);
    assert_eq!(summary.by_class[&AccessClass::Read], 2);
    assert_eq!(summary.ungrantable, 1);
    assert_eq!(
      summary
        .grants
        .iter()
        .map(|g| g.to_string())
        .collect::<Vec<_>>(),
      ["--allow-net=<host>:443", "--allow-read=/a"]
    );
    assert_eq!(
      summary_line(&summary).unwrap(),
      "kernel sandbox (warn): enforce would refuse 4 access(es) (4 line(s) not shown above). Grants that would allow them: --allow-net=<host>:443 --allow-read=/a. 1 can't be granted (see each line)"
    );
    assert_eq!(summary_line(&Summary::default()), None);
    // an advisory is said, never counted as a refusal
    let advisory = WouldDeny::ungrantable(
      AccessClass::Exec,
      "git",
      "it reads its configuration",
    )
    .from_source(Source::Advisory);
    assert_eq!(
      advisory.message(),
      "kernel sandbox (warn): git inside the sandbox: it reads its configuration"
    );
    reporter.report(advisory);
    let summary = reporter.summary();
    assert_eq!((summary.distinct, summary.advisories), (4, 1));
    assert_eq!(summary.ungrantable, 1);
    // rules enforce wouldn't start over are said apart from accesses
    reporter.report(WouldDeny::startup(
      "--allow-net=example.com",
      "needs Mode B",
    ));
    let line = summary_line(&reporter.summary()).unwrap();
    assert!(
      line.starts_with(
        "kernel sandbox (warn): enforce would refuse to start over 1 rule(s) the OS can't hold here, and would refuse 4 access(es)"
      ),
      "{line}"
    );
    assert!(
      line.ends_with("1 can't be granted (see each line)"),
      "{line}"
    );
    let mut only_startup = Reporter {
      seen: HashSet::new(),
      events: Vec::new(),
      total: 0,
      benign: 0,
      printed: 1,
      log: None,
      print: false,
    };
    only_startup.report(WouldDeny::startup(
      "--allow-net=example.com",
      "needs Mode B",
    ));
    assert_eq!(
      summary_line(&only_startup.summary()).unwrap(),
      "kernel sandbox (warn): enforce would refuse to start over 1 rule(s) the OS can't hold here"
    );
  }

  #[test]
  #[allow(clippy::disallowed_methods, reason = "the real file system")]
  fn the_reporter_appends_json_lines() {
    let dir = std::env::temp_dir().join(format!(
      "deno_sandbox_would_deny_{}_{:?}",
      std::process::id(),
      std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("warn.jsonl");
    let open = || {
      std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap()
    };
    // two reporters (two processes of one run) append to the same file
    for name in ["a", "b"] {
      let mut reporter = Reporter {
        seen: HashSet::new(),
        events: Vec::new(),
        total: 0,
        benign: 0,
        printed: 0,
        log: Some(open()),
        print: false,
      };
      reporter.report(WouldDeny::read(Path::new(&format!("/{name}"))));
      reporter.report(WouldDeny::read(Path::new(&format!("/{name}"))));
    }
    let text = std::fs::read_to_string(&path).unwrap();
    let targets: Vec<String> = text
      .lines()
      .filter_map(Record::from_line)
      .map(|r| match r {
        Record::WouldDeny { event, .. } => event.target,
        Record::Run { .. } => unreachable!(),
      })
      .collect();
    assert_eq!(targets, ["/a", "/b"]);
    std::fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn seatbelt_reports_parse_into_events() {
    let event =
      seatbelt("Sandbox: cat(50119) allow file-read-data /private/etc/hosts")
        .unwrap();
    assert_eq!(event.class, AccessClass::Read);
    assert_eq!(event.target, "/private/etc/hosts");
    assert_eq!(event.op.as_deref(), Some("file-read-data"));
    assert_eq!(event.source, Source::Os);
    assert_eq!(
      event.process,
      Some(ProcessRef {
        pid: 50119,
        name: Some("cat".to_string())
      })
    );
    assert_eq!(
      event.grant.unwrap().to_string(),
      "--allow-read=/private/etc/hosts"
    );

    let event = seatbelt(
      "Sandbox: bash(73346) allow file-write-create /private/tmp/out file.txt",
    )
    .unwrap();
    assert_eq!(event.class, AccessClass::Write);
    // the rest of the line is the path, spaces included
    assert_eq!(event.target, "/private/tmp/out file.txt");

    let event =
      seatbelt("Sandbox: rm(73362) allow file-write-unlink /private/tmp/x")
        .unwrap();
    assert_eq!(
      event.grant.unwrap().to_string(),
      "--allow-write=/private/tmp/x"
    );

    let event =
      seatbelt("Sandbox: nc(73355) allow network-outbound remote:*:9").unwrap();
    assert_eq!(event.class, AccessClass::NetConnect);
    assert_eq!(event.target, "*:9");
    assert_eq!(event.grant.unwrap().to_string(), "--allow-net=<host>:9");

    let event = seatbelt(
      "Sandbox: curl(1) allow network-outbound /private/var/run/docker.sock",
    )
    .unwrap();
    assert_eq!(event.class, AccessClass::Unix);
    assert_eq!(
      event.grant.unwrap().to_string(),
      "--allow-net=unix:/private/var/run/docker.sock"
    );

    let event =
      seatbelt("Sandbox: deno(2) allow network-bind local:*:8000").unwrap();
    assert_eq!(event.class, AccessClass::NetListen);
    assert_eq!(event.grant.unwrap().to_string(), "--allow-net=0.0.0.0:8000");

    let event = seatbelt(
      "Sandbox: deno(2) allow file-map-executable /opt/homebrew/lib/libpq.dylib",
    )
    .unwrap();
    assert_eq!(event.class, AccessClass::LoadCode);
    assert_eq!(
      event.grant.unwrap().to_string(),
      "--allow-ffi=/opt/homebrew/lib/libpq.dylib"
    );

    let event =
      seatbelt("Sandbox: sh(3) allow process-exec* /usr/bin/git").unwrap();
    assert_eq!(event.class, AccessClass::Exec);
    assert_eq!(event.grant.unwrap().to_string(), "--allow-run=/usr/bin/git");

    let event = seatbelt(
      "Sandbox: deno(2) allow mach-lookup com.apple.windowserver.active",
    )
    .unwrap();
    assert_eq!(event.class, AccessClass::Service);
    assert_eq!(event.grant, None);
    assert!(event.note.is_some());

    let event =
      seatbelt("Sandbox: deno(2) allow sysctl-read vm.loadavg").unwrap();
    assert_eq!(event.grant.unwrap().to_string(), "--allow-sys=loadavg");
    let event =
      seatbelt("Sandbox: deno(2) allow sysctl-read kern.proc.pid.1").unwrap();
    assert_eq!(event.grant, None);

    let event = seatbelt(
      "Sandbox: bash(4) allow file-ioctl path:/dev/ttys003 ioctl-command:(_IO \"t\" 114)",
    )
    .unwrap();
    assert_eq!(event.target, "/dev/ttys003");
    assert_eq!(event.grant, None);

    // a terminal that isn't the run's own: no grant
    for line in [
      "Sandbox: bash(4) allow file-read-data /dev/tty",
      "Sandbox: bash(4) allow file-write-data /dev/ttys012",
      "Sandbox: script(4) allow file-read-data /dev/ptmx-like",
    ] {
      let event = seatbelt(line);
      if line.contains("ptmx-like") {
        // not a terminal device name the profile refuses: an ordinary path
        assert!(event.unwrap().grant.is_some(), "{line}");
        continue;
      }
      let event = event.unwrap();
      assert_eq!(event.grant, None, "{line}");
      assert_eq!(event.class, AccessClass::Other, "{line}");
    }
    let event =
      seatbelt("Sandbox: sh(4) allow file-read-data /dev/ttyS-notes").unwrap();
    assert!(event.grant.is_some());

    // the kernel's duplicate counter
    let event = seatbelt(
      "Sandbox: 3 duplicate reports for git(9) allow file-read-data /Users/me/.gitconfig",
    )
    .unwrap();
    assert_eq!(event.target, "/Users/me/.gitconfig");
    assert_eq!(event.process.unwrap().name.as_deref(), Some("git"));

    // a program name with spaces and parentheses
    let event =
      seatbelt("Sandbox: Google Chrome (Helper)(77) allow file-read-data /x")
        .unwrap();
    assert_eq!(
      event.process.unwrap().name.as_deref(),
      Some("Google Chrome (Helper)")
    );
  }

  #[test]
  fn system_library_noise_is_logged_never_counted() {
    for line in [
      "Sandbox: deno(2) allow file-read-data /Library/Preferences/com.apple.networkd.plist",
      "Sandbox: deno(2) allow file-read-data /Library/Preferences/Logging/com.apple.diagnosticd.filter.plist",
      "Sandbox: deno(2) allow mach-lookup com.apple.diagnosticd",
      "Sandbox: nc(3) allow mach-lookup com.apple.system.opendirectoryd.libinfo",
      "Sandbox: deno(2) allow system-info net.link.addr",
      "Sandbox: deno(2) allow network-outbound /private/var/run/mDNSResponder",
      "Sandbox: nc(3) allow file-read-data /Users/me/.CFUserTextEncoding",
      "Sandbox: nc(3) allow file-read-data /dev/autofs_nowait",
    ] {
      assert!(seatbelt(line).unwrap().benign, "{line}");
    }
    for line in [
      // the program's doing: a preference file of its own, another service,
      // a write, another socket
      "Sandbox: deno(2) allow file-read-data /Library/Preferences/com.example.app.plist",
      "Sandbox: deno(2) allow mach-lookup com.apple.windowserver.active",
      "Sandbox: deno(2) allow file-write-data /Library/Preferences/Logging/x.plist",
      "Sandbox: deno(2) allow network-outbound /private/var/run/docker.sock",
      "Sandbox: git(9) allow file-read-data /Users/me/.gitconfig",
    ] {
      assert!(!seatbelt(line).unwrap().benign, "{line}");
    }
    let mut reporter = Reporter {
      seen: HashSet::new(),
      events: Vec::new(),
      total: 0,
      benign: 0,
      printed: 0,
      log: None,
      print: false,
    };
    reporter.report(
      seatbelt("Sandbox: deno(2) allow system-info net.link.addr").unwrap(),
    );
    reporter.report(
      seatbelt("Sandbox: git(9) allow file-read-data /Users/me/.gitconfig")
        .unwrap(),
    );
    let summary = reporter.summary();
    assert_eq!((summary.distinct, summary.benign), (1, 1));
    assert_eq!(summary.grants.len(), 1);
    // the record says so, and an ordinary one leaves the field out
    let line = Record::WouldDeny {
      v: WOULD_DENY_FORMAT,
      event: seatbelt("Sandbox: deno(2) allow system-info net.link.addr")
        .unwrap(),
    }
    .to_line();
    assert!(line.contains("\"benign\":true"), "{line}");
    assert_eq!(
      Record::from_line(&line),
      Some(Record::WouldDeny {
        v: WOULD_DENY_FORMAT,
        event: seatbelt("Sandbox: deno(2) allow system-info net.link.addr")
          .unwrap(),
      })
    );
  }

  #[test]
  fn seatbelt_lines_of_other_processes_are_ignored() {
    // no tag, another run's tag, a tag only as part of a longer line
    for message in [
      "Sandbox: cat(1) allow file-read-data /x".to_string(),
      "Sandbox: cat(1) allow file-read-data /x\ndenext-ks:other".to_string(),
      format!("Sandbox: cat(1) allow file-read-data /x\n{TAG}0"),
      // a refusal by someone's enforcing profile is not a report
      format!("Sandbox: cat(1) deny(1) file-read-data /x\n{TAG}"),
      format!("something else\n{TAG}"),
      String::new(),
    ] {
      assert_eq!(parse_seatbelt_report(&message, TAG), None, "{message}");
    }
  }

  #[test]
  fn tags_are_plain() {
    assert!(valid_tag("denext-ks:5f3a_1.2"));
    for bad in ["", "a b", "a\"b", "a)b", "a\nb", &"x".repeat(65)] {
      assert!(!valid_tag(bad), "{bad:?}");
    }
  }

  #[test]
  fn program_advisories_match_by_name() {
    assert!(
      program_advisory("/usr/bin/sudo")
        .unwrap()
        .contains("setuid")
    );
    assert!(program_advisory("git").unwrap().contains(".gitconfig"));
    assert!(
      program_advisory("C:\\Program Files\\Chrome\\chrome.exe")
        .unwrap()
        .contains("--no-sandbox")
    );
    assert!(
      program_advisory(
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
      )
      .unwrap()
      .contains("--no-sandbox")
    );
    assert_eq!(program_advisory("/bin/cat"), None);
    assert_eq!(program_advisory("deno"), None);
  }
}
