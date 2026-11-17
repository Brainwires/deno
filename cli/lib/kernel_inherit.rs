// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: the inherited kernel policy (DENEXT.md items 449, 450), shared
//! by the CLI and `denort` (a compiled program started inside a sandbox
//! honours it the same way).
//!
//! A process the kernel sandbox confines (`--kernel-sandbox=enforce`) hands
//! the `deno` processes it starts, and everything they start, the grants it
//! runs under: one [`GrantLayer`] per confined ancestor, in a carrier
//! descriptor (`deno_sandbox::inherit`: a sealed memfd on Linux, an
//! unlinked read-only file on macOS). A nested `deno` intersects its own
//! permission options with every layer ([`clamp`], the jail's
//! `clamp_permissions` generalised), so its checks refuse with `NotCapable`
//! what the kernel would refuse with a bare `EACCES`.
//!
//! The carrier is never trusted from the environment alone. A process
//! accepts it ([`load_at_startup`]) only when the descriptor is a carrier
//! (sealed / unlinked and read-only), the kernel reports the process
//! confined (Linux: no-new-privileges and a seccomp filter; macOS:
//! `sandbox_check`), and each layer's probe path is refused by the kernel
//! here. Anything else is ignored, with a warning. Even an accepted carrier
//! can only narrow: the intersection never adds a grant, never switches a
//! check, the kernel layer or a nested `enforce` off, and the kernel holds
//! the process to its domain whatever the carrier says.
//!
//! A program between the confined process and a `deno` may drop the carrier
//! (DENEXT.md item 450). `env -i` clears the variable but keeps the
//! descriptor: a process this runtime's sandbox holds (its marker,
//! `deno_sandbox::inherit::kernel_reports_marked`) then looks for the
//! carrier among its descriptors. Python's `close_fds` closes it: the
//! process then knows it is held, but not by what grant, and fails closed
//! ([`Inherited::Unknown`]): every permission is refused with `NotCapable`
//! and a note that says why. The kernel holds such a process all the same;
//! the marker only decides whether Deno's own checks refuse everything too,
//! so it is never a trust upgrade, and a process that fakes it only narrows
//! itself.

use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

use deno_runtime::deno_permissions::PermissionsOptions;
use serde::Deserialize;
use serde::Serialize;

/// Bumped whenever the serialized shape changes; a carrier of another
/// format is ignored.
pub const CARRIER_FORMAT: u32 = 1;

/// The most layers a carrier holds (one per confined ancestor that applied
/// a sandbox of its own; Landlock itself stacks at most 16 domains).
pub const MAX_LAYERS: usize = 16;

/// The grants one confined ancestor runs under, resolved where it ran
/// (absolute paths, so a child in another working directory reads them
/// alike). Each allow: `None` refused, an empty list everything, else the
/// entries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrantLayer {
  pub read: Option<Vec<PathBuf>>,
  pub write: Option<Vec<PathBuf>>,
  pub ffi: Option<Vec<PathBuf>>,
  /// Connecting: `--allow-net` and `--allow-net-connect` together.
  pub connect: Option<Vec<String>>,
  /// Listening: `--allow-net` and `--allow-net-listen` together.
  pub listen: Option<Vec<String>>,
  pub run: Option<Vec<String>>,
  /// The run entries resolved to programs (a nested `deno` naming the same
  /// program by another spelling keeps it).
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub run_paths: Vec<PathBuf>,
  pub env: Option<Vec<String>>,
  pub sys: Option<Vec<String>>,
  pub midi: Option<Vec<String>>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub deny_read: Vec<PathBuf>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub deny_write: Vec<PathBuf>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub deny_ffi: Vec<PathBuf>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub deny_net: Vec<String>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub deny_net_connect: Vec<String>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub deny_net_listen: Vec<String>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub deny_run: Vec<String>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub deny_env: Vec<String>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub deny_sys: Vec<String>,
  /// A path the ancestor's kernel sandbox refuses to read: a process that
  /// can still read it isn't under that sandbox, and ignores the carrier.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub probe: Option<PathBuf>,
}

/// What a carrier holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Carried {
  pub format: u32,
  pub layers: Vec<GrantLayer>,
}

impl Carried {
  pub fn to_bytes(&self) -> Vec<u8> {
    serde_json::to_vec(self).expect("a carrier serializes")
  }

  /// Parses a carrier; anything malformed is an error (the carrier is then
  /// ignored).
  pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
    let carried: Self = serde_json::from_slice(bytes)
      .map_err(|err| format!("it does not parse: {err}"))?;
    if carried.format != CARRIER_FORMAT {
      return Err(format!(
        "it has format {}, this runtime reads {CARRIER_FORMAT}",
        carried.format
      ));
    }
    if carried.layers.is_empty() || carried.layers.len() > MAX_LAYERS {
      return Err(format!("it has {} layers", carried.layers.len()));
    }
    Ok(carried)
  }
}

// ---------------------------------------------------------------------------
// Startup: accepting a carrier
// ---------------------------------------------------------------------------

/// What this process inherited from a confined ancestor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inherited {
  /// No ancestor's kernel sandbox of this runtime holds the process.
  None,
  /// The layers of an accepted carrier, and its descriptor when the
  /// processes this one starts are to be told it (`None`: several carriers
  /// were found, and each child looks for itself).
  Layers {
    layers: Vec<GrantLayer>,
    fd: Option<i32>,
  },
  /// This runtime's sandbox holds the process (its marker answers), but no
  /// carrier measures here: a program between them closed the descriptor.
  /// The grants are unknown, so every permission is refused.
  Unknown,
}

/// The layers this process accepted at startup.
static INHERITED: OnceLock<Vec<GrantLayer>> = OnceLock::new();

/// Whether this process is held by an ancestor's sandbox whose grants it
/// couldn't read ([`Inherited::Unknown`]).
static UNKNOWN: OnceLock<()> = OnceLock::new();

/// The layers this process accepted at startup (empty: none). A process
/// whose grants are unknown has one layer that grants nothing.
pub fn inherited_layers() -> &'static [GrantLayer] {
  INHERITED.get().map(|l| l.as_slice()).unwrap_or(&[])
}

/// Whether an ancestor's sandbox holds this process with grants it
/// couldn't read: every permission is then refused.
pub fn grants_unknown() -> bool {
  UNKNOWN.get().is_some()
}

/// The checks that decide whether a carrier's layers apply here: the kernel
/// says the process is confined, and each probe is refused.
pub struct Measure<'a> {
  pub confined: &'a dyn Fn() -> bool,
  pub refuses_read: &'a dyn Fn(&Path) -> bool,
}

/// What a process whose variable names no carrier that measures asks next:
/// whether this runtime's sandbox holds it anyway (its marker), and which
/// carriers its open descriptors hold.
pub struct Fallback<'a> {
  pub marked: &'a dyn Fn() -> bool,
  pub scan: &'a dyn Fn() -> Vec<(i32, Vec<u8>)>,
}

/// Accepts a carrier's bytes, measured with `measure` (see the module docs).
pub fn accept(
  bytes: &[u8],
  measure: &Measure<'_>,
) -> Result<Vec<GrantLayer>, String> {
  let carried = Carried::from_bytes(bytes)?;
  if !(measure.confined)() {
    return Err("the kernel doesn't report this process confined".to_string());
  }
  for layer in &carried.layers {
    if let Some(probe) = &layer.probe
      && !(measure.refuses_read)(probe)
    {
      return Err(format!(
        "{} is readable here, so the sandbox it describes doesn't hold this process",
        probe.display()
      ));
    }
  }
  Ok(carried.layers)
}

/// Decides what this process inherited: `var` is the carrier its variable
/// names (`Ok(None)`: no variable; `Err`: it names no carrier). The
/// variable's carrier is accepted when it measures. Otherwise, and only
/// when the marker says this runtime's sandbox holds the process, the open
/// descriptors are searched (an `env -i` launcher clears the variable but
/// keeps the descriptor): every carrier that measures applies, all of them
/// (their layers intersect); none at all means the grants are unknown. The
/// second value lists the warnings to print. Never a wider grant than a
/// carrier that measures: the marker only ever takes permissions away.
pub fn decide(
  var: Result<Option<(i32, Vec<u8>)>, String>,
  measure: &Measure<'_>,
  fallback: &Fallback<'_>,
) -> (Inherited, Vec<String>) {
  let mut warnings = Vec::new();
  match var {
    Ok(Some((fd, bytes))) => match accept(&bytes, measure) {
      Ok(layers) => {
        return (
          Inherited::Layers {
            layers,
            fd: Some(fd),
          },
          warnings,
        );
      }
      Err(why) => warnings.push(why),
    },
    Ok(None) => {}
    Err(why) => warnings.push(why),
  }
  if !(fallback.marked)() {
    return (Inherited::None, warnings);
  }
  let mut layers: Vec<GrantLayer> = Vec::new();
  let mut fds = Vec::new();
  for (fd, bytes) in (fallback.scan)() {
    // anything that isn't a policy, or doesn't measure, is passed over
    let Ok(found) = accept(&bytes, measure) else {
      continue;
    };
    fds.push(fd);
    for layer in found {
      if !layers.contains(&layer) {
        layers.push(layer);
      }
    }
  }
  if layers.is_empty() || layers.len() > MAX_LAYERS {
    return (Inherited::Unknown, warnings);
  }
  let fd = (fds.len() == 1).then(|| fds[0]);
  (Inherited::Layers { layers, fd }, warnings)
}

/// Reads and accepts the inherited carrier, first thing in `main` (after a
/// handed-over plan is applied): the CLI and `denort` alike. A carrier that
/// isn't accepted is ignored with a warning; the kernel holds the process to
/// its domain either way. A process this runtime's sandbox holds with no
/// carrier that measures refuses every permission ([`Inherited::Unknown`]).
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn load_at_startup() {
  use deno_sandbox::inherit;
  let var = inherit::read_inherited()
    .map(|c| c.map(|carrier| (carrier.fd, carrier.bytes)));
  let measure = Measure {
    confined: &inherit::kernel_reports_confined,
    refuses_read: &inherit::refuses_read,
  };
  let scan = || {
    inherit::scan_inherited(inherit::SCAN_MAX_FD)
      .into_iter()
      .map(|c| (c.fd, c.bytes))
      .collect()
  };
  let fallback = Fallback {
    marked: &inherit::kernel_reports_marked,
    scan: &scan,
  };
  let (inherited, warnings) = decide(var, &measure, &fallback);
  for why in &warnings {
    warn_ignored(why);
  }
  let layers = match inherited {
    Inherited::None => return,
    Inherited::Layers { layers, fd } => {
      if let Some(fd) = fd {
        // a child started with `clearEnv` or another environment still
        // gets the carrier (the descriptor itself is inherited)
        deno_runtime::deno_process::pin_child_env_var(
          inherit::POLICY_ENV_VAR.into(),
          fd.to_string().into(),
        );
      }
      layers
    }
    Inherited::Unknown => {
      #[allow(clippy::print_stderr, reason = "nothing else is set up yet")]
      {
        eprintln!(
          "warning: confined by an ancestor's kernel sandbox; grants unknown: a program between them closed the descriptor that hands them down, so every permission is refused (NotCapable). Keep that descriptor open in the programs between them (Python: subprocess's pass_fds) to run with the ancestor's grant."
        );
      }
      deno_runtime::deno_permissions::kernel_sandbox_state::set_grants_unknown(
      );
      let _ = UNKNOWN.set(());
      vec![GrantLayer::default()]
    }
  };
  deno_runtime::deno_permissions::kernel_sandbox_state::set_inherited();
  deno_runtime::deno_core::error::set_permission_denied_note(
    permission_denied_note,
  );
  let _ = INHERITED.set(layers);
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn load_at_startup() {}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn warn_ignored(why: &str) {
  #[allow(clippy::print_stderr, reason = "nothing else is set up yet")]
  {
    eprintln!(
      "warning: ignoring the kernel sandbox policy handed down by the starting process: {why}"
    );
  }
}

// ---------------------------------------------------------------------------
// The note on a refusal
// ---------------------------------------------------------------------------

/// The path a permission error names (`..., open '/x/y'`), if any.
#[cfg(any(target_os = "linux", test))]
fn quoted_path(message: &str) -> Option<&str> {
  let end = message.rfind('\'')?;
  let start = message[..end].rfind('\'')?;
  let path = &message[start + 1..end];
  path.starts_with('/').then_some(path)
}

#[cfg(target_os = "linux")]
fn mode_allows(path: &str) -> bool {
  use std::os::unix::ffi::OsStrExt;
  let check = |p: &Path, mode| {
    std::ffi::CString::new(p.as_os_str().as_bytes())
      // SAFETY: a valid path
      .is_ok_and(|c| unsafe { libc::access(c.as_ptr(), mode) } == 0)
  };
  let path = Path::new(path);
  if check(path, libc::F_OK) {
    // Landlock doesn't mediate access(2): a mode that allows reading and
    // writing can't explain the refusal
    return check(path, libc::R_OK | libc::W_OK);
  }
  // a path that doesn't exist yet: creating it needs the folder
  path
    .parent()
    .is_some_and(|parent| check(parent, libc::W_OK | libc::X_OK))
}

/// The note on a `PermissionDenied` error while a kernel sandbox holds the
/// process (its own, or one it inherited and accepted):
/// Linux: `EPERM` (seccomp's answer; a file mode gives `EACCES`), or
/// `EACCES` where the file's mode allows the access (`access(2)`, which
/// Landlock doesn't mediate) or the operation names no path (a socket);
/// macOS: `EPERM` (Seatbelt's answer; file modes give `EACCES`).
pub fn permission_denied_note(
  errno: Option<i32>,
  message: &str,
) -> Option<&'static str> {
  if !deno_runtime::deno_permissions::kernel_sandbox_state::is_confined() {
    return None;
  }
  let errno = errno?;
  let note =
    Some(deno_runtime::deno_permissions::kernel_sandbox_state::REFUSED_NOTE);
  #[cfg(unix)]
  if errno == libc::EPERM {
    return note;
  }
  #[cfg(target_os = "linux")]
  if errno == libc::EACCES {
    return match quoted_path(message) {
      Some(path) if !mode_allows(path) => None,
      _ => note,
    };
  }
  let _ = (message, errno, note);
  None
}

// ---------------------------------------------------------------------------
// Resolving paths as the kernel sees them
// ---------------------------------------------------------------------------

/// The existing part of `path` resolved (symlinks, `..`, the on-disk
/// spelling), and the rest: the components that don't exist yet. `decided`
/// is false when resolution stopped at something other than a missing
/// component (a folder that can't be searched, a symlink loop): the rest
/// may then still hold a link, and what the path names can't be told.
pub struct ResolvedPrefix {
  pub existing: PathBuf,
  pub rest: PathBuf,
  pub decided: bool,
}

impl ResolvedPrefix {
  pub fn full(&self) -> PathBuf {
    if self.rest.as_os_str().is_empty() {
      self.existing.clone()
    } else {
      self.existing.join(&self.rest)
    }
  }
}

/// Resolves the existing part of `path` ([`ResolvedPrefix`]).
#[allow(
  clippy::disallowed_methods,
  reason = "the kernel resolves the real file system's links, never a virtual one"
)]
pub fn resolve_prefix(path: &Path) -> ResolvedPrefix {
  let mut existing = path.to_path_buf();
  let mut rest = Vec::new();
  let mut decided = true;
  loop {
    match std::fs::canonicalize(&existing) {
      Ok(canonical) => {
        let mut tail = PathBuf::new();
        for part in rest.iter().rev() {
          tail.push(part);
        }
        return ResolvedPrefix {
          existing: canonical,
          rest: tail,
          decided,
        };
      }
      Err(err) => {
        if !matches!(
          err.kind(),
          std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
        ) {
          decided = false;
        }
      }
    }
    match (existing.file_name(), existing.parent()) {
      (Some(name), Some(parent)) => {
        rest.push(name.to_os_string());
        existing = parent.to_path_buf();
      }
      _ => {
        return ResolvedPrefix {
          existing: path.to_path_buf(),
          rest: PathBuf::new(),
          decided: false,
        };
      }
    }
  }
}

// ---------------------------------------------------------------------------
// A layer from this process's own grant
// ---------------------------------------------------------------------------

/// `entry` resolved against `cwd` through its existing prefix (symlinks,
/// `..`, the on-disk spelling), as the checks and the kernel see it.
fn resolve(cwd: &Path, entry: &str) -> PathBuf {
  let joined = cwd.join(entry);
  let normalized =
    deno_path_util::normalize_path(std::borrow::Cow::Borrowed(&joined))
      .into_owned();
  resolve_prefix(&normalized).full()
}

fn resolve_list(
  list: &Option<Vec<String>>,
  cwd: &Path,
) -> Option<Vec<PathBuf>> {
  list.as_ref().map(|l| {
    let mut out: Vec<PathBuf> = l.iter().map(|e| resolve(cwd, e)).collect();
    out.sort();
    out.dedup();
    out
  })
}

fn resolve_denies(list: &Option<Vec<String>>, cwd: &Path) -> Vec<PathBuf> {
  match list {
    // denied entirely: everything
    Some(l) if l.is_empty() => vec![PathBuf::from("/")],
    Some(_) => resolve_list(list, cwd).unwrap_or_default(),
    None => Vec::new(),
  }
}

fn strings(list: &Option<Vec<String>>) -> Vec<String> {
  let mut out = list.clone().unwrap_or_default();
  out.sort();
  out.dedup();
  out
}

/// The grants of one side of the network (`--allow-net` with one of the
/// direction-specific flags).
fn merged(
  both: &Option<Vec<String>>,
  side: &Option<Vec<String>>,
) -> Option<Vec<String>> {
  match (both, side) {
    (None, None) => None,
    (Some(l), _) | (_, Some(l)) if l.is_empty() => Some(Vec::new()),
    _ => {
      let mut out: Vec<String> =
        both.iter().chain(side.iter()).flatten().cloned().collect();
      out.sort();
      out.dedup();
      Some(out)
    }
  }
}

/// A program on `PATH` (or a path) resolved to the file that would run.
#[allow(
  clippy::disallowed_methods,
  reason = "the real programs a child would start, and the real PATH"
)]
fn resolve_program(cwd: &Path, name: &str) -> Option<PathBuf> {
  if name.contains('/') {
    return std::fs::canonicalize(cwd.join(name)).ok();
  }
  let path = std::env::var_os("PATH")?;
  std::env::split_paths(&path)
    .map(|dir| dir.join(name))
    .find(|p| p.is_file())
    .and_then(|p| std::fs::canonicalize(p).ok())
}

/// The layer that describes `options` (this process's final grant) for the
/// processes it starts, with the kernel sandbox's `probe`.
pub fn layer_for(
  options: &PermissionsOptions,
  cwd: &Path,
  probe: Option<PathBuf>,
) -> GrantLayer {
  let mut run_paths: Vec<PathBuf> = options
    .allow_run
    .iter()
    .flatten()
    .filter_map(|e| resolve_program(cwd, e))
    .collect();
  run_paths.sort();
  run_paths.dedup();
  let sorted = |l: &Option<Vec<String>>| {
    l.as_ref().map(|l| {
      let mut l = l.clone();
      l.sort();
      l.dedup();
      l
    })
  };
  GrantLayer {
    read: resolve_list(&options.allow_read, cwd),
    write: resolve_list(&options.allow_write, cwd),
    ffi: resolve_list(&options.allow_ffi, cwd),
    connect: merged(&options.allow_net, &options.allow_net_connect),
    listen: merged(&options.allow_net, &options.allow_net_listen),
    run: sorted(&options.allow_run),
    run_paths,
    env: sorted(&options.allow_env),
    sys: sorted(&options.allow_sys),
    midi: sorted(&options.allow_midi),
    deny_read: resolve_denies(&options.deny_read, cwd),
    deny_write: resolve_denies(&options.deny_write, cwd),
    deny_ffi: resolve_denies(&options.deny_ffi, cwd),
    deny_net: strings(&options.deny_net),
    deny_net_connect: strings(&options.deny_net_connect),
    deny_net_listen: strings(&options.deny_net_listen),
    deny_run: strings(&options.deny_run),
    deny_env: strings(&options.deny_env),
    deny_sys: strings(&options.deny_sys),
    probe,
  }
}

// ---------------------------------------------------------------------------
// The intersection
// ---------------------------------------------------------------------------

/// The intersection of an allow list with an ancestor's, given how one
/// entry covers another. Entries the ancestor covers are kept as written;
/// an entry wider than the ancestor's is replaced by the ancestor's entries
/// within it. Unchanged when the ancestor covers every entry (so clamping a
/// grant with its own layer changes nothing). Never an empty list, which
/// would mean everything.
fn intersect<P>(
  child: Option<Vec<String>>,
  parent: &Option<Vec<P>>,
  child_within_parent: impl Fn(&str, &P) -> bool,
  parent_within_child: impl Fn(&P, &str) -> bool,
  parent_text: impl Fn(&P) -> String,
) -> Option<Vec<String>> {
  let parent = match parent {
    None => return None,
    Some(p) if p.is_empty() => return child,
    Some(p) => p,
  };
  let child = child?;
  if child.is_empty() {
    let mut all: Vec<String> = parent.iter().map(&parent_text).collect();
    all.dedup();
    return Some(all);
  }
  let mut out: Vec<String> = Vec::new();
  for entry in child {
    if parent.iter().any(|p| child_within_parent(&entry, p)) {
      if !out.contains(&entry) {
        out.push(entry);
      }
      continue;
    }
    for p in parent.iter().filter(|p| parent_within_child(p, &entry)) {
      let text = parent_text(p);
      if !out.contains(&text) {
        out.push(text);
      }
    }
  }
  (!out.is_empty()).then_some(out)
}

fn path_text(p: &Path) -> String {
  p.to_string_lossy().into_owned()
}

fn clamp_paths(
  child: Option<Vec<String>>,
  parent: &Option<Vec<PathBuf>>,
  cwd: &Path,
) -> Option<Vec<String>> {
  intersect(
    child,
    parent,
    |c, p| resolve(cwd, c).starts_with(p),
    |p, c| p.starts_with(resolve(cwd, c)),
    |p| path_text(p),
  )
}

/// A network entry's host and port (`host`, `host:port`, `[v6]:port`);
/// `unix:` / `vsock:` entries are whole.
fn host_port(entry: &str) -> (String, Option<&str>) {
  if entry.starts_with("unix:") || entry.starts_with("vsock:") {
    return (entry.to_string(), None);
  }
  if let Some(rest) = entry.strip_prefix('[')
    && let Some((host, after)) = rest.split_once(']')
  {
    let port = after.strip_prefix(':').filter(|p| !p.is_empty());
    return (host.to_ascii_lowercase(), port);
  }
  match entry.rsplit_once(':') {
    Some((host, port))
      if !host.contains(':')
        && !port.is_empty()
        && port.bytes().all(|b| b.is_ascii_digit() || b == b'-') =>
    {
      (host.to_ascii_lowercase(), Some(port))
    }
    _ => (entry.to_ascii_lowercase(), None),
  }
}

/// Whether network entry `inner` lies within `outer` (the same host; any
/// port when `outer` names none).
fn net_within(inner: &str, outer: &str) -> bool {
  if inner == outer {
    return true;
  }
  let (ih, ip) = host_port(inner);
  let (oh, op) = host_port(outer);
  ih == oh && (op.is_none() || ip == op)
}

fn clamp_net(
  child: Option<Vec<String>>,
  parent: &Option<Vec<String>>,
) -> Option<Vec<String>> {
  intersect(
    child,
    parent,
    |c, p| net_within(c, p),
    |p, c| net_within(p, c),
    |p| p.clone(),
  )
}

/// Whether every entry of `child` lies within `parent`.
fn net_side_within(
  child: &Option<Vec<String>>,
  parent: &Option<Vec<String>>,
) -> bool {
  match (child, parent) {
    (None, _) => true,
    (Some(_), None) => false,
    (_, Some(p)) if p.is_empty() => true,
    (Some(c), Some(_)) if c.is_empty() => false,
    (Some(c), Some(p)) => c.iter().all(|c| p.iter().any(|p| net_within(c, p))),
  }
}

/// Whether variable name pattern `inner` (`NAME` or `PREFIX*`) lies within
/// `outer`.
fn env_within(inner: &str, outer: &str) -> bool {
  match outer.strip_suffix('*') {
    Some(prefix) => inner.starts_with(prefix),
    None => inner == outer,
  }
}

fn clamp_names(
  child: Option<Vec<String>>,
  parent: &Option<Vec<String>>,
  within: impl Fn(&str, &str) -> bool + Copy,
) -> Option<Vec<String>> {
  intersect(
    child,
    parent,
    |c, p| within(c, p),
    |p, c| within(p, c),
    |p| p.clone(),
  )
}

fn clamp_run(
  child: Option<Vec<String>>,
  layer: &GrantLayer,
  cwd: &Path,
) -> Option<Vec<String>> {
  let same = |c: &str, p: &str| {
    c == p
      || resolve_program(cwd, c).is_some_and(|c| {
        layer.run_paths.contains(&c)
          && resolve_program(cwd, p).is_none_or(|p| p == c)
      })
  };
  intersect(
    child,
    &layer.run,
    |c, p: &String| same(c, p),
    |p: &String, c| same(c, p),
    |p| p.clone(),
  )
}

/// Adds `extra` path denies to `list`, skipping those an entry there
/// already covers (so a deny list that holds them is unchanged).
fn add_path_denies(
  list: &mut Option<Vec<String>>,
  extra: &[PathBuf],
  cwd: &Path,
) {
  if extra.is_empty() {
    return;
  }
  if list.as_ref().is_some_and(|l| l.is_empty()) {
    // denied entirely already
    return;
  }
  let mut entries = list.take().unwrap_or_default();
  let mut resolved: Vec<PathBuf> =
    entries.iter().map(|e| resolve(cwd, e)).collect();
  for path in extra {
    if resolved.iter().any(|r| path.starts_with(r)) {
      continue;
    }
    entries.push(path_text(path));
    resolved.push(path.clone());
  }
  *list = Some(entries);
}

/// Adds `extra` name denies to `list`, skipping those an entry there
/// already covers.
fn add_name_denies(
  list: &mut Option<Vec<String>>,
  extra: &[String],
  within: impl Fn(&str, &str) -> bool,
) {
  if extra.is_empty() || list.as_ref().is_some_and(|l| l.is_empty()) {
    return;
  }
  let mut entries = list.take().unwrap_or_default();
  for name in extra {
    if !entries.iter().any(|e| within(name, e)) {
      entries.push(name.clone());
    }
  }
  *list = Some(entries);
}

/// Intersects `options` with every inherited layer (see the module docs);
/// relative entries resolve against `cwd`. Prompting is turned off: inside
/// a confined tree a prompt could only grant what the kernel refuses.
pub fn clamp(
  options: &mut PermissionsOptions,
  layers: &[GrantLayer],
  cwd: &Path,
) {
  if layers.is_empty() {
    return;
  }
  for layer in layers {
    clamp_one(options, layer, cwd);
  }
  options.prompt = false;
}

fn clamp_one(options: &mut PermissionsOptions, layer: &GrantLayer, cwd: &Path) {
  let o = options;
  o.allow_read = clamp_paths(o.allow_read.take(), &layer.read, cwd);
  o.allow_write = clamp_paths(o.allow_write.take(), &layer.write, cwd);
  o.allow_ffi = clamp_paths(o.allow_ffi.take(), &layer.ffi, cwd);
  // the network by direction: a grant the layer covers stays as written;
  // otherwise both directions are clamped on their own
  let connect = merged(&o.allow_net, &o.allow_net_connect);
  let listen = merged(&o.allow_net, &o.allow_net_listen);
  if !net_side_within(&connect, &layer.connect)
    || !net_side_within(&listen, &layer.listen)
  {
    o.allow_net = None;
    o.allow_net_connect = clamp_net(connect, &layer.connect);
    o.allow_net_listen = clamp_net(listen, &layer.listen);
  }
  o.allow_run = clamp_run(o.allow_run.take(), layer, cwd);
  o.allow_env = clamp_names(o.allow_env.take(), &layer.env, env_within);
  o.allow_sys = clamp_names(o.allow_sys.take(), &layer.sys, |a, b| a == b);
  o.allow_midi = clamp_names(o.allow_midi.take(), &layer.midi, |a, b| a == b);
  add_path_denies(&mut o.deny_read, &layer.deny_read, cwd);
  add_path_denies(&mut o.deny_write, &layer.deny_write, cwd);
  add_path_denies(&mut o.deny_ffi, &layer.deny_ffi, cwd);
  add_name_denies(&mut o.deny_net, &layer.deny_net, net_within);
  add_name_denies(&mut o.deny_net_connect, &layer.deny_net_connect, net_within);
  add_name_denies(&mut o.deny_net_listen, &layer.deny_net_listen, net_within);
  add_name_denies(&mut o.deny_run, &layer.deny_run, |a, b| a == b);
  add_name_denies(&mut o.deny_env, &layer.deny_env, env_within);
  add_name_denies(&mut o.deny_sys, &layer.deny_sys, |a, b| a == b);
}

#[cfg(test)]
mod tests {
  use super::*;

  fn opts() -> PermissionsOptions {
    PermissionsOptions {
      prompt: true,
      ..Default::default()
    }
  }

  #[allow(clippy::disallowed_methods, reason = "a real folder for the tests")]
  fn tmp() -> PathBuf {
    resolve_prefix(&std::env::temp_dir()).full()
  }

  fn layer(read: Option<Vec<&str>>) -> GrantLayer {
    GrantLayer {
      read: read.map(|l| l.into_iter().map(PathBuf::from).collect()),
      ..Default::default()
    }
  }

  fn s(list: &[&str]) -> Option<Vec<String>> {
    Some(list.iter().map(|x| x.to_string()).collect())
  }

  #[test]
  fn allow_all_becomes_the_ancestors_grant() {
    let root = tmp();
    let inside = root.join("inside");
    let mut o = opts();
    o.allow_read = Some(vec![]);
    o.allow_write = Some(vec![]);
    o.allow_net = Some(vec![]);
    o.allow_run = Some(vec![]);
    o.allow_env = Some(vec![]);
    o.allow_ffi = Some(vec![]);
    o.allow_sys = Some(vec![]);
    let l = GrantLayer {
      read: Some(vec![inside.clone()]),
      write: None,
      connect: s(&["127.0.0.1:4545"]),
      listen: None,
      run: s(&["git"]),
      env: s(&["HOME"]),
      sys: s(&["hostname"]),
      ..Default::default()
    };
    clamp(&mut o, &[l], &root);
    assert_eq!(o.allow_read, Some(vec![inside.to_string_lossy().into()]));
    assert_eq!(o.allow_write, None);
    assert_eq!(o.allow_net, None);
    assert_eq!(o.allow_net_connect, s(&["127.0.0.1:4545"]));
    assert_eq!(o.allow_net_listen, None);
    assert_eq!(o.allow_run, s(&["git"]));
    assert_eq!(o.allow_env, s(&["HOME"]));
    assert_eq!(o.allow_ffi, None);
    assert_eq!(o.allow_sys, s(&["hostname"]));
    assert!(!o.prompt);
  }

  #[test]
  fn entries_keep_their_spelling_or_narrow_to_the_ancestors() {
    let root = tmp();
    let a = root.join("a");
    let mut o = opts();
    // relative and within: kept as written; wider: narrowed; outside:
    // dropped
    o.allow_read = s(&["a/x", "/", "/nonexistent-elsewhere"]);
    clamp(&mut o, &[layer(Some(vec![a.to_str().unwrap()]))], &root);
    assert_eq!(o.allow_read, Some(vec!["a/x".into(), path_text(&a)]));
    // nothing left: refused, never an empty list
    let mut o = opts();
    o.allow_read = s(&["/nonexistent-elsewhere"]);
    clamp(&mut o, &[layer(Some(vec![a.to_str().unwrap()]))], &root);
    assert_eq!(o.allow_read, None);
    // the ancestor read nothing: nothing
    let mut o = opts();
    o.allow_read = Some(vec![]);
    clamp(&mut o, &[layer(None)], &root);
    assert_eq!(o.allow_read, None);
    // the ancestor read everything: unchanged
    let mut o = opts();
    o.allow_read = s(&["a"]);
    clamp(&mut o, &[layer(Some(vec![]))], &root);
    assert_eq!(o.allow_read, s(&["a"]));
  }

  #[test]
  fn network_by_direction_and_port() {
    let cwd = tmp();
    let l = GrantLayer {
      connect: s(&["127.0.0.1:443", "example.com"]),
      listen: s(&["0.0.0.0:8000"]),
      ..Default::default()
    };
    // covered: unchanged, still --allow-net
    let mut o = opts();
    o.allow_net_connect = s(&["example.com:443"]);
    clamp(&mut o, std::slice::from_ref(&l), &cwd);
    assert_eq!(o.allow_net_connect, s(&["example.com:443"]));
    assert_eq!(o.allow_net, None);
    // --allow-net=127.0.0.1 is both directions: connect narrows to the
    // port, listen to nothing
    let mut o = opts();
    o.allow_net = s(&["127.0.0.1"]);
    clamp(&mut o, std::slice::from_ref(&l), &cwd);
    assert_eq!(o.allow_net, None);
    assert_eq!(o.allow_net_connect, s(&["127.0.0.1:443"]));
    assert_eq!(o.allow_net_listen, None);
    // unscoped
    let mut o = opts();
    o.allow_net = Some(vec![]);
    clamp(&mut o, std::slice::from_ref(&l), &cwd);
    assert_eq!(o.allow_net_connect, s(&["127.0.0.1:443", "example.com"]));
    assert_eq!(o.allow_net_listen, s(&["0.0.0.0:8000"]));
    assert_eq!(host_port("[::1]:80"), ("::1".to_string(), Some("80")));
    assert_eq!(host_port("unix:/a:b"), ("unix:/a:b".to_string(), None));
    assert!(net_within("Example.com:443", "example.com"));
    assert!(!net_within("example.com", "example.com:443"));
    assert!(!net_within("evil.com:443", "example.com"));
  }

  #[test]
  fn env_patterns_and_names() {
    let cwd = tmp();
    let l = GrantLayer {
      env: s(&["DENO_*", "HOME"]),
      sys: s(&["hostname"]),
      ..Default::default()
    };
    let mut o = opts();
    o.allow_env = s(&["DENO_DIR", "*", "SECRET"]);
    o.allow_sys = s(&["hostname", "osRelease"]);
    clamp(&mut o, &[l], &cwd);
    assert_eq!(o.allow_env, s(&["DENO_DIR", "DENO_*", "HOME"]));
    assert_eq!(o.allow_sys, s(&["hostname"]));
  }

  #[test]
  fn denies_are_added_once() {
    let root = tmp();
    let secret = root.join("secret");
    let l = GrantLayer {
      read: Some(vec![]),
      deny_read: vec![secret.clone()],
      deny_env: vec!["TOKEN".into()],
      deny_net: vec!["10.0.0.1".into()],
      ..Default::default()
    };
    let mut o = opts();
    o.allow_read = Some(vec![]);
    clamp(&mut o, std::slice::from_ref(&l), &root);
    assert_eq!(o.deny_read, Some(vec![path_text(&secret)]));
    assert_eq!(o.deny_env, s(&["TOKEN"]));
    assert_eq!(o.deny_net, s(&["10.0.0.1"]));
    // a deny already covered (relative, or a wider one) isn't added
    let mut o = opts();
    o.allow_read = Some(vec![]);
    o.deny_read = s(&["secret"]);
    clamp(&mut o, std::slice::from_ref(&l), &root);
    assert_eq!(o.deny_read, s(&["secret"]));
    // denied entirely stays so
    let mut o = opts();
    o.deny_read = Some(vec![]);
    clamp(&mut o, &[l], &root);
    assert_eq!(o.deny_read, Some(vec![]));
  }

  // a program on PATH (`sh`): the carrier is Linux and macOS only
  #[cfg(unix)]
  #[test]
  fn run_entries_match_by_name_or_program() {
    let cwd = tmp();
    let l = GrantLayer {
      run: s(&["sh"]),
      run_paths: resolve_program(&cwd, "sh").into_iter().collect(),
      ..Default::default()
    };
    let mut o = opts();
    let sh = resolve_program(&cwd, "sh").unwrap();
    o.allow_run = Some(vec![path_text(&sh), "curl".into()]);
    clamp(&mut o, std::slice::from_ref(&l), &cwd);
    assert_eq!(o.allow_run, Some(vec![path_text(&sh)]));
    // no run grant above: none below
    let mut o = opts();
    o.allow_run = Some(vec![]);
    clamp(&mut o, &[GrantLayer::default()], &cwd);
    assert_eq!(o.allow_run, None);
  }

  /// The re-executed image clamps with its own layer as well; that must not
  /// change its grant, or it would compile another plan.
  #[test]
  fn clamping_with_its_own_layer_changes_nothing() {
    let root = tmp();
    let shapes: Vec<PermissionsOptions> = vec![
      PermissionsOptions {
        allow_read: s(&[".", "/usr"]),
        allow_write: s(&["./out"]),
        deny_read: s(&["./.env"]),
        allow_net: s(&["127.0.0.1:4545", "example.com"]),
        allow_run: s(&["sh"]),
        allow_env: s(&["HOME", "DENO_*"]),
        prompt: false,
        ..Default::default()
      },
      PermissionsOptions {
        allow_read: Some(vec![]),
        allow_write: Some(vec![]),
        allow_net: Some(vec![]),
        allow_env: Some(vec![]),
        allow_run: Some(vec![]),
        allow_ffi: Some(vec![]),
        allow_sys: Some(vec![]),
        deny_read: s(&["/etc/shadow", "x"]),
        deny_net: s(&["10.0.0.1"]),
        prompt: false,
        ..Default::default()
      },
      PermissionsOptions {
        allow_net_connect: s(&["a.com:443"]),
        allow_net_listen: s(&["0.0.0.0:8000"]),
        allow_sys: s(&["hostname"]),
        deny_write: Some(vec![]),
        prompt: false,
        ..Default::default()
      },
      PermissionsOptions {
        prompt: false,
        ..Default::default()
      },
    ];
    for shape in shapes {
      let own = layer_for(&shape, &root, Some(PathBuf::from("/")));
      let mut clamped = shape.clone();
      clamp(&mut clamped, std::slice::from_ref(&own), &root);
      assert_eq!(clamped, shape);
      // and the layer of the result is the same layer
      assert_eq!(layer_for(&clamped, &root, Some(PathBuf::from("/"))), own);
    }
  }

  #[test]
  fn several_layers_intersect() {
    let root = tmp();
    let a = root.join("a");
    let ab = a.join("b");
    let mut o = opts();
    o.allow_read = Some(vec![]);
    clamp(
      &mut o,
      &[
        layer(Some(vec![a.to_str().unwrap()])),
        layer(Some(vec![ab.to_str().unwrap()])),
      ],
      &root,
    );
    assert_eq!(o.allow_read, Some(vec![path_text(&ab)]));
  }

  #[test]
  fn a_carrier_is_accepted_only_where_it_measures() {
    let carried = Carried {
      format: CARRIER_FORMAT,
      layers: vec![GrantLayer {
        probe: Some(PathBuf::from("/probe")),
        ..Default::default()
      }],
    };
    let bytes = carried.to_bytes();
    let yes = || true;
    let no = || false;
    let refused = |_: &Path| true;
    let readable = |_: &Path| false;
    let ok = Measure {
      confined: &yes,
      refuses_read: &refused,
    };
    assert_eq!(accept(&bytes, &ok).unwrap(), carried.layers);
    // the kernel doesn't report the process confined: ignored
    let unconfined = Measure {
      confined: &no,
      refuses_read: &refused,
    };
    assert!(
      accept(&bytes, &unconfined)
        .unwrap_err()
        .contains("confined")
    );
    // the probe opens: the sandbox it describes isn't this one
    let open = Measure {
      confined: &yes,
      refuses_read: &readable,
    };
    assert!(accept(&bytes, &open).unwrap_err().contains("/probe"));
    // malformed, another format, no layers, too many
    assert!(accept(b"{", &ok).is_err());
    let mut other = carried.clone();
    other.format = 2;
    assert!(accept(&other.to_bytes(), &ok).is_err());
    other.format = CARRIER_FORMAT;
    other.layers.clear();
    assert!(accept(&other.to_bytes(), &ok).is_err());
    other.layers = vec![GrantLayer::default(); MAX_LAYERS + 1];
    assert!(accept(&other.to_bytes(), &ok).is_err());
    // unknown fields are refused
    assert!(accept(br#"{"format":1,"layers":[{}],"skip":true}"#, &ok).is_err());
  }

  // the note is a no-op unless a kernel sandbox holds this process (nothing
  // in the unit tests confines it)
  #[cfg(unix)]
  #[test]
  fn note_only_while_confined() {
    assert!(
      !deno_runtime::deno_permissions::kernel_sandbox_state::is_confined()
    );
    for errno in [libc::EPERM, libc::EACCES] {
      assert_eq!(
        permission_denied_note(Some(errno), "denied (os error 1): open '/x'"),
        None
      );
    }
    assert_eq!(permission_denied_note(None, "denied"), None);
  }

  #[test]
  fn quoted_paths() {
    assert_eq!(
      quoted_path("Permission denied (os error 13): open '/a/b c'"),
      Some("/a/b c")
    );
    assert_eq!(quoted_path("Permission denied (os error 13)"), None);
    assert_eq!(quoted_path("spawn 'probe'"), None);
  }

  fn carried(layers: Vec<GrantLayer>) -> Vec<u8> {
    Carried {
      format: CARRIER_FORMAT,
      layers,
    }
    .to_bytes()
  }

  fn yes() -> bool {
    true
  }

  fn refused(_: &Path) -> bool {
    true
  }

  const CONFINED: Measure<'static> = Measure {
    confined: &yes,
    refuses_read: &refused,
  };

  fn fallback<'a>(
    marked: &'a dyn Fn() -> bool,
    scan: &'a dyn Fn() -> Vec<(i32, Vec<u8>)>,
  ) -> Fallback<'a> {
    Fallback { marked, scan }
  }

  // the variable's carrier, when it measures, is all that counts: no scan
  #[test]
  fn the_variables_carrier_wins() {
    let bytes = carried(vec![layer(Some(vec!["/a"]))]);
    let scan = || -> Vec<(i32, Vec<u8>)> { panic!("no scan") };
    let (got, warnings) =
      decide(Ok(Some((7, bytes))), &CONFINED, &fallback(&|| true, &scan));
    assert_eq!(
      got,
      Inherited::Layers {
        layers: vec![layer(Some(vec!["/a"]))],
        fd: Some(7)
      }
    );
    assert!(warnings.is_empty());
  }

  // unmarked: no carrier means nothing inherited, a bad one a warning; the
  // descriptors aren't searched (another sandbox, or none)
  #[test]
  fn unmarked_processes_inherit_nothing() {
    let scan = || -> Vec<(i32, Vec<u8>)> { panic!("no scan") };
    let fb = fallback(&|| false, &scan);
    assert_eq!(decide(Ok(None), &CONFINED, &fb), (Inherited::None, vec![]));
    let (got, warnings) = decide(Err("not a carrier".into()), &CONFINED, &fb);
    assert_eq!(got, Inherited::None);
    assert_eq!(warnings, vec!["not a carrier".to_string()]);
    // a carrier that doesn't measure is ignored, as before
    let unconfined = Measure {
      confined: &|| false,
      refuses_read: &|_| true,
    };
    let bytes = carried(vec![layer(Some(vec!["/a"]))]);
    let (got, warnings) = decide(Ok(Some((7, bytes))), &unconfined, &fb);
    assert_eq!(got, Inherited::None);
    assert_eq!(warnings.len(), 1);
  }

  // `env -i`: marked, no variable, the descriptor still open
  #[test]
  fn a_cleared_variable_is_found_by_its_descriptor() {
    let bytes = carried(vec![layer(Some(vec!["/a"]))]);
    let scan = move || vec![(5, b"{}".to_vec()), (9, bytes.clone())];
    let (got, _) = decide(Ok(None), &CONFINED, &fallback(&|| true, &scan));
    assert_eq!(
      got,
      Inherited::Layers {
        layers: vec![layer(Some(vec!["/a"]))],
        fd: Some(9)
      }
    );
    // two carriers: both apply, and no single descriptor is handed down
    let a = carried(vec![layer(Some(vec!["/a"]))]);
    let b = carried(vec![layer(Some(vec!["/b"]))]);
    let scan = move || vec![(5, a.clone()), (9, b.clone())];
    let (got, _) = decide(Ok(None), &CONFINED, &fallback(&|| true, &scan));
    assert_eq!(
      got,
      Inherited::Layers {
        layers: vec![layer(Some(vec!["/a"])), layer(Some(vec!["/b"]))],
        fd: None
      }
    );
  }

  // Python's close_fds: marked and nothing found, or nothing that
  // measures, or a variable naming no carrier: the grants are unknown
  #[test]
  fn a_closed_descriptor_fails_closed() {
    let none = || Vec::new();
    let fb = fallback(&|| true, &none);
    assert_eq!(decide(Ok(None), &CONFINED, &fb).0, Inherited::Unknown);
    assert_eq!(
      decide(Err("descriptor 9 is not open".into()), &CONFINED, &fb).0,
      Inherited::Unknown
    );
    let open = Measure {
      confined: &|| true,
      refuses_read: &|_| false,
    };
    let mut probed = layer(Some(vec!["/a"]));
    probed.probe = Some(PathBuf::from("/probe"));
    let bytes = carried(vec![probed]);
    let scan = move || vec![(9, bytes.clone())];
    assert_eq!(
      decide(Ok(None), &open, &fallback(&|| true, &scan)).0,
      Inherited::Unknown
    );
  }

  // grants unknown: the layer that stands for them grants nothing, so even
  // `-A` ends with no permission at all, and prompting off
  #[test]
  fn unknown_grants_refuse_everything() {
    let mut o = opts();
    o.allow_read = Some(vec![]);
    o.allow_write = Some(vec![]);
    o.allow_net = Some(vec![]);
    o.allow_env = Some(vec![]);
    o.allow_run = Some(vec![]);
    o.allow_ffi = Some(vec![]);
    o.allow_sys = Some(vec![]);
    clamp(&mut o, &[GrantLayer::default()], &tmp());
    assert_eq!(o.allow_read, None);
    assert_eq!(o.allow_write, None);
    assert_eq!(o.allow_net, None);
    assert_eq!(o.allow_net_connect, None);
    assert_eq!(o.allow_net_listen, None);
    assert_eq!(o.allow_env, None);
    assert_eq!(o.allow_run, None);
    assert_eq!(o.allow_ffi, None);
    assert_eq!(o.allow_sys, None);
    assert!(!o.prompt);
  }
}
