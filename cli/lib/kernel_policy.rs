// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: the kernel-sandbox policy model (DENEXT.md item 420). Pure data,
//! no enforcement.
//!
//! [`KernelPolicy::compile`] turns a run's [`PermissionsOptions`] plus the
//! runtime's own needs ([`ImplicitSet`]) and the machine ([`Context`],
//! [`Capabilities`]) into what each OS mechanism would apply, and records a
//! [`PolicyGap`] for every Deno rule a mechanism can't express exactly, with
//! the explicit choice made for it:
//!
//! - [`GapResolution::FailOpen`]: the kernel allows more than Deno does; Deno's
//!   check still holds for Deno's own APIs, native code and children are not
//!   held to the rule. `--kernel-sandbox=enforce` refuses to start on one.
//! - [`GapResolution::FailClosed`]: the kernel allows less than Deno does
//!   (something Deno would allow fails with a kernel error instead).
//! - [`GapResolution::Documented`]: outside what the kernel layer promises by
//!   design (`env`, `sys`, metadata, exec of a scoped `--allow-run`); never
//!   fatal.
//!
//! The rules follow plans/kernel-enforcement.md §1 as amended by its review:
//! host-scoped network is a broker (Mode B) feature on Linux from the start;
//! Unix sockets are allowed only for an explicit `unix:` grant or `-A` with
//! denies; a read deny inside a read allow on Linux is enforced by expansion
//! only when the expansion is stable, by overmounting where user namespaces
//! exist, and is otherwise a reported Deno-only rule.

#![allow(
  clippy::disallowed_methods,
  reason = "the policy is compiled for the real file system and environment (the reach of a deny, the programs on PATH), never a virtual one"
)]

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;

use deno_path_util::normalize_path;
use deno_runtime::deno_permissions::Host;
use deno_runtime::deno_permissions::NetDescriptor;
use deno_runtime::deno_permissions::PermissionsOptions;
use serde::Deserialize;
use serde::Serialize;

use crate::kernel_sandbox::KernelSandboxMode;

/// Bumped whenever the serialized shape changes (part of the policy hash).
pub const POLICY_FORMAT: u32 = 1;

/// Whether this runtime's supervising broker (Mode B) has the file
/// operations: a write deny inside a write allow on Linux is then held
/// exactly (the kernel refuses changes directly in the folders that lead to
/// the deny; the broker makes them for Deno's own operations).
pub const BROKER_FS_BUILT: bool = true;

/// Whether the broker has the network side: connecting and binding by
/// host, name resolution and Unix sockets by path are the broker's
/// (`deno_sandbox::net_authority` over Deno's own checks), and the confined
/// process makes no IP socket of its own. What it doesn't carry fails
/// closed (a module fetched after start, the runtime's own services), so
/// nothing is left to Deno's checks alone.
pub const BROKER_NET_BUILT: bool = true;

/// Whether this runtime overmounts denied paths in a user + mount namespace
/// (the exact Linux read deny). Not yet: the model falls back to expansion
/// or a Deno-only rule.
pub const OVERMOUNT_BUILT: bool = false;

// ---------------------------------------------------------------------------
// The machine: OS, mechanisms and the per-OS / per-ABI capability table
// ---------------------------------------------------------------------------

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
pub enum TargetOs {
  Linux,
  Macos,
  Windows,
  Other,
}

impl TargetOs {
  pub fn current() -> Self {
    if cfg!(target_os = "linux") {
      Self::Linux
    } else if cfg!(target_os = "macos") {
      Self::Macos
    } else if cfg!(windows) {
      Self::Windows
    } else {
      Self::Other
    }
  }

  pub fn as_str(self) -> &'static str {
    match self {
      Self::Linux => "linux",
      Self::Macos => "macos",
      Self::Windows => "windows",
      Self::Other => "other",
    }
  }
}

/// An OS confinement mechanism.
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
pub enum Mechanism {
  Landlock,
  Seccomp,
  /// Unprivileged user + mount namespaces (overmounting denied paths).
  UserNamespace,
  Seatbelt,
  /// Windows job objects (tree containment, no children).
  JobObject,
  /// Windows: lowering the token's integrity level (write confinement).
  LowIntegrity,
}

/// What a mechanism can hold.
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
pub enum Feature {
  /// Path-scoped reads.
  FsRead,
  /// Path-scoped writes (content, create, remove).
  FsWrite,
  /// Path-scoped execution.
  FsExec,
  /// Cross-directory rename and link within write scopes.
  FsRefer,
  /// Truncation held to the write scope.
  FsTruncate,
  /// Device ioctls held to the write scope.
  FsIoctlDev,
  /// A deny below an allow, exactly.
  DenyInsideAllow,
  /// Path-scoped metadata reads (stat and friends).
  MetadataRead,
  /// Path-scoped loading of executable code (FFI, addons, dyld).
  MapExecutable,
  /// TCP connect and bind by port.
  TcpPorts,
  /// Socket family / type gating (no UDP, no raw, no AF_UNIX).
  SocketFamilies,
  /// Unix sockets by path.
  UnixSocketPaths,
  /// Abstract Unix sockets scoped to the sandbox.
  ScopeAbstractUnix,
  /// Signals scoped to the sandbox.
  ScopeSignal,
  /// Denials written to an audit log.
  AuditLog,
  /// Refusing to start programs.
  ExecRefusal,
  /// Narrowing system information (sysctl).
  SysctlNarrowing,
  /// A kernel attack-surface syscall list.
  AttackSurface,
  /// Hiding denied paths by overmounting them (exact read denies).
  Overmount,
  /// The process tree can't escape (killed together).
  ChildContainment,
  /// Writes held to labelled directories.
  WriteConfinement,
}

/// One row of the capability table: on `os`, `mechanism` at level `min`
/// (the Landlock ABI; `0` for mechanisms without levels) provides `feature`.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityRow {
  pub os: TargetOs,
  pub mechanism: Mechanism,
  pub min: u32,
  pub feature: Feature,
  pub note: &'static str,
}

const fn row(
  os: TargetOs,
  mechanism: Mechanism,
  min: u32,
  feature: Feature,
  note: &'static str,
) -> CapabilityRow {
  CapabilityRow {
    os,
    mechanism,
    min,
    feature,
    note,
  }
}

/// The capability table: what each OS mechanism (and each Landlock ABI) can
/// hold. The guarantee matrix and every per-ABI decision are read from here.
pub static CAPABILITY_TABLE: &[CapabilityRow] = {
  use Feature::*;
  use Mechanism::*;
  use TargetOs::*;
  &[
    // Linux: Landlock, by ABI
    row(
      Linux,
      Landlock,
      1,
      FsRead,
      "read rights on path-beneath rules",
    ),
    row(
      Linux,
      Landlock,
      1,
      FsWrite,
      "write rights on path-beneath rules",
    ),
    row(Linux, Landlock, 1, FsExec, "the execute right"),
    row(
      Linux,
      Landlock,
      2,
      FsRefer,
      "REFER: before it every cross-directory rename/link is refused",
    ),
    row(Linux, Landlock, 3, FsTruncate, "TRUNCATE"),
    row(
      Linux,
      Landlock,
      4,
      TcpPorts,
      "TCP bind/connect by port, no address",
    ),
    row(Linux, Landlock, 5, FsIoctlDev, "IOCTL_DEV"),
    row(
      Linux,
      Landlock,
      6,
      ScopeAbstractUnix,
      "LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET",
    ),
    row(Linux, Landlock, 6, ScopeSignal, "LANDLOCK_SCOPE_SIGNAL"),
    row(Linux, Landlock, 7, AuditLog, "denials in the audit log"),
    // Linux: seccomp
    row(
      Linux,
      Seccomp,
      0,
      SocketFamilies,
      "socket family/type/protocol",
    ),
    row(Linux, Seccomp, 0, ExecRefusal, "execve/execveat refused"),
    row(Linux, Seccomp, 0, AttackSurface, "Docker-derived denylist"),
    // Linux: user + mount namespaces
    row(
      Linux,
      UserNamespace,
      0,
      Overmount,
      "an empty mount over each denied path",
    ),
    // macOS: Seatbelt
    row(Macos, Seatbelt, 0, FsRead, "file-read* by subpath/literal"),
    row(
      Macos,
      Seatbelt,
      0,
      FsWrite,
      "file-write* by subpath/literal",
    ),
    row(Macos, Seatbelt, 0, FsExec, "process-exec by literal"),
    row(Macos, Seatbelt, 0, FsRefer, "renames are file-write*"),
    row(Macos, Seatbelt, 0, FsTruncate, "truncation is file-write*"),
    row(Macos, Seatbelt, 0, FsIoctlDev, "file-ioctl"),
    row(
      Macos,
      Seatbelt,
      0,
      DenyInsideAllow,
      "the last matching rule wins",
    ),
    row(Macos, Seatbelt, 0, MetadataRead, "file-read-metadata"),
    row(
      Macos,
      Seatbelt,
      0,
      TcpPorts,
      "remote ip \"*:port\" / local ip",
    ),
    row(
      Macos,
      Seatbelt,
      0,
      SocketFamilies,
      "network-outbound / bind",
    ),
    row(
      Macos,
      Seatbelt,
      0,
      UnixSocketPaths,
      "remote unix-socket path",
    ),
    row(Macos, Seatbelt, 0, ScopeAbstractUnix, "no abstract sockets"),
    row(Macos, Seatbelt, 0, ScopeSignal, "signal (target others)"),
    row(Macos, Seatbelt, 0, ExecRefusal, "process-exec"),
    row(Macos, Seatbelt, 0, SysctlNarrowing, "sysctl-read by name"),
    // Windows (the small tier): containment and writes only
    row(Windows, JobObject, 0, ChildContainment, "kill on job close"),
    row(Windows, JobObject, 0, ExecRefusal, "active-process limit 1"),
    row(
      Windows,
      LowIntegrity,
      0,
      WriteConfinement,
      "writes only to Low-labelled directories",
    ),
  ]
};

/// What this machine offers, detected at run time (or given by a test).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
  pub os: TargetOs,
  /// The Landlock ABI, `None` without Landlock (Linux only).
  pub landlock_abi: Option<u32>,
  pub seccomp: bool,
  pub user_namespaces: bool,
  pub seatbelt: bool,
  pub job_object: bool,
  pub low_integrity: bool,
  /// Whether this runtime has an enforcer for `os` (Mode A: Linux on x86_64
  /// and aarch64, macOS).
  pub enforcer_built: bool,
}

impl Capabilities {
  /// No mechanism at all on `os`.
  pub fn none(os: TargetOs) -> Self {
    Self {
      os,
      landlock_abi: None,
      seccomp: false,
      user_namespaces: false,
      seatbelt: false,
      job_object: false,
      low_integrity: false,
      enforcer_built: false,
    }
  }

  fn level(&self, mechanism: Mechanism) -> Option<u32> {
    let present = match mechanism {
      Mechanism::Landlock => return self.landlock_abi,
      Mechanism::Seccomp => self.seccomp,
      Mechanism::UserNamespace => self.user_namespaces,
      Mechanism::Seatbelt => self.seatbelt,
      Mechanism::JobObject => self.job_object,
      Mechanism::LowIntegrity => self.low_integrity,
    };
    present.then_some(0)
  }

  /// Whether some mechanism here provides `feature`.
  pub fn supports(&self, feature: Feature) -> bool {
    CAPABILITY_TABLE.iter().any(|row| {
      row.os == self.os
        && row.feature == feature
        && self
          .level(row.mechanism)
          .is_some_and(|level| level >= row.min)
    })
  }

  /// The lowest Landlock ABI providing `feature`, when Landlock does.
  pub fn landlock_abi_for(feature: Feature) -> Option<u32> {
    CAPABILITY_TABLE
      .iter()
      .filter(|row| {
        row.mechanism == Mechanism::Landlock && row.feature == feature
      })
      .map(|row| row.min)
      .min()
  }
}

// ---------------------------------------------------------------------------
// Gaps
// ---------------------------------------------------------------------------

/// Which permission class a gap concerns.
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
pub enum GapClass {
  Read,
  Write,
  Net,
  Import,
  Unix,
  Run,
  Ffi,
  Sys,
  Env,
  Signal,
  Metadata,
  Enforcer,
}

impl GapClass {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Read => "read",
      Self::Write => "write",
      Self::Net => "net",
      Self::Import => "import",
      Self::Unix => "unix",
      Self::Run => "run",
      Self::Ffi => "ffi",
      Self::Sys => "sys",
      Self::Env => "env",
      Self::Signal => "signal",
      Self::Metadata => "metadata",
      Self::Enforcer => "enforcer",
    }
  }
}

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
pub enum GapResolution {
  FailOpen,
  FailClosed,
  Documented,
}

/// What would hold the rule exactly.
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
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ExactWith {
  /// `--kernel-sandbox=broker` (the supervising broker).
  Broker,
  /// A kernel with this Landlock ABI.
  LandlockAbi { abi: u32 },
  /// Unprivileged user namespaces (overmounting).
  UserNamespaces,
  /// Nothing the OS offers.
  Never,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyGap {
  pub class: GapClass,
  /// The Deno rule, as a flag (`--deny-write=/a/b`).
  pub rule: String,
  /// What the kernel does instead, and why.
  pub detail: String,
  pub resolution: GapResolution,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub exact_with: Option<ExactWith>,
}

impl PolicyGap {
  /// `enforce` / `broker` refuse to start on this gap.
  pub fn is_fatal_under_enforce(&self) -> bool {
    self.resolution == GapResolution::FailOpen
  }
}

// ---------------------------------------------------------------------------
// The implicit set: what the runtime needs that the user never grants
// ---------------------------------------------------------------------------

/// Who needs an implicit path.
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
pub enum ImplicitNeed {
  /// The deno / denort binary itself (denort's VFS lives in it).
  Runtime,
  /// Shared libraries, the loader and its cache, NSS modules.
  SharedLibraries,
  /// Time zones and locales.
  LocaleTime,
  /// The resolver's configuration (only with network).
  Dns,
  /// The user database (`os.userInfo()`, `getpwuid`).
  Users,
  /// System TLS roots (only with network).
  TlsRoots,
  /// CPU count, cgroup quotas, the process's own `/proc` entries.
  ProcessInfo,
  /// `/dev/null`, `/dev/urandom`, shared memory and friends.
  Devices,
  /// The controlling terminal, when the parent has one.
  Terminal,
  /// deno.json / package.json / the lockfile / import maps.
  ProjectConfig,
  /// The DENO_DIR caches (remote modules, npm, emit).
  DenoDir,
  /// The project's `node_modules`.
  NodeModules,
  /// System programs and their data, when `--allow-run` is granted (the
  /// children baseline).
  ChildrenBaseline,
  /// The inspector's listening port.
  Inspector,
  /// System information an `--allow-sys` name grants (`/proc` files).
  SysInfo,
  /// The working directory itself, listed but not read beneath (macOS:
  /// `getcwd` opens `.`, which spawning a child needs).
  WorkingDir,
  /// The policy-keyed cache overlay (`DENO_DIR/sandboxed/<policy hash>`):
  /// the SQLite caches and new emits of confined runs with this policy,
  /// never read by any other run (plan §5.1, item 451).
  CacheOverlay,
  /// `--coverage=<dir>` / `DENO_COVERAGE_DIR`: written for the user.
  Coverage,
  /// Certificate files the environment names (`DENO_CERT`, `SSL_CERT_FILE`,
  /// `SSL_CERT_DIR`, `NODE_EXTRA_CA_CERTS`), only with network.
  TlsFiles,
  /// This program's own `localStorage` and Cache API folders
  /// (`location_data/<key>`, `location_data/web_cache/<key>`).
  OriginData,
  /// `--cpu-prof` / `--heap-prof` folders: written for the user.
  Profiles,
  /// `deno test --junit-path=<file>`: the report written for the user
  /// (created before the sandbox applies, so a rule can name it).
  TestReport,
}

impl ImplicitNeed {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Runtime => "runtime",
      Self::SharedLibraries => "shared libraries",
      Self::LocaleTime => "locale and time zones",
      Self::Dns => "DNS",
      Self::Users => "user database",
      Self::TlsRoots => "TLS roots",
      Self::ProcessInfo => "process info",
      Self::Devices => "devices",
      Self::Terminal => "terminal",
      Self::ProjectConfig => "project config",
      Self::DenoDir => "DENO_DIR",
      Self::NodeModules => "node_modules",
      Self::ChildrenBaseline => "children baseline",
      Self::Inspector => "inspector",
      Self::SysInfo => "sys info",
      Self::WorkingDir => "working directory",
      Self::CacheOverlay => "cache overlay",
      Self::Coverage => "coverage",
      Self::TlsFiles => "TLS files",
      Self::OriginData => "origin data",
      Self::Profiles => "profiles",
      Self::TestReport => "test report",
    }
  }

  /// Entries that hold loadable code: never kept where the grant also makes
  /// them writable (the write + read + dlopen pivot, plan §1.4).
  fn holds_code(self) -> bool {
    matches!(
      self,
      Self::Runtime | Self::SharedLibraries | Self::NodeModules
    )
  }
}

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
pub enum ImplicitAccess {
  Read,
  /// Read, and load as executable code (dyld / dlopen).
  ReadMap,
  ReadWrite,
  /// The directory itself (its entries' names), nothing beneath it.
  ListDir,
}

#[derive(
  Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub struct ImplicitEntry {
  pub path: PathBuf,
  pub need: ImplicitNeed,
  pub access: ImplicitAccess,
}

/// When a static implicit row applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum When {
  Always,
  NetAllowed,
  RunGranted,
  TtyParent,
}

struct ImplicitRow {
  os: TargetOs,
  path: &'static str,
  need: ImplicitNeed,
  access: ImplicitAccess,
  when: When,
}

/// The per-OS implicit paths, each with who needs it (plan §5.1, first P5
/// pass, item 451); every entry is data here, printed by
/// `--kernel-sandbox-report`. The dynamic entries (the runtime binary, the
/// DENO_DIR caches read-only and this policy's cache overlay, this program's
/// origin data, the project's config files and lockfile, the coverage
/// folder, certificate files the environment names, the standard streams'
/// terminals, the `--allow-sys` names' `/proc` files) are added by
/// [`ImplicitSet::build`]. Not implicit, on purpose: the temp folder
/// (`Deno.makeTempFile` needs a write grant), the home folder, `.npmrc`.
static IMPLICIT_ROWS: &[ImplicitRow] = {
  use ImplicitAccess::*;
  use ImplicitNeed::*;
  use TargetOs::*;
  use When::*;
  macro_rules! r {
    ($os:expr, $path:expr, $need:expr, $access:expr, $when:expr) => {
      ImplicitRow {
        os: $os,
        path: $path,
        need: $need,
        access: $access,
        when: $when,
      }
    };
  }
  &[
    // Linux: the loader and libraries (ld.so.cache: dlopen by soname; glibc's
    // locale archive and gconv modules live under /usr/lib too)
    r!(Linux, "/lib", SharedLibraries, ReadMap, Always),
    r!(Linux, "/lib64", SharedLibraries, ReadMap, Always),
    r!(Linux, "/usr/lib", SharedLibraries, ReadMap, Always),
    r!(Linux, "/usr/lib64", SharedLibraries, ReadMap, Always),
    r!(Linux, "/usr/local/lib", SharedLibraries, ReadMap, Always),
    r!(Linux, "/etc/ld.so.cache", SharedLibraries, Read, Always),
    r!(Linux, "/etc/ld.so.conf", SharedLibraries, Read, Always),
    r!(Linux, "/etc/ld.so.conf.d", SharedLibraries, Read, Always),
    // Linux: time and locale (/etc/localtime is a symlink: the rule follows
    // it to its zoneinfo file; ICU's data is embedded)
    r!(Linux, "/usr/share/zoneinfo", LocaleTime, Read, Always),
    r!(Linux, "/etc/localtime", LocaleTime, Read, Always),
    r!(Linux, "/usr/lib/locale", LocaleTime, Read, Always),
    r!(Linux, "/usr/share/locale", LocaleTime, Read, Always),
    // Linux: users (os.userInfo, getpwuid: NSS reads these; nscd and
    // systemd-userdb are Unix sockets, refused, and NSS falls back to files)
    r!(Linux, "/etc/passwd", Users, Read, Always),
    r!(Linux, "/etc/group", Users, Read, Always),
    r!(Linux, "/etc/nsswitch.conf", Users, Read, Always),
    // Linux: DNS, only with network (resolv.conf's target under
    // /run/systemd/resolve or /run/NetworkManager)
    r!(Linux, "/etc/resolv.conf", Dns, Read, NetAllowed),
    r!(Linux, "/etc/hosts", Dns, Read, NetAllowed),
    r!(Linux, "/etc/gai.conf", Dns, Read, NetAllowed),
    r!(Linux, "/etc/host.conf", Dns, Read, NetAllowed),
    r!(Linux, "/etc/services", Dns, Read, NetAllowed),
    r!(Linux, "/etc/protocols", Dns, Read, NetAllowed),
    r!(Linux, "/run/systemd/resolve", Dns, Read, NetAllowed),
    r!(Linux, "/run/NetworkManager", Dns, Read, NetAllowed),
    // Linux: TLS roots, only with network (the bundled roots need nothing;
    // these serve DENO_TLS_CA_STORE=system)
    r!(Linux, "/etc/ssl", TlsRoots, Read, NetAllowed),
    r!(Linux, "/etc/pki", TlsRoots, Read, NetAllowed),
    r!(Linux, "/etc/ca-certificates", TlsRoots, Read, NetAllowed),
    r!(
      Linux,
      "/usr/share/ca-certificates",
      TlsRoots,
      Read,
      NetAllowed
    ),
    // Linux: process info (V8/tokio CPU count, cgroup quotas)
    r!(Linux, "/proc/self", ProcessInfo, Read, Always),
    r!(Linux, "/proc/thread-self", ProcessInfo, Read, Always),
    r!(Linux, "/proc/cpuinfo", ProcessInfo, Read, Always),
    r!(Linux, "/proc/stat", ProcessInfo, Read, Always),
    r!(Linux, "/sys/devices/system/cpu", ProcessInfo, Read, Always),
    r!(Linux, "/sys/fs/cgroup", ProcessInfo, Read, Always),
    // Linux: devices (/dev/shm: V8 and POSIX shared memory; a documented gap,
    // named segments aren't per process)
    r!(Linux, "/dev/null", Devices, ReadWrite, Always),
    r!(Linux, "/dev/zero", Devices, ReadWrite, Always),
    r!(Linux, "/dev/full", Devices, ReadWrite, Always),
    r!(Linux, "/dev/random", Devices, Read, Always),
    r!(Linux, "/dev/urandom", Devices, Read, Always),
    r!(Linux, "/dev/shm", Devices, ReadWrite, Always),
    r!(Linux, "/dev/fd", Devices, Read, Always),
    // Linux: the terminal, only when the parent has one
    // (the standard streams' own terminals come from the context, never
    // every pseudo-terminal)
    r!(Linux, "/dev/tty", Terminal, ReadWrite, TtyParent),
    // Linux: the children baseline (review F8): programs and their data
    r!(Linux, "/usr", ChildrenBaseline, ReadMap, RunGranted),
    r!(Linux, "/bin", ChildrenBaseline, ReadMap, RunGranted),
    r!(Linux, "/sbin", ChildrenBaseline, ReadMap, RunGranted),
    r!(Linux, "/etc", ChildrenBaseline, Read, RunGranted),
    r!(Linux, "/opt", ChildrenBaseline, ReadMap, RunGranted),
    // Linuxbrew's system-wide prefix (as /opt/homebrew is on macOS, under
    // /opt): programs on PATH there (python3, git) run as children
    r!(
      Linux,
      "/home/linuxbrew/.linuxbrew",
      ChildrenBaseline,
      ReadMap,
      RunGranted
    ),
    // macOS: dyld, libraries and the shared cache
    r!(Macos, "/usr/lib", SharedLibraries, ReadMap, Always),
    r!(Macos, "/System", SharedLibraries, ReadMap, Always),
    r!(Macos, "/Library/Apple", SharedLibraries, ReadMap, Always),
    r!(
      Macos,
      "/System/Volumes/Preboot/Cryptexes",
      SharedLibraries,
      ReadMap,
      Always
    ),
    r!(
      Macos,
      "/private/var/db/dyld",
      SharedLibraries,
      ReadMap,
      Always
    ),
    // macOS: time and locale
    r!(Macos, "/usr/share/zoneinfo", LocaleTime, Read, Always),
    r!(Macos, "/private/var/db/timezone", LocaleTime, Read, Always),
    r!(Macos, "/private/etc/localtime", LocaleTime, Read, Always),
    // macOS: DNS (Seatbelt checks the resolved path), only with network
    r!(Macos, "/private/etc/hosts", Dns, Read, NetAllowed),
    r!(Macos, "/private/etc/services", Dns, Read, NetAllowed),
    r!(Macos, "/private/var/run/resolv.conf", Dns, Read, NetAllowed),
    // macOS: devices
    r!(Macos, "/dev/null", Devices, ReadWrite, Always),
    r!(Macos, "/dev/zero", Devices, Read, Always),
    r!(Macos, "/dev/random", Devices, Read, Always),
    r!(Macos, "/dev/urandom", Devices, Read, Always),
    r!(Macos, "/dev/dtracehelper", Devices, ReadWrite, Always),
    r!(Macos, "/dev/fd", Devices, Read, Always),
    // libc's automounter probe at startup (E3: harmless, a denial only logs)
    r!(Macos, "/dev/autofs_nowait", Devices, Read, Always),
    r!(Macos, "/dev/tty", Terminal, ReadWrite, TtyParent),
    // macOS: the children baseline
    // dyld in every new process lists `/` and aborts when refused (measured
    // on macOS 26): the folder itself, nothing beneath it
    r!(Macos, "/", ChildrenBaseline, ListDir, RunGranted),
    r!(Macos, "/usr", ChildrenBaseline, ReadMap, RunGranted),
    r!(Macos, "/bin", ChildrenBaseline, ReadMap, RunGranted),
    r!(Macos, "/sbin", ChildrenBaseline, ReadMap, RunGranted),
    r!(Macos, "/private/etc", ChildrenBaseline, Read, RunGranted),
    r!(Macos, "/opt", ChildrenBaseline, ReadMap, RunGranted),
    r!(
      Macos,
      "/Library/Developer/CommandLineTools",
      ChildrenBaseline,
      ReadMap,
      RunGranted
    ),
    r!(
      Macos,
      "/Applications/Xcode.app/Contents/Developer",
      ChildrenBaseline,
      ReadMap,
      RunGranted
    ),
  ]
};

/// The `/proc` files each `--allow-sys` name reads on Linux. The process's
/// own `/proc/self` (`processInfo`) is in the implicit set already (the
/// runtime reads it), as are `/proc/cpuinfo`, `/proc/stat`, the cgroup
/// limits and `/sys/devices/system/cpu` (V8 and tokio size themselves from
/// them). Other processes' entries (`processes`) can't be scoped: a gap.
const SYS_FILES: &[(&str, &str)] = &[
  ("systemMemoryInfo", "/proc/meminfo"),
  ("loadavg", "/proc/loadavg"),
  ("osUptime", "/proc/uptime"),
  ("osRelease", "/proc/version"),
];

/// The `--allow-sys` names an unscoped `--allow-sys` doesn't include: only
/// naming them grants them (other processes' and this process's own
/// `/proc` entries).
const SYS_NAMED_ONLY: &[&str] = &["processInfo", "processes"];

/// Whether `--allow-sys` grants `name` (unscoped grants every name but the
/// named-only ones).
fn sys_granted(options: &PermissionsOptions, name: &str) -> bool {
  match &options.allow_sys {
    None => false,
    Some(list) if list.is_empty() => !SYS_NAMED_ONLY.contains(&name),
    Some(list) => list.iter().any(|n| n == name),
  }
}

/// The DENO_DIR folders a confined run reads (never writes: caches must not
/// become a privilege ladder, plan §5.1).
const DENO_DIR_READ: &[&str] = &[
  "remote",
  "npm",
  "gen",
  "deps",
  "registries",
  "location_data",
];

/// The variables a scrubbed environment keeps besides the `--allow-env`
/// names (plan §1.5).
const RUNTIME_ENV: &[&str] = &[
  "PATH",
  "HOME",
  "TMPDIR",
  "DENO_DIR",
  "LANG",
  "LC_*",
  "TZ",
  "HTTP_PROXY",
  "HTTPS_PROXY",
  "ALL_PROXY",
  "NO_PROXY",
  "http_proxy",
  "https_proxy",
  "all_proxy",
  "no_proxy",
  "NO_COLOR",
  "TERM",
];

/// What the runtime needs on this machine for this grant.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImplicitSet {
  pub entries: Vec<ImplicitEntry>,
}

impl ImplicitSet {
  /// The implicit set for `options` in `ctx`.
  pub fn build(options: &PermissionsOptions, ctx: &Context) -> Self {
    let net_allowed = net_allowed(options);
    let run_granted = options.allow_run.is_some();
    let mut entries: Vec<ImplicitEntry> = IMPLICIT_ROWS
      .iter()
      .filter(|row| row.os == ctx.caps.os)
      .filter(|row| match row.when {
        When::Always => true,
        When::NetAllowed => net_allowed,
        When::RunGranted => run_granted,
        When::TtyParent => ctx.tty_parent,
      })
      .map(|row| ImplicitEntry {
        path: PathBuf::from(row.path),
        need: row.need,
        access: row.access,
      })
      .collect();
    let mut add = |path: &Path, need, access| {
      entries.push(ImplicitEntry {
        path: path.to_path_buf(),
        need,
        access,
      })
    };
    if let Some(exe) = &ctx.exe {
      add(exe, ImplicitNeed::Runtime, ImplicitAccess::ReadMap);
      // a runtime installed in a store brings its libraries along
      for store in ["/nix/store", "/gnu/store"] {
        if exe.starts_with(store) && ctx.caps.os == TargetOs::Linux {
          add(
            Path::new(store),
            ImplicitNeed::SharedLibraries,
            ImplicitAccess::ReadMap,
          );
        }
      }
    }
    if !ctx.compiled
      && let Some(deno_dir) = &ctx.deno_dir
    {
      // (Node-API addons in the npm cache load from the read scope: loading
      // one needs --allow-ffi, whose load-code scope is the read scope)
      for sub in DENO_DIR_READ {
        add(
          &deno_dir.join(sub),
          ImplicitNeed::DenoDir,
          ImplicitAccess::Read,
        );
      }
      // the overlay lies inside DENO_DIR, so it is only ever this run's
      if let Some(overlay) = &ctx.cache_overlay
        && overlay.starts_with(deno_dir)
      {
        add(
          overlay,
          ImplicitNeed::CacheOverlay,
          ImplicitAccess::ReadWrite,
        );
      }
      // this program's localStorage / Cache API folders, never another's
      for dir in &ctx.origin_data {
        if dir.starts_with(deno_dir.join("location_data")) {
          add(dir, ImplicitNeed::OriginData, ImplicitAccess::ReadWrite);
        }
      }
    }
    // a compiled program's own cache folder (its V8 code cache, the native
    // addons it extracts), keyed by its policy like the overlay (item 473)
    if ctx.compiled
      && let Some(overlay) = &ctx.cache_overlay
    {
      add(
        overlay,
        ImplicitNeed::CacheOverlay,
        ImplicitAccess::ReadWrite,
      );
    }
    if let Some(dir) = &ctx.coverage_dir {
      add(dir, ImplicitNeed::Coverage, ImplicitAccess::ReadWrite);
    }
    for dir in &ctx.profile_dirs {
      add(dir, ImplicitNeed::Profiles, ImplicitAccess::ReadWrite);
    }
    for file in &ctx.report_files {
      add(file, ImplicitNeed::TestReport, ImplicitAccess::ReadWrite);
    }
    if net_allowed {
      for file in &ctx.tls_files {
        add(file, ImplicitNeed::TlsFiles, ImplicitAccess::Read);
      }
    }
    for file in &ctx.config_files {
      add(file, ImplicitNeed::ProjectConfig, ImplicitAccess::Read);
    }
    // the standard streams' own terminals (reopened through /dev/stdin and
    // friends), not every terminal of the user
    for tty in &ctx.ttys {
      add(tty, ImplicitNeed::Terminal, ImplicitAccess::ReadWrite);
    }
    // the files each `--allow-sys` name reads (Linux), and nothing for a
    // name not granted
    if ctx.caps.os == TargetOs::Linux {
      for (name, file) in SYS_FILES {
        if sys_granted(options, name) {
          add(Path::new(file), ImplicitNeed::SysInfo, ImplicitAccess::Read);
        }
      }
    }
    if ctx.caps.os == TargetOs::Macos {
      add(&ctx.cwd, ImplicitNeed::WorkingDir, ImplicitAccess::ListDir);
    }
    if ctx.caps.os == TargetOs::Macos
      && let Some(home) = &ctx.home
    {
      // CoreFoundation reads it at startup (harmless; a denial only logs)
      add(
        &home.join(".CFUserTextEncoding"),
        ImplicitNeed::LocaleTime,
        ImplicitAccess::Read,
      );
    }
    if let Some(node_modules) = &ctx.node_modules {
      add(
        node_modules,
        ImplicitNeed::NodeModules,
        ImplicitAccess::ReadMap,
      );
    }
    entries.sort();
    entries.dedup();
    Self { entries }
  }
}

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

/// The machine and the run a policy is compiled for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
  pub caps: Capabilities,
  /// The effective requested mode (`auto` once enforcement ships, `warn`,
  /// `enforce`, `broker`). Decides the shape and env scrubbing.
  pub mode: KernelSandboxMode,
  /// Relative grant paths resolve against this.
  pub cwd: PathBuf,
  pub exe: Option<PathBuf>,
  pub home: Option<PathBuf>,
  pub deno_dir: Option<PathBuf>,
  pub config_files: Vec<PathBuf>,
  pub node_modules: Option<PathBuf>,
  pub inspector_port: Option<u16>,
  /// stdin is a terminal (the `/dev/tty` rule, plan §3).
  pub tty_parent: bool,
  /// The terminals the standard streams are (`ttyname` of fds 0 to 2): the
  /// only pseudo-terminals the policy names.
  pub ttys: Vec<PathBuf>,
  /// A compiled program: no remote imports, no DENO_DIR caches.
  pub compiled: bool,
  /// `--kernel-sandbox-exec=strict` (plan D3's opt-in).
  pub exec_strict: bool,
  /// The policy-keyed cache overlay (`DENO_DIR/sandboxed/<hash>`; a compiled
  /// program's own cache folder), set once the policy without it is hashed.
  pub cache_overlay: Option<PathBuf>,
  /// The coverage folder the run writes (`--coverage`, `DENO_COVERAGE_DIR`).
  pub coverage_dir: Option<PathBuf>,
  /// Certificate files and folders the environment names.
  pub tls_files: Vec<PathBuf>,
  /// This program's own origin data folders (`localStorage`, the Cache API).
  pub origin_data: Vec<PathBuf>,
  /// The `--cpu-prof` / `--heap-prof` folders the run writes.
  pub profile_dirs: Vec<PathBuf>,
  /// Report files the run writes for the user (`deno test --junit-path`).
  pub report_files: Vec<PathBuf>,
  /// `auto` enforces the deny rules alone (D1 / D2,
  /// `ModeResolution::deny_ceiling`): the grant passed in is
  /// [`deny_ceiling_options`], nothing is expanded or scrubbed, no seccomp
  /// list applies, and a deny the OS can't hold that way is a documented
  /// Deno-only rule, never a refusal (decision O4).
  pub deny_ceiling: bool,
}

impl Context {
  pub fn new(
    caps: Capabilities,
    mode: KernelSandboxMode,
    cwd: PathBuf,
  ) -> Self {
    Self {
      caps,
      mode,
      cwd,
      exe: None,
      home: None,
      deno_dir: None,
      config_files: Vec::new(),
      node_modules: None,
      inspector_port: None,
      tty_parent: false,
      ttys: Vec::new(),
      compiled: false,
      exec_strict: false,
      cache_overlay: None,
      coverage_dir: None,
      tls_files: Vec::new(),
      origin_data: Vec::new(),
      profile_dirs: Vec::new(),
      report_files: Vec::new(),
      deny_ceiling: false,
    }
  }
}

/// The grant a deny ceiling compiles (D1 / D2): every allow unscoped, every
/// deny and ignore rule as the run wrote it. A prompt can grant anything
/// but a denied path, so this is what the kernel may hold the run to
/// without refusing what a prompt grants.
pub fn deny_ceiling_options(options: &PermissionsOptions) -> PermissionsOptions {
  let all = Some(Vec::new());
  PermissionsOptions {
    allow_read: all.clone(),
    allow_write: all.clone(),
    allow_net: all.clone(),
    // `--allow-net` unscoped covers both directions
    allow_net_connect: None,
    allow_net_listen: None,
    allow_env: all.clone(),
    allow_run: all.clone(),
    allow_ffi: all.clone(),
    allow_sys: all,
    ..options.clone()
  }
}

// ---------------------------------------------------------------------------
// The policy
// ---------------------------------------------------------------------------

/// Mode A (the process confines itself) or Mode B (a supervising broker
/// performs what only it can hold exactly).
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
pub enum Shape {
  ModeA,
  ModeB,
}

#[derive(
  Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub struct PathRule {
  pub path: PathBuf,
  /// `None`: the user's grant; else who needs it.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub implicit: Option<ImplicitNeed>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PathScope {
  /// Unrestricted.
  All,
  /// These paths (and everything beneath a directory).
  Paths { paths: Vec<PathRule> },
}

impl PathScope {
  fn none() -> Self {
    Self::Paths { paths: Vec::new() }
  }

  /// Whether `path` lies in the scope.
  pub fn covers(&self, path: &Path) -> bool {
    match self {
      Self::All => true,
      Self::Paths { paths } => paths.iter().any(|r| path.starts_with(&r.path)),
    }
  }

  pub fn is_all(&self) -> bool {
    matches!(self, Self::All)
  }

  /// Whether a path lies in what the user granted (implicit entries not
  /// counted).
  pub fn grant_covers(&self, path: &Path) -> bool {
    match self {
      Self::All => true,
      Self::Paths { paths } => paths
        .iter()
        .any(|r| r.implicit.is_none() && path.starts_with(&r.path)),
    }
  }
}

pub use crate::kernel_inherit::resolve_prefix;

/// Whether a path lies in a scope, deciding on resolved paths as well as
/// the spelling (an allow and a deny may name one object through different
/// paths: a symlink, `/tmp` and `/private/tmp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
  Inside,
  Outside,
  /// The path's resolution stopped early: it can't be told.
  Undecided,
}

/// How a deny rule is held.
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
pub enum DenyEnforcement {
  /// No allow covers it: the kernel refuses it anyway.
  Implied,
  /// Exactly (Seatbelt: the last matching rule wins).
  Exact,
  /// Landlock: the allow is expanded into rules on every entry beside the
  /// path toward the deny (stable: no grant writes those directories).
  Expand,
  /// An empty mount over the denied path in a user + mount namespace.
  Overmount,
  /// The broker performs the operations that would reach it.
  Broker,
  /// Not held by the kernel; Deno's check only (a gap says why).
  DenoOnly,
}

#[derive(
  Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub struct DenyRule {
  pub path: PathBuf,
  /// From `--ignore-read` (the kernel can't fake `NotFound`).
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub ignore: bool,
  pub enforcement: DenyEnforcement,
}

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
pub enum ExecScope {
  /// No program may start (no `--allow-run`).
  None,
  /// Any readable program may start; the protection is that children
  /// inherit the fs / net domain (plan D3).
  Unrestricted,
  /// `--kernel-sandbox-exec=strict`: the whole process tree may start only
  /// the `--allow-run` programs and the interpreters they need
  /// ([`FsRules::exec_programs`]).
  Listed,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsRules {
  pub read: PathScope,
  pub write: PathScope,
  pub deny_read: Vec<DenyRule>,
  pub deny_write: Vec<DenyRule>,
  pub exec: ExecScope,
  /// The files [`ExecScope::Listed`] lets execute (canonical): the
  /// programs, their `#!` interpreters and ELF loaders.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub exec_programs: Vec<PathBuf>,
  /// Where executable code may be loaded from (Seatbelt
  /// `file-map-executable`; on Linux the read scope bounds it).
  pub map_exec: PathScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PortScope {
  None,
  Any,
  Ports { ports: Vec<u16> },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HostScope {
  None,
  Any,
  Hosts { hosts: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum NetPolicy {
  /// No IP sockets.
  Denied,
  /// IP sockets unrestricted.
  Open,
  /// TCP by port only (Landlock ABI 4+, Seatbelt); no UDP.
  Ports { connect: PortScope, bind: PortScope },
  /// The broker connects, binds and resolves, checking hosts exactly; the
  /// confined process gets no IP sockets of its own.
  Broker {
    connect: HostScope,
    bind: HostScope,
    deny: Vec<String>,
  },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum UnixPolicy {
  /// `socket(AF_UNIX)` refused (`socketpair` stays allowed).
  Denied,
  /// These socket paths only (Seatbelt per path; on Linux the broker).
  Paths { paths: Vec<PathBuf> },
  /// Every socket the user can reach by file mode.
  Open,
}

#[derive(
  Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub struct SysNarrowing {
  /// Files left out of the read scope because their `sys` name isn't granted.
  pub hidden_files: Vec<PathBuf>,
  /// sysctl names (or prefixes ending in `*`) refused (macOS).
  pub denied_sysctls: Vec<String>,
  /// Whether `AF_NETLINK` (route family only) stays allowed (Linux).
  pub netlink_route: bool,
}

#[derive(
  Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub struct Scopes {
  /// Signals can't reach processes outside the sandbox.
  pub signal: bool,
  /// Abstract Unix sockets outside the sandbox can't be reached.
  pub abstract_unix: bool,
}

#[derive(
  Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub struct SyscallPolicy {
  /// The attack-surface list (bpf, perf_event_open, userfaultfd, keyctl,
  /// ptrace, mount, kexec, modules, …), never under `-A`.
  pub attack_surface: bool,
  /// `execve` / `execveat` refused (no `--allow-run`).
  pub refuse_exec: bool,
  /// `listen` refused when nothing may bind (review F2).
  pub refuse_listen: bool,
  /// `clone3` answers `ENOSYS` so `clone`'s flags can be filtered.
  pub clone3_enosys: bool,
  /// `io_uring_setup` refused.
  pub refuse_io_uring: bool,
  /// The path forms of chmod / chown / utime / xattr refused (review F5;
  /// `enforce` only).
  pub refuse_path_metadata: bool,
  /// Truncation Landlock can't hold before ABI 3 refused by seccomp:
  /// `truncate(2)` by path, read-only truncating opens, `openat2` (`enforce`
  /// only).
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub refuse_path_truncate: bool,
  /// `AF_VSOCK` allowed (a `vsock:` grant).
  pub vsock: bool,
  /// Signals reach this process only (seccomp, where Landlock can't scope
  /// them and no child may start).
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub signal_self: bool,
}

/// The compiled kernel policy: what each mechanism would apply.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KernelPolicy {
  pub format: u32,
  pub os: TargetOs,
  pub shape: Shape,
  /// `-A` with denies: only the denies are applied.
  pub deny_only: bool,
  /// A deny ceiling (D1 / D2, [`Context::deny_ceiling`]): nothing but the
  /// denies is applied, with no seccomp list and no no-new-privileges.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub deny_ceiling: bool,
  pub fs: FsRules,
  pub net: NetPolicy,
  /// TCP listeners bound before the process confines itself (Linux, Mode
  /// A): the addresses the grant names for listening. The confined
  /// process binds none of its own.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub prebound: Vec<SocketAddr>,
  pub unix: UnixPolicy,
  pub sys: SysNarrowing,
  /// `Some`: the environment a re-exec'd image keeps (Linux and macOS,
  /// `enforce`).
  pub env_scrub: Option<Vec<String>>,
  pub scopes: Scopes,
  pub syscalls: SyscallPolicy,
}

/// A compiled policy with its gaps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompiledPolicy {
  pub policy: KernelPolicy,
  pub gaps: Vec<PolicyGap>,
  pub implicit: ImplicitSet,
}

impl CompiledPolicy {
  pub fn hash(&self) -> String {
    self.policy.hash()
  }

  /// The gaps `enforce` / `broker` refuse to start on.
  pub fn fatal_gaps(&self) -> impl Iterator<Item = &PolicyGap> {
    self.gaps.iter().filter(|g| g.is_fatal_under_enforce())
  }
}

impl KernelPolicy {
  /// A deny ceiling the kernel holds at least one rule of: a read or write
  /// deny held exactly (Seatbelt) or by an overmount. A ceiling that holds
  /// none applies nothing (no re-exec, no profile): its denies are Deno's
  /// checks only, reported as such.
  pub fn deny_ceiling_held(&self) -> bool {
    self
      .fs
      .deny_read
      .iter()
      .chain(&self.fs.deny_write)
      .any(|d| {
        matches!(
          d.enforcement,
          DenyEnforcement::Exact | DenyEnforcement::Overmount
        )
      })
  }

  /// A stable hash of the policy (format, rules; not the gaps): the key for
  /// per-policy caches and notice deduplication. 16 hex digits of SHA-256
  /// over the canonical JSON (every list sorted).
  pub fn hash(&self) -> String {
    let json = serde_json::to_vec(self).expect("policy serializes");
    let mut hex = crate::util::checksum::r#gen(&[json]);
    hex.truncate(16);
    hex
  }

  /// Compiles `options` for `ctx`, choosing Mode B where the policy needs it:
  /// always for `broker`; for host-scoped network on Linux (review F2); and
  /// under `enforce` whenever the broker would close a fail-open gap.
  pub fn compile(
    options: &PermissionsOptions,
    implicit: &ImplicitSet,
    ctx: &Context,
  ) -> CompiledPolicy {
    let first_shape = if ctx.mode == KernelSandboxMode::Broker {
      Shape::ModeB
    } else {
      Shape::ModeA
    };
    let compiled = Compiler::new(options, implicit, ctx, first_shape).run();
    // a deny ceiling holds what Mode A holds and reports the rest: it never
    // starts a supervisor (nothing it leaves to Deno's checks is a refusal)
    if first_shape == Shape::ModeB || compiled.policy.deny_ceiling {
      return compiled;
    }
    let needs_broker = matches!(compiled.policy.net, NetPolicy::Broker { .. })
      || (ctx.mode == KernelSandboxMode::Enforce
        && compiled.gaps.iter().any(|g| {
          g.is_fatal_under_enforce() && g.exact_with == Some(ExactWith::Broker)
        }));
    if needs_broker && ctx.compiled {
      // a compiled program applies its sandbox in place, with no supervising
      // broker: what only the broker would hold stays a rule the OS can't
      // hold (warned about at `deno compile`, refused at start), never a
      // Mode B plan nothing runs
      let mut compiled = compiled;
      if matches!(compiled.policy.net, NetPolicy::Broker { .. }) {
        compiled.gaps.push(PolicyGap {
          class: GapClass::Net,
          rule: net_rule(options),
          detail: "Deno only: host-scoped network needs the supervising broker, which a compiled program doesn't run".to_string(),
          resolution: GapResolution::FailOpen,
          exact_with: Some(ExactWith::Broker),
        });
      }
      return compiled;
    }
    if needs_broker {
      // the rules the broker's unbuilt side would hold (a write deny is
      // the built file side's: it is held, and not among them)
      let mut rules: Vec<String> = compiled
        .gaps
        .iter()
        .filter(|g| {
          g.is_fatal_under_enforce()
            && g.exact_with == Some(ExactWith::Broker)
            && !(BROKER_FS_BUILT && g.class == GapClass::Write)
        })
        .map(|g| g.rule.clone())
        .collect();
      let host_net = matches!(compiled.policy.net, NetPolicy::Broker { .. });
      if host_net {
        rules.push(net_rule(options));
      }
      rules.sort();
      rules.dedup();
      let mut mode_b =
        Compiler::new(options, implicit, ctx, Shape::ModeB).run();
      if !rules.is_empty() {
        // say which rules made the broker necessary, and what they are
        let needs = broker_needs(host_net, rules.len());
        for gap in &mut mode_b.gaps {
          if gap.class == GapClass::Enforcer && gap.rule == BROKER_RULE {
            gap.detail = format!(
              "{BROKER_NOT_BUILT}: {needs} would be held by Deno's checks only (needed for: {})",
              rules.join("; ")
            );
          }
        }
      }
      mode_b
    } else {
      compiled
    }
  }
}

// ---------------------------------------------------------------------------
// The compiler
// ---------------------------------------------------------------------------

fn net_allowed(options: &PermissionsOptions) -> bool {
  options.allow_net.is_some()
    || options.allow_net_connect.is_some()
    || options.allow_net_listen.is_some()
}

/// The rule a "broker not built" gap names.
const BROKER_RULE: &str = "--kernel-sandbox (broker)";

/// The broker gap's opening words.
const BROKER_NOT_BUILT: &str =
  "the supervising broker's network side is not built into this runtime yet";

/// What the broker gap says would be held by Deno's checks only: the
/// host-scoped network when that is the one rule that needs the broker,
/// else the rules it lists.
fn broker_needs(host_net: bool, rules: usize) -> &'static str {
  if host_net && rules == 1 {
    "host-scoped network"
  } else {
    "these rules"
  }
}

/// The network grant as written, for a gap's rule list.
fn net_rule(options: &PermissionsOptions) -> String {
  let mut parts = Vec::new();
  for (flag, list) in [
    ("--allow-net", &options.allow_net),
    ("--allow-net-connect", &options.allow_net_connect),
    ("--allow-net-listen", &options.allow_net_listen),
  ] {
    match list {
      Some(list) if list.is_empty() => parts.push(flag.to_string()),
      Some(list) => parts.push(format!("{flag}={}", list.join(","))),
      None => {}
    }
  }
  if parts.is_empty() {
    "no --allow-net".to_string()
  } else {
    parts.join(" ")
  }
}

pub use crate::kernel_sandbox::options_allow_all;
pub use crate::kernel_sandbox::options_have_deny;

/// A net list entry, classified for the kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
enum NetEntry {
  /// An IP literal or `localhost`, with a port.
  AddressPort(u16),
  /// A Unix socket path (`unix:/path`).
  Unix(PathBuf),
  Vsock,
  /// Anything else (a name, a wildcard, a subnet, no port): only the broker
  /// can hold it.
  Hosted,
}

/// The addresses to bind before confinement for an `--allow-net-listen`
/// list whose every IP entry is an address with a port: an IP literal as itself, `localhost`
/// as both loopback addresses. `None` when the side is unscoped, empty, or
/// has an entry that can't be bound ahead (a name, a subnet, no port, port
/// 0: an ephemeral port is a new one for every listener).
fn prebound_listeners(side: &NetSide) -> Option<Vec<SocketAddr>> {
  use std::net::Ipv4Addr;
  use std::net::Ipv6Addr;
  if side.is_any() {
    return None;
  }
  let mut addrs = BTreeSet::new();
  for entry in side.entries() {
    match NetDescriptor::parse_for_list(entry) {
      Ok(NetDescriptor(Host::UnixSocket(_) | Host::Vsock(_), _)) => {}
      Ok(NetDescriptor(Host::Ip(ip), Some(port))) => {
        let port = u16::try_from(port).ok().filter(|port| *port != 0)?;
        addrs.insert(SocketAddr::new(ip, port));
      }
      Ok(NetDescriptor(Host::Fqdn(_), Some(port)))
        if matches!(classify_net(entry), NetEntry::AddressPort(_)) =>
      {
        let port = u16::try_from(port).ok().filter(|port| *port != 0)?;
        addrs.insert(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port));
        addrs.insert(SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port));
      }
      _ => return None,
    }
  }
  (!addrs.is_empty()).then(|| addrs.into_iter().collect())
}

fn classify_net(entry: &str) -> NetEntry {
  match NetDescriptor::parse_for_list(entry) {
    Ok(NetDescriptor(Host::UnixSocket(path), _)) => NetEntry::Unix(path),
    Ok(NetDescriptor(Host::Vsock(_), _)) => NetEntry::Vsock,
    Ok(NetDescriptor(Host::Ip(_), Some(port))) => {
      u16::try_from(port).map_or(NetEntry::Hosted, NetEntry::AddressPort)
    }
    Ok(NetDescriptor(Host::Fqdn(name), Some(port)))
      if name
        .to_string()
        .trim_end_matches('.')
        .eq_ignore_ascii_case("localhost") =>
    {
      u16::try_from(port).map_or(NetEntry::Hosted, NetEntry::AddressPort)
    }
    _ => NetEntry::Hosted,
  }
}

/// Whether `entry` names an IP literal (for the metadata-endpoint warning).
fn is_ip_literal(entry: &str) -> bool {
  matches!(
    NetDescriptor::parse_for_list(entry),
    Ok(NetDescriptor(Host::Ip(_), _))
  )
}

struct NetSide {
  /// `None`: no grant; `Some(empty)`: unscoped.
  list: Option<Vec<String>>,
}

impl NetSide {
  fn merged(a: &Option<Vec<String>>, b: &Option<Vec<String>>) -> Self {
    let list = match (a, b) {
      (None, None) => None,
      (Some(a), _) if a.is_empty() => Some(Vec::new()),
      (_, Some(b)) if b.is_empty() => Some(Vec::new()),
      (a, b) => Some(
        a.iter()
          .chain(b.iter())
          .flatten()
          .cloned()
          .collect::<BTreeSet<_>>()
          .into_iter()
          .collect(),
      ),
    };
    Self { list }
  }

  fn is_any(&self) -> bool {
    self.list.as_ref().is_some_and(|l| l.is_empty())
  }

  fn entries(&self) -> &[String] {
    self.list.as_deref().unwrap_or_default()
  }
}

struct Compiler<'a> {
  options: &'a PermissionsOptions,
  implicit: &'a ImplicitSet,
  ctx: &'a Context,
  shape: Shape,
  caps: Capabilities,
  gaps: Vec<PolicyGap>,
  deny_only: bool,
  /// A deny ceiling ([`Context::deny_ceiling`] with denies): only the
  /// denies are held, and what can't be held is documented, never refused.
  ceiling: bool,
  /// Every allow unscoped and no deny (`-A`): only the forced harness
  /// compiles it (plain `-A` is off). Nothing is narrower than the grant, so
  /// reaching every socket is the grant, not a gap.
  allow_all: bool,
  /// Native code can run (FFI, addons or children).
  native: bool,
  /// The listeners bound before confinement ([`KernelPolicy::prebound`]).
  prebound: Vec<SocketAddr>,
}

impl<'a> Compiler<'a> {
  fn new(
    options: &'a PermissionsOptions,
    implicit: &'a ImplicitSet,
    ctx: &'a Context,
    shape: Shape,
  ) -> Self {
    Self {
      options,
      implicit,
      ctx,
      shape,
      caps: ctx.caps,
      gaps: Vec::new(),
      deny_only: options_allow_all(options) && options_have_deny(options),
      ceiling: ctx.deny_ceiling
        && options_allow_all(options)
        && options_have_deny(options),
      allow_all: options_allow_all(options) && !options_have_deny(options),
      native: options.allow_ffi.is_some() || options.allow_run.is_some(),
      prebound: Vec::new(),
    }
  }

  fn opts(&self) -> &'a PermissionsOptions {
    self.options
  }

  fn implicit_set(&self) -> &'a ImplicitSet {
    self.implicit
  }

  fn ctx(&self) -> &'a Context {
    self.ctx
  }

  fn os(&self) -> TargetOs {
    self.caps.os
  }

  fn gap(
    &mut self,
    class: GapClass,
    rule: impl Into<String>,
    detail: impl Into<String>,
    resolution: GapResolution,
    exact_with: Option<ExactWith>,
  ) {
    // a deny ceiling never refuses (decision O4): what the OS can't hold is
    // a documented Deno-only rule, as the prompt run it is would have it
    let resolution = if self.ceiling && resolution == GapResolution::FailOpen
    {
      GapResolution::Documented
    } else {
      resolution
    };
    self.gaps.push(PolicyGap {
      class,
      rule: rule.into(),
      detail: detail.into(),
      resolution,
      exact_with,
    });
  }

  fn abs(&self, path: &str) -> PathBuf {
    normalize_path(Cow::Owned(self.ctx().cwd.join(path))).into_owned()
  }

  fn abs_list(&self, list: &Option<Vec<String>>) -> Option<Vec<PathBuf>> {
    list
      .as_ref()
      .map(|l| l.iter().map(|p| self.abs(p)).collect())
  }

  /// Whether paths are compared resolved as well as spelled: where a Mode A
  /// enforcer resolves the paths it is given (Landlock opens each rule's
  /// path, Seatbelt checks resolved paths).
  fn resolves(&self) -> bool {
    cfg!(unix) && matches!(self.os(), TargetOs::Linux | TargetOs::Macos)
  }

  /// `path` resolved, or `None` when that can't be told.
  fn resolved(&self, path: &Path) -> Option<PathBuf> {
    let r = resolve_prefix(path);
    r.decided.then(|| r.full())
  }

  /// Whether `path` lies at or beneath `base`, by spelling or resolved.
  fn within(&self, path: &Path, base: &Path) -> Reach {
    if path.starts_with(base) {
      return Reach::Inside;
    }
    if !self.resolves() {
      return Reach::Outside;
    }
    let (Some(path), Some(base)) = (self.resolved(path), self.resolved(base))
    else {
      return Reach::Undecided;
    };
    if path.starts_with(&base) {
      Reach::Inside
    } else {
      Reach::Outside
    }
  }

  /// Whether `path` lies in `scope`, by spelling or resolved: inside one
  /// rule is inside, otherwise any rule that can't be told makes it
  /// undecided.
  fn reach(&self, scope: &PathScope, path: &Path) -> Reach {
    // by spelling first: no file system access
    if scope.covers(path) {
      return Reach::Inside;
    }
    match scope {
      PathScope::All => Reach::Inside,
      PathScope::Paths { paths } => {
        let mut out = Reach::Outside;
        for rule in paths {
          match self.within(path, &rule.path) {
            Reach::Inside => return Reach::Inside,
            Reach::Undecided => out = Reach::Undecided,
            Reach::Outside => {}
          }
        }
        out
      }
    }
  }

  /// The user's allows that lie inside one of `denies` are left out of the
  /// kernel's scope (the deny wins, as in Deno's checks); an allow whose
  /// place can't be told is a fail-open gap (`enforce` refuses). Implicit
  /// entries stay (the runtime needs them; inside a deny they are a gap of
  /// their own).
  fn carve(
    &mut self,
    scope: PathScope,
    denies: &[(PathBuf, String)],
    class: GapClass,
  ) -> PathScope {
    let PathScope::Paths { paths } = scope else {
      return scope;
    };
    let mut kept = Vec::with_capacity(paths.len());
    for rule in paths {
      if rule.implicit.is_some() {
        kept.push(rule);
        continue;
      }
      let mut drop_rule = false;
      for (deny, flag) in denies {
        match self.within(&rule.path, deny) {
          Reach::Inside => {
            self.gap(
              class,
              flag.clone(),
              format!(
                "fail closed: {} is left out of the kernel's {} scope, as it lies inside the denied path (the deny wins, as in Deno's checks)",
                rule.path.display(),
                class.as_str()
              ),
              GapResolution::FailClosed,
              None,
            );
            drop_rule = true;
            break;
          }
          Reach::Undecided => {
            self.gap(
              class,
              flag.clone(),
              format!(
                "Deno only: whether {} lies inside the denied path can't be told (a folder on the way can't be searched)",
                rule.path.display()
              ),
              GapResolution::FailOpen,
              Some(ExactWith::Never),
            );
          }
          Reach::Outside => {}
        }
      }
      if !drop_rule {
        kept.push(rule);
      }
    }
    PathScope::Paths { paths: kept }
  }

  /// The read denies (`--deny-read`, `--ignore-read`) with their flags.
  fn read_denies(&self) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for (list, flag) in [
      (&self.opts().deny_read, "--deny-read"),
      (&self.opts().ignore_read, "--ignore-read"),
    ] {
      for path in self.abs_list(list).unwrap_or_default() {
        let rule = format!("{flag}={}", path.display());
        out.push((path, rule));
      }
    }
    out
  }

  /// Whether the broker's network side holds this policy's network rules:
  /// Mode B with that side built. Without it Mode B compiles the network
  /// (hosts, Unix sockets, module fetches) as Mode A does: what the kernel
  /// holds alone, with the same gaps.
  fn broker_net(&self) -> bool {
    self.shape == Shape::ModeB && BROKER_NET_BUILT
  }

  fn write_denies(&self) -> Vec<(PathBuf, String)> {
    self
      .abs_list(&self.opts().deny_write)
      .unwrap_or_default()
      .into_iter()
      .map(|path| {
        let rule = format!("--deny-write={}", path.display());
        (path, rule)
      })
      .collect()
  }

  fn run(mut self) -> CompiledPolicy {
    let read = self.read_scope();
    let write = self.write_scope();
    // an allow inside a deny of its class is held by leaving it out
    let read_denies = self.read_denies();
    let read = self.carve(read, &read_denies, GapClass::Read);
    let write_denies = self.write_denies();
    let write = self.carve(write, &write_denies, GapClass::Write);
    let deny_read = self.deny_read(&read, &write);
    let deny_write = self.deny_write(&write);
    let (exec, exec_programs) = self.exec();
    let map_exec = self.map_exec(&read, &write);
    let (net, refuse_listen, vsock) = self.net();
    let unix = self.unix(&read, &write);
    let sys = self.sys(&net);
    let env_scrub = self.env_scrub();
    let scopes = self.scopes(&unix);
    // signals can reach only this process: what Landlock's scope (ABI 6)
    // holds, by seccomp, where no child may start (Deno.kill needs
    // --allow-run, which isn't granted)
    let signal_self = self.os() == TargetOs::Linux
      && self.native
      && self.opts().allow_run.is_none()
      && !scopes.signal;
    self.import_gap(&net);
    self.fs_mechanism_gaps(&read, &write);
    self.metadata_gaps(&read, &write);
    let enforce = self.ctx().mode.fails_closed() && !self.ceiling;
    // a deny ceiling changes nothing but the denies: no seccomp program at
    // all (D2), so a setuid child a prompt allowed still works
    let ceiling = self.ceiling;
    let syscalls = SyscallPolicy {
      attack_surface: !self.deny_only,
      refuse_exec: exec == ExecScope::None,
      refuse_listen,
      clone3_enosys: !self.deny_only && self.os() == TargetOs::Linux,
      refuse_io_uring: !ceiling && self.os() == TargetOs::Linux,
      signal_self: signal_self && !ceiling,
      refuse_path_metadata: enforce
        && self.os() == TargetOs::Linux
        && !write.is_all(),
      refuse_path_truncate: enforce
        && self.os() == TargetOs::Linux
        && !write.is_all()
        && self.caps.landlock_abi.is_some()
        && !self.caps.supports(Feature::FsTruncate),
      vsock,
    };
    // what of Mode B this policy leans on: the file side (a write deny
    // held by the broker) is built; the network side (hosts, Unix sockets
    // by path on Linux) is not yet. Module fetches after start fail closed
    // until then, as in Mode A (`import_gap`).
    let needs_net_side = matches!(net, NetPolicy::Broker { .. })
      || (self.os() == TargetOs::Linux
        && matches!(unix, UnixPolicy::Paths { .. }));
    let needs_fs_side = deny_write
      .iter()
      .any(|d| d.enforcement == DenyEnforcement::Broker);
    if self.caps.enforcer_built
      && self.shape == Shape::ModeB
      && ((needs_net_side && !BROKER_NET_BUILT)
        || (needs_fs_side && !BROKER_FS_BUILT))
    {
      let needs = match &net {
        NetPolicy::Broker { .. } => "host-scoped network",
        _ => "these rules",
      };
      self.gap(
        GapClass::Enforcer,
        BROKER_RULE,
        format!(
          "{BROKER_NOT_BUILT}: {needs} would be held by Deno's checks only"
        ),
        GapResolution::FailOpen,
        None,
      );
    }
    if self.ctx().mode.fails_closed()
      && self.os() == TargetOs::Linux
      && !write.is_all()
    {
      self.gap(
        GapClass::Metadata,
        write_rule(self.opts()),
        "fail closed: chmod, chown, utime and extended-attribute writes by path are refused, inside the write scope too (Deno.chmod, Deno.utime, cp -p, tar); the descriptor forms work",
        GapResolution::FailClosed,
        Some(ExactWith::Never),
      );
    }
    if self.caps.enforcer_built
      && self.os() == TargetOs::Linux
      && !self.caps.seccomp
      && self.shape == Shape::ModeA
      && !self.ceiling
    {
      // Mode A's one seccomp program (exec, sockets, the attack surface)
      // can't be installed: probed, so enforce refuses here instead of
      // failing after the re-exec
      self.gap(
        GapClass::Enforcer,
        "--kernel-sandbox (seccomp)",
        "a seccomp filter can't be installed here (a container's or an outer sandbox's seccomp profile refuses it): exec, socket and attack-surface rules would be held by Deno's checks only",
        GapResolution::FailOpen,
        Some(ExactWith::Never),
      );
    }
    if !self.caps.enforcer_built {
      let os = self.os().as_str();
      self.gap(
        GapClass::Enforcer,
        "--kernel-sandbox",
        format!(
          "no kernel enforcer for {os} is built into this runtime yet: every rule is held by Deno's checks only"
        ),
        GapResolution::FailOpen,
        None,
      );
    }
    let os = self.os();
    let shape = self.shape;
    let deny_only = self.deny_only;
    let deny_ceiling = self.ceiling;
    let prebound = std::mem::take(&mut self.prebound);
    let implicit = self.implicit_set().clone();
    let mut gaps = self.gaps;
    gaps.sort_by(|a, b| {
      (a.class, &a.rule, &a.detail).cmp(&(b.class, &b.rule, &b.detail))
    });
    gaps.dedup();
    CompiledPolicy {
      policy: KernelPolicy {
        format: POLICY_FORMAT,
        os,
        shape,
        deny_only,
        deny_ceiling,
        fs: FsRules {
          read,
          write,
          deny_read,
          deny_write,
          exec,
          exec_programs,
          map_exec,
        },
        net,
        prebound,
        unix,
        sys,
        env_scrub,
        scopes,
        syscalls,
      },
      gaps,
      implicit,
    }
  }

  /// A grant list plus the implicit entries with the given accesses.
  fn scope(
    &mut self,
    grant: &Option<Vec<String>>,
    accesses: &[ImplicitAccess],
    writable: Option<&PathScope>,
  ) -> PathScope {
    let grant_paths = match grant {
      Some(list) if list.is_empty() => return PathScope::All,
      Some(_) => self.abs_list(grant).unwrap_or_default(),
      None => Vec::new(),
    };
    let mut paths: Vec<PathRule> = grant_paths
      .into_iter()
      .map(|path| PathRule {
        path,
        implicit: None,
      })
      .collect();
    let implicit = self.implicit_set();
    for entry in &implicit.entries {
      if !accesses.contains(&entry.access) {
        continue;
      }
      // the write + read + dlopen pivot: code-holding implicit paths are
      // dropped where the grant also makes them writable
      if entry.need.holds_code()
        && let Some(writable) = writable
        && writable.grant_covers(&entry.path)
      {
        self.gaps.push(PolicyGap {
          class: GapClass::Ffi,
          rule: format!("implicit {}", entry.path.display()),
          detail: format!(
            "left out of the kernel's read scope ({}): the grant also makes it writable, and a writable, loadable folder lets code plant a library",
            entry.need.as_str()
          ),
          resolution: GapResolution::FailClosed,
          exact_with: None,
        });
        continue;
      }
      paths.push(PathRule {
        path: entry.path.clone(),
        implicit: Some(entry.need),
      });
    }
    sort_rules(&mut paths);
    PathScope::Paths { paths }
  }

  fn read_scope(&mut self) -> PathScope {
    let write_grant = self.opts().allow_write.clone();
    let writable = write_grant
      .as_ref()
      .map(|_| self.scope_without_implicit(&write_grant));
    self.scope(
      &self.opts().allow_read.clone(),
      &[
        ImplicitAccess::Read,
        ImplicitAccess::ReadMap,
        ImplicitAccess::ReadWrite,
        ImplicitAccess::ListDir,
      ],
      writable.as_ref(),
    )
  }

  fn scope_without_implicit(&self, grant: &Option<Vec<String>>) -> PathScope {
    match grant {
      Some(list) if list.is_empty() => PathScope::All,
      Some(_) => {
        let mut paths: Vec<PathRule> = self
          .abs_list(grant)
          .unwrap_or_default()
          .into_iter()
          .map(|path| PathRule {
            path,
            implicit: None,
          })
          .collect();
        sort_rules(&mut paths);
        PathScope::Paths { paths }
      }
      None => PathScope::none(),
    }
  }

  fn write_scope(&mut self) -> PathScope {
    self.scope(
      &self.opts().allow_write.clone(),
      &[ImplicitAccess::ReadWrite],
      None,
    )
  }

  fn deny_read(
    &mut self,
    read: &PathScope,
    write: &PathScope,
  ) -> Vec<DenyRule> {
    let mut denies = Vec::new();
    let lists = [
      (self.abs_list(&self.opts().deny_read), false),
      (self.abs_list(&self.opts().ignore_read), true),
    ];
    for (list, ignore) in lists {
      for path in list.unwrap_or_default() {
        let flag = if ignore {
          "--ignore-read"
        } else {
          "--deny-read"
        };
        let rule = format!("{flag}={}", path.display());
        if ignore {
          self.gap(
            GapClass::Read,
            rule.clone(),
            "native code and children get a permission error, not NotFound",
            GapResolution::Documented,
            None,
          );
        }
        let enforcement = self.read_deny_enforcement(&path, read, write, &rule);
        self.implicit_inside_deny(&path, &rule);
        denies.push(DenyRule {
          path,
          ignore,
          enforcement,
        });
      }
    }
    denies.sort();
    denies.dedup();
    denies
  }

  /// Implicit entries inside a user's deny are kept (the runtime needs them)
  /// and reported.
  fn implicit_inside_deny(&mut self, deny: &Path, rule: &str) {
    let inside: Vec<_> = self
      .implicit
      .entries
      .iter()
      .filter(|e| self.within(&e.path, deny) != Reach::Outside)
      .map(|e| (e.path.clone(), e.need))
      .collect();
    for (path, need) in inside {
      self.gap(
        GapClass::Read,
        rule.to_string(),
        format!(
          "the kernel still allows {} inside it for the runtime ({})",
          path.display(),
          need.as_str()
        ),
        GapResolution::FailOpen,
        Some(ExactWith::Never),
      );
    }
  }

  fn read_deny_enforcement(
    &mut self,
    path: &Path,
    read: &PathScope,
    write: &PathScope,
    rule: &str,
  ) -> DenyEnforcement {
    // Implied only when no allow reaches the deny, by spelling or resolved
    match self.reach(read, path) {
      Reach::Outside => return DenyEnforcement::Implied,
      Reach::Undecided => {
        self.gap(
          GapClass::Read,
          rule,
          "Deno only: whether an allow reaches it can't be told (a folder on the way can't be searched)",
          GapResolution::FailOpen,
          Some(ExactWith::Never),
        );
        return DenyEnforcement::DenoOnly;
      }
      Reach::Inside => {}
    }
    if self.caps.supports(Feature::DenyInsideAllow) {
      return DenyEnforcement::Exact;
    }
    match self.os() {
      TargetOs::Linux => {}
      TargetOs::Windows => {
        // reads aren't kernel-confined on Windows; the read gap says so
        return DenyEnforcement::DenoOnly;
      }
      _ => return DenyEnforcement::DenoOnly,
    }
    // Linux, a deny ceiling (D1 / D2): Landlock needs no-new-privileges,
    // which would change the rest of the run (a setuid program a prompt
    // allowed), and a read expansion freezes folders a prompt may let the
    // program write: the overmount where it is built, else Deno's check
    // only, reported and never refused (decision O4)
    if self.ceiling {
      if OVERMOUNT_BUILT && self.caps.supports(Feature::Overmount) {
        return DenyEnforcement::Overmount;
      }
      self.gap(
        GapClass::Read,
        rule,
        "Deno only: as a deny ceiling Linux holds it only by overmounting it in a user namespace (Landlock would need no-new-privileges and an expanded allow, which change the rest of the run)",
        GapResolution::Documented,
        Some(ExactWith::UserNamespaces),
      );
      return DenyEnforcement::DenoOnly;
    }
    // Linux (review F7)
    let parent = path.parent().unwrap_or(path);
    let stable = self.reach(write, parent) == Reach::Outside;
    let enforce = self.ctx().mode.fails_closed();
    if OVERMOUNT_BUILT && self.caps.supports(Feature::Overmount) {
      return DenyEnforcement::Overmount;
    }
    if stable {
      return DenyEnforcement::Expand;
    }
    if enforce {
      self.gap(
        GapClass::Read,
        rule,
        "held by expanding the allow: files created or replaced (atomic saves) in the expanded folders after start are unreadable",
        GapResolution::Documented,
        Some(ExactWith::UserNamespaces),
      );
      return DenyEnforcement::Expand;
    }
    self.gap(
      GapClass::Read,
      rule,
      "Deno only: Landlock can't deny inside an allowed folder, and the grant writes the folders an expansion would freeze",
      GapResolution::FailOpen,
      Some(ExactWith::UserNamespaces),
    );
    DenyEnforcement::DenoOnly
  }

  fn deny_write(&mut self, write: &PathScope) -> Vec<DenyRule> {
    let mut denies = Vec::new();
    for path in self.abs_list(&self.opts().deny_write).unwrap_or_default() {
      let rule = format!("--deny-write={}", path.display());
      let reach = self.reach(write, &path);
      let enforcement = if reach == Reach::Outside {
        DenyEnforcement::Implied
      } else if reach == Reach::Undecided {
        self.gap(
          GapClass::Write,
          rule,
          "Deno only: whether an allow reaches it can't be told (a folder on the way can't be searched)",
          GapResolution::FailOpen,
          Some(ExactWith::Never),
        );
        DenyEnforcement::DenoOnly
      } else if self.caps.supports(Feature::DenyInsideAllow) {
        DenyEnforcement::Exact
      } else if self.os() == TargetOs::Linux && self.shape == Shape::ModeB {
        DenyEnforcement::Broker
      } else if self.os() == TargetOs::Linux {
        self.gap(
          GapClass::Write,
          rule,
          "Deno only: Landlock can't deny inside an allowed folder",
          GapResolution::FailOpen,
          Some(ExactWith::Broker),
        );
        DenyEnforcement::DenoOnly
      } else {
        self.gap(
          GapClass::Write,
          rule,
          "Deno only: no mechanism here denies inside an allowed folder",
          GapResolution::FailOpen,
          Some(ExactWith::Never),
        );
        DenyEnforcement::DenoOnly
      };
      denies.push(DenyRule {
        path,
        ignore: false,
        enforcement,
      });
    }
    denies.sort();
    denies.dedup();
    denies
  }

  fn exec(&mut self) -> (ExecScope, Vec<PathBuf>) {
    let strict = self.ctx().exec_strict;
    match &self.opts().allow_run {
      None => (ExecScope::None, Vec::new()),
      Some(list) if list.is_empty() && strict => {
        self.gap(
          GapClass::Run,
          "--kernel-sandbox-exec=strict --allow-run",
          "a strict exec scope needs the programs listed (--allow-run=<programs>); with an unscoped --allow-run any readable program can start",
          GapResolution::FailOpen,
          Some(ExactWith::Never),
        );
        (ExecScope::Unrestricted, Vec::new())
      }
      Some(list) => {
        if let Some(deny) = &self.opts().deny_run {
          self.gap(
            GapClass::Run,
            format!("--deny-run={}", deny.join(",")),
            "Deno only: the kernel can't tell programs apart once running is allowed",
            GapResolution::Documented,
            None,
          );
        }
        if list.is_empty() {
          return (ExecScope::Unrestricted, Vec::new());
        }
        if strict {
          let programs = self.exec_closure(list);
          self.gap(
            GapClass::Run,
            format!(
              "--kernel-sandbox-exec=strict --allow-run={}",
              list.join(",")
            ),
            "the whole process tree may start only these programs and their interpreters; a program that starts others (git runs ssh, a compiler its assembler) needs them listed too",
            GapResolution::Documented,
            None,
          );
          return (ExecScope::Listed, programs);
        }
        self.gap(
          GapClass::Run,
          format!("--allow-run={}", list.join(",")),
          "the kernel lets native code and children start any readable program; the protection is that every child inherits the file and network rules (--kernel-sandbox-exec=strict holds the tree to the list)",
          GapResolution::Documented,
          None,
        );
        (ExecScope::Unrestricted, Vec::new())
      }
    }
  }

  /// The files the run list needs to execute: each entry found as Deno
  /// finds it (a path against the working directory, a name on `PATH`),
  /// then its interpreters ([`deno_sandbox::exec::program_closure`]).
  #[cfg(unix)]
  fn exec_closure(&self, list: &[String]) -> Vec<PathBuf> {
    let path = std::env::var_os("PATH");
    let programs: Vec<PathBuf> = list
      .iter()
      .filter_map(|entry| {
        if entry.contains('/') {
          return Some(self.ctx().cwd.join(entry));
        }
        std::env::split_paths(path.as_ref()?)
          .map(|dir| dir.join(entry))
          .find(|p| p.is_file())
      })
      .collect();
    deno_sandbox::exec::program_closure(&programs, path.as_deref())
  }

  #[cfg(not(unix))]
  fn exec_closure(&self, _list: &[String]) -> Vec<PathBuf> {
    Vec::new()
  }

  fn map_exec(&mut self, read: &PathScope, write: &PathScope) -> PathScope {
    // children run programs (and their libraries) from anywhere they read;
    // unscoped FFI loads anything readable
    let ffi_all = self.opts().allow_ffi.as_ref().is_some_and(|l| l.is_empty());
    if self.opts().allow_run.is_some() || ffi_all {
      return read.clone();
    }
    // macOS: a library outside the --allow-ffi paths still loaded under a
    // profile that wrote file-map-executable for them alone (macOS 26.6), so
    // the model claims only what holds: the read scope (plus the system
    // code the profile always maps), as on Linux
    if self.os() == TargetOs::Macos {
      if let Some(list) = &self.opts().allow_ffi {
        self.gap(
          GapClass::Ffi,
          format!("--allow-ffi={}", list.join(",")),
          "macOS doesn't hold library loading to the --allow-ffi paths (file-map-executable didn't refuse in testing): native code may load any library in the read scope",
          GapResolution::Documented,
          None,
        );
      }
      return read.clone();
    }
    let ffi = self.opts().allow_ffi.clone();
    let scope = self.scope(&ffi, &[ImplicitAccess::ReadMap], Some(write));
    if self.os() == TargetOs::Linux
      && let Some(list) = &self.opts().allow_ffi
    {
      self.gap(
        GapClass::Ffi,
        format!("--allow-ffi={}", list.join(",")),
        "Linux can't scope library loading: native code may load any library in the read scope",
        GapResolution::Documented,
        None,
      );
    }
    scope
  }

  /// The network policy, whether `listen` is refused, and whether
  /// `AF_VSOCK` stays allowed.
  fn net(&mut self) -> (NetPolicy, bool, bool) {
    let o = self.opts();
    let connect = NetSide::merged(&o.allow_net, &o.allow_net_connect);
    let bind = NetSide::merged(&o.allow_net, &o.allow_net_listen);
    let deny: Vec<String> =
      [&o.deny_net, &o.deny_net_connect, &o.deny_net_listen]
        .into_iter()
        .flatten()
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let vsock = connect
      .entries()
      .iter()
      .chain(bind.entries())
      .any(|e| classify_net(e) == NetEntry::Vsock)
      || connect.is_any()
      || bind.is_any();

    let ip_entries = |side: &NetSide| -> Vec<NetEntry> {
      side
        .entries()
        .iter()
        .map(|e| classify_net(e))
        .filter(|e| !matches!(e, NetEntry::Unix(_) | NetEntry::Vsock))
        .collect()
    };
    let connect_ip = ip_entries(&connect);
    let mut bind_ip = ip_entries(&bind);
    let inspector = self.ctx().inspector_port;
    if let Some(port) = inspector {
      bind_ip.push(NetEntry::AddressPort(port));
    }
    let connect_none =
      connect.list.is_none() || (!connect.is_any() && connect_ip.is_empty());
    let mut bind_none = (bind.list.is_none()
      || (!bind.is_any() && bind_ip.is_empty()))
      && inspector.is_none();
    // Linux, Mode A: listeners `--allow-net-listen` names by address are
    // bound before the process confines itself and handed to the operation
    // that listens there, so the kernel policy has no bind side at all
    // (review F2): a listen-only server runs with no IP socket of its own
    // on every kernel, and the address is held, not only its port. Only
    // the listen-direction flag: an `--allow-net` entry is most often a
    // server to connect to, whose port this process must not take.
    let hosted_connect = connect_ip.iter().any(|e| *e == NetEntry::Hosted);
    let listen_only = NetSide::merged(&None, &o.allow_net_listen);
    let allow_net_binds = !ip_entries(&NetSide::merged(&o.allow_net, &None))
      .is_empty()
      || o.allow_net.as_ref().is_some_and(|list| list.is_empty());
    if self.os() == TargetOs::Linux
      && self.shape == Shape::ModeA
      && !hosted_connect
      && !allow_net_binds
      && inspector.is_none()
      && let Some(prebound) = prebound_listeners(&listen_only)
    {
      self.gap(
        GapClass::Net,
        format!("--allow-net-listen={}", listen_only.entries().join(",")),
        "bound before the sandbox applies: each address is bound (and listening) from startup for the life of the process, whether or not the program listens there; UDP on these addresses is refused",
        GapResolution::Documented,
        Some(ExactWith::Broker),
      );
      self.prebound = prebound;
      bind_ip.clear();
      bind_none = true;
    }

    if connect_none && bind_none {
      return (NetPolicy::Denied, true, vsock);
    }
    let refuse_listen = bind_none;
    let hosted = connect_ip
      .iter()
      .chain(bind_ip.iter())
      .any(|e| *e == NetEntry::Hosted);
    let os = self.os();
    let ports_supported = self.caps.supports(Feature::TcpPorts);

    // Mode B: hosts held exactly by the broker. A name needs it on Linux
    // and macOS alike (no kernel scopes by host); so does every scoped list
    // or deny under an explicit `broker`. A network with nothing to hold
    // (unscoped, no deny) stays the process's own.
    let nothing_to_hold = connect.is_any() && bind.is_any() && deny.is_empty();
    let use_broker = (self.broker_net() && !nothing_to_hold)
      || (hosted && os == TargetOs::Linux)
      || (hosted
        && os == TargetOs::Macos
        && (BROKER_NET_BUILT || self.has_portless(&connect, &bind)));
    if use_broker && os != TargetOs::Windows {
      let host_scope = |side: &NetSide, extra: Option<u16>| -> HostScope {
        if side.is_any() {
          return HostScope::Any;
        }
        let mut hosts: Vec<String> = side
          .entries()
          .iter()
          .filter(|e| {
            !matches!(classify_net(e), NetEntry::Unix(_) | NetEntry::Vsock)
          })
          .cloned()
          .collect();
        if let Some(port) = extra {
          hosts.push(format!("127.0.0.1:{port}"));
        }
        hosts.sort();
        hosts.dedup();
        if hosts.is_empty() {
          HostScope::None
        } else {
          HostScope::Hosts { hosts }
        }
      };
      return (
        NetPolicy::Broker {
          connect: host_scope(&connect, None),
          bind: host_scope(&bind, inspector),
          deny,
        },
        refuse_listen,
        vsock,
      );
    }

    let rule_text = |side: &NetSide, flag: &str| -> String {
      if side.is_any() {
        flag.to_string()
      } else {
        format!("{flag}={}", side.entries().join(","))
      }
    };
    for d in &deny {
      self.gap(
        GapClass::Net,
        format!("--deny-net={d}"),
        "Deno only: no kernel can deny a host",
        GapResolution::FailOpen,
        Some(ExactWith::Broker),
      );
    }
    if os == TargetOs::Windows {
      if !(connect.is_any() && bind.is_any()) {
        self.gap(
          GapClass::Net,
          rule_text(&connect, "--allow-net"),
          "Deno only: the network isn't kernel-confined on Windows",
          GapResolution::Documented,
          Some(ExactWith::Never),
        );
      }
      return (NetPolicy::Open, false, vsock);
    }
    if connect.is_any() && bind.is_any() {
      return (NetPolicy::Open, false, vsock);
    }
    let port_scope =
      |any: bool, none: bool, entries: &[NetEntry]| -> PortScope {
        if any {
          return PortScope::Any;
        }
        if none {
          return PortScope::None;
        }
        let ports: BTreeSet<u16> = entries
          .iter()
          .filter_map(|e| match e {
            NetEntry::AddressPort(p) => Some(*p),
            _ => None,
          })
          .collect();
        // a macOS name with a port is held by port (hosts by Deno only)
        PortScope::Ports {
          ports: ports.into_iter().collect(),
        }
      };
    let mut connect_ports =
      port_scope(connect.is_any(), connect_none, &connect_ip);
    let mut bind_ports = port_scope(bind.is_any(), bind_none, &bind_ip);
    // macOS names with ports: their ports go in, hosts are Deno-only
    if os == TargetOs::Macos {
      for (side, scope) in
        [(&connect, &mut connect_ports), (&bind, &mut bind_ports)]
      {
        if let PortScope::Ports { ports } = scope {
          for e in side.entries() {
            if let Ok(NetDescriptor(
              Host::Fqdn(_) | Host::FqdnWithSubdomainWildcard(_),
              Some(port),
            )) = NetDescriptor::parse_for_list(e)
              && let Ok(port) = u16::try_from(port)
            {
              ports.push(port);
            }
          }
          ports.sort_unstable();
          ports.dedup();
        }
      }
      if hosted {
        let rule = rule_text(&connect, "--allow-net");
        self.gap(
          GapClass::Net,
          rule,
          "coarser: the kernel holds the ports, not the hosts; native code and children may reach any host on them and resolve any name",
          // the brief's macOS port rules: the coarsening is documented
          GapResolution::Documented,
          Some(ExactWith::Broker),
        );
      }
    }
    let scoped_side = |s: &PortScope| matches!(s, PortScope::Ports { .. });
    if scoped_side(&connect_ports) || scoped_side(&bind_ports) {
      if !ports_supported {
        let abi =
          Capabilities::landlock_abi_for(Feature::TcpPorts).unwrap_or(4);
        self.gap(
          GapClass::Net,
          rule_text(&connect, "--allow-net"),
          format!(
            "Deno only: TCP port rules need Landlock ABI {abi}; the kernel allows every TCP port here"
          ),
          GapResolution::FailOpen,
          Some(ExactWith::Broker),
        );
      } else {
        let addresses: Vec<&String> = connect
          .entries()
          .iter()
          .filter(|e| is_ip_literal(e) || e.starts_with("localhost"))
          .collect();
        if !addresses.is_empty() {
          self.gap(
            GapClass::Net,
            rule_text(&connect, "--allow-net"),
            "coarser: the kernel holds the ports, not the addresses; native code and children may reach any address on these ports (169.254.169.254 included)",
            // IP-literal / localhost port lists are what Mode A holds
            // the coarsening is documented
            GapResolution::Documented,
            Some(ExactWith::Broker),
          );
        }
      }
    }
    (
      NetPolicy::Ports {
        connect: connect_ports,
        bind: bind_ports,
      },
      refuse_listen,
      vsock,
    )
  }

  fn has_portless(&self, connect: &NetSide, bind: &NetSide) -> bool {
    connect.entries().iter().chain(bind.entries()).any(|e| {
      match NetDescriptor::parse_for_list(e) {
        Ok(NetDescriptor(Host::UnixSocket(_) | Host::Vsock(_), _)) => false,
        Ok(NetDescriptor(_, port)) => port.is_none(),
        Err(_) => true,
      }
    })
  }

  fn unix(&mut self, read: &PathScope, write: &PathScope) -> UnixPolicy {
    let o = self.opts();
    let connect = NetSide::merged(&o.allow_net, &o.allow_net_connect);
    let bind = NetSide::merged(&o.allow_net, &o.allow_net_listen);
    let named: Vec<PathBuf> = connect
      .entries()
      .iter()
      .chain(bind.entries())
      .filter_map(|e| match classify_net(e) {
        NetEntry::Unix(path) => Some(path),
        _ => None,
      })
      .collect::<BTreeSet<_>>()
      .into_iter()
      .collect();
    let unscoped_net = connect.is_any() || bind.is_any();
    let os = self.os();
    if os == TargetOs::Windows {
      return if unscoped_net || !named.is_empty() {
        UnixPolicy::Open
      } else {
        UnixPolicy::Denied
      };
    }
    if self.allow_all {
      // -A: every socket, as Deno's checks allow
      return UnixPolicy::Open;
    }
    if self.deny_only {
      // -A with denies: every socket the user can reach
      if os == TargetOs::Linux && !self.broker_net() {
        self.gap(
          GapClass::Unix,
          "--allow-all",
          "every Unix socket the user can reach is reachable (the session bus, Docker, ssh-agent)",
          GapResolution::FailOpen,
          Some(ExactWith::Broker),
        );
      }
      return UnixPolicy::Open;
    }
    if unscoped_net {
      // Deno allows any socket inside the read + write scope
      if read.is_all() && write.is_all() {
        if os == TargetOs::Linux && !self.broker_net() {
          self.gap(
            GapClass::Unix,
            "--allow-net --allow-read --allow-write",
            "every Unix socket the user can reach is reachable (the session bus, Docker, ssh-agent)",
            GapResolution::FailOpen,
            Some(ExactWith::Broker),
          );
        }
        return UnixPolicy::Open;
      }
      if os == TargetOs::Macos || self.broker_net() {
        // held per path: the folders both read and written
        let mut paths: Vec<PathBuf> = match (read, write) {
          (_, PathScope::Paths { paths }) if read.is_all() => paths
            .iter()
            .filter(|r| r.implicit.is_none())
            .map(|r| r.path.clone())
            .collect(),
          (PathScope::Paths { paths }, _) if write.is_all() => paths
            .iter()
            .filter(|r| r.implicit.is_none())
            .map(|r| r.path.clone())
            .collect(),
          (PathScope::Paths { paths: r }, PathScope::Paths { paths: w }) => r
            .iter()
            .filter(|r| r.implicit.is_none())
            .filter_map(|r| {
              w.iter().filter(|w| w.implicit.is_none()).find_map(|w| {
                if r.path.starts_with(&w.path) {
                  Some(r.path.clone())
                } else if w.path.starts_with(&r.path) {
                  Some(w.path.clone())
                } else {
                  None
                }
              })
            })
            .collect(),
          _ => Vec::new(),
        };
        paths.extend(named);
        paths.sort();
        paths.dedup();
        return if paths.is_empty() {
          UnixPolicy::Denied
        } else {
          UnixPolicy::Paths { paths }
        };
      }
      self.gap(
        GapClass::Unix,
        "--allow-net",
        "fail closed: Unix sockets Deno would allow inside the read and write scope are refused by the kernel; name each with --allow-net=unix:<path>",
        GapResolution::FailClosed,
        Some(ExactWith::Broker),
      );
      return if named.is_empty() {
        UnixPolicy::Denied
      } else {
        self.named_unix(named)
      };
    }
    if named.is_empty() {
      return UnixPolicy::Denied;
    }
    self.named_unix(named)
  }

  fn named_unix(&mut self, named: Vec<PathBuf>) -> UnixPolicy {
    if self.os() == TargetOs::Linux
      && !self.broker_net()
      && !self.caps.supports(Feature::UnixSocketPaths)
    {
      let rule = named
        .iter()
        .map(|p| format!("unix:{}", p.display()))
        .collect::<Vec<_>>()
        .join(",");
      self.gap(
        GapClass::Unix,
        format!("--allow-net={rule}"),
        "every Unix socket the user can reach is reachable: Linux can't scope a socket connect by path without the broker",
        GapResolution::FailOpen,
        Some(ExactWith::Broker),
      );
      return UnixPolicy::Open;
    }
    UnixPolicy::Paths { paths: named }
  }

  fn sys(&mut self, net: &NetPolicy) -> SysNarrowing {
    let o = self.opts();
    let granted = |name: &str| sys_granted(o, name);
    let mut out = SysNarrowing::default();
    match self.os() {
      TargetOs::Linux => {
        for (name, file) in [
          ("loadavg", "/proc/loadavg"),
          ("systemMemoryInfo", "/proc/meminfo"),
          ("osUptime", "/proc/uptime"),
          ("networkInterfaces", "/proc/net"),
        ] {
          if !granted(name) {
            out.hidden_files.push(PathBuf::from(file));
          }
        }
        out.netlink_route =
          granted("networkInterfaces") || !matches!(net, NetPolicy::Denied);
        // other processes' /proc entries come and go (rules attach to
        // inodes), and Landlock's ptrace scoping refuses them anyway
        // the runtime's own reads, whatever --allow-sys says: Landlock rules
        // attach to whole folders here (per-pid inodes, cgroup globs), so the
        // kernel is wider than the processInfo / cpus / systemMemoryInfo
        // file sets, never narrower
        self.gap(
          GapClass::Sys,
          "implicit /proc/self, /sys/fs/cgroup",
          "documented: the runtime reads its own /proc/self entries and the cgroup CPU / memory limits (V8 and tokio size themselves from them), so native code can read them whatever --allow-sys grants (processInfo, cpus, systemMemoryInfo hold them for Deno's APIs)",
          GapResolution::Documented,
          Some(ExactWith::Never),
        );
        if granted("processes") {
          self.gap(
            GapClass::Sys,
            "--allow-sys=processes",
            "fail closed: other processes' /proc/<pid> entries are not readable under the kernel sandbox (Landlock can't scope them by name)",
            GapResolution::FailClosed,
            Some(ExactWith::Never),
          );
        }
        if self.opts().allow_env.as_ref().is_some_and(|l| l.is_empty())
          && !self.deny_only
        {
          self.gap(
            GapClass::Env,
            "--allow-env",
            "fail closed: other processes' /proc/<pid>/environ is not readable under the kernel sandbox",
            GapResolution::FailClosed,
            Some(ExactWith::Never),
          );
        }
      }
      TargetOs::Macos => {
        out.denied_sysctls.push("kern.proc*".to_string());
        if !granted("loadavg") {
          out.denied_sysctls.push("vm.loadavg".to_string());
        }
        if !granted("systemMemoryInfo") {
          out.denied_sysctls.push("hw.memsize".to_string());
        }
      }
      _ => {}
    }
    // shared memory by name: the runtime and the system libraries use it,
    // and neither mechanism scopes it to this process
    let shm = match self.os() {
      TargetOs::Linux => Some("/dev/shm"),
      TargetOs::Macos => Some("POSIX shared memory and semaphores"),
      _ => None,
    };
    if let Some(shm) = shm
      && self.native
      && !self.deny_only
    {
      self.gap(
        GapClass::Sys,
        format!("implicit {shm}"),
        "documented: named shared memory isn't scoped to this process: native code can open the user's other named shared memory by its name",
        GapResolution::Documented,
        Some(ExactWith::Never),
      );
    }
    let unscoped = self.opts().allow_sys.as_ref().is_some_and(|l| l.is_empty());
    if !unscoped {
      let narrowed: Vec<String> = out
        .hidden_files
        .iter()
        .map(|p| p.display().to_string())
        .chain(out.denied_sysctls.iter().cloned())
        .collect();
      let rule = match &self.opts().allow_sys {
        None => "no --allow-sys".to_string(),
        Some(list) => format!("--allow-sys={}", list.join(",")),
      };
      self.gap(
        GapClass::Sys,
        rule,
        if narrowed.is_empty() {
          "sys is enforced by Deno; the kernel narrows nothing here".to_string()
        } else {
          format!(
            "sys is enforced by Deno; the kernel narrows: {}",
            narrowed.join(", ")
          )
        },
        GapResolution::Documented,
        Some(ExactWith::Never),
      );
    }
    out
  }

  fn env_scrub(&mut self) -> Option<Vec<String>> {
    let unscoped = self.opts().allow_env.as_ref().is_some_and(|l| l.is_empty());
    if unscoped {
      return None;
    }
    // enforce re-executes on Linux and macOS: the new image gets only what
    // the grant names
    let scrub = self.ctx().mode.fails_closed()
      && (matches!(self.os(), TargetOs::Linux | TargetOs::Macos)
        || self.shape == Shape::ModeB);
    let rule = match &self.opts().allow_env {
      None => "no --allow-env".to_string(),
      Some(list) => format!("--allow-env={}", list.join(",")),
    };
    if !scrub {
      self.gap(
        GapClass::Env,
        rule,
        "env is enforced by Deno: native code reads the process environment directly (scrubbed only under --kernel-sandbox=enforce where the runtime re-executes)",
        GapResolution::Documented,
        Some(ExactWith::Never),
      );
      return None;
    }
    let mut keep: Vec<String> =
      RUNTIME_ENV.iter().map(|s| s.to_string()).collect();
    keep.extend(self.opts().allow_env.iter().flatten().cloned());
    keep.sort();
    keep.dedup();
    Some(keep)
  }

  fn scopes(&mut self, unix: &UnixPolicy) -> Scopes {
    let scopes = Scopes {
      signal: self.caps.supports(Feature::ScopeSignal),
      abstract_unix: self.caps.supports(Feature::ScopeAbstractUnix),
    };
    let linux = self.os() == TargetOs::Linux;
    // only native code (FFI, addons, children) can signal or reach sockets
    // around Deno's APIs; signals without Landlock's scope (before ABI 6)
    // are held by seccomp when nothing may start a child
    // (SyscallPolicy::signal_self)
    if linux
      && self.native
      && !self.allow_all
      && *unix != UnixPolicy::Denied
      && !scopes.abstract_unix
    {
      self.gap(
        GapClass::Unix,
        "abstract Unix sockets",
        "native code can reach any abstract Unix socket on the machine",
        GapResolution::FailOpen,
        Capabilities::landlock_abi_for(Feature::ScopeAbstractUnix)
          .map(|abi| ExactWith::LandlockAbi { abi }),
      );
    }
    scopes
  }

  /// Remote modules fetched after start (dynamic `import()`, npm
  /// auto-install) need the network the kernel policy may not grant.
  fn import_gap(&mut self, net: &NetPolicy) {
    // the broker's network side holds the network grant, not the import
    // list: a fetch after start is still refused (a host the net grant
    // doesn't name) or has no socket at all, so Mode B fails closed like
    // Mode A until the broker has an import class
    if self.ctx().compiled || matches!(net, NetPolicy::Open) {
      return;
    }
    if let Some(list) = &self.opts().allow_import {
      let rule = if list.is_empty() {
        "--allow-import".to_string()
      } else {
        format!("--allow-import={}", list.join(","))
      };
      self.gap(
        GapClass::Import,
        rule,
        "fail closed: remote modules not cached before start (a dynamic import, an npm auto-install) can't be fetched; run `deno cache` first",
        GapResolution::FailClosed,
        Some(ExactWith::Broker),
      );
    }
  }

  /// Gaps where the machine lacks the mechanism for a whole class.
  fn fs_mechanism_gaps(&mut self, read: &PathScope, write: &PathScope) {
    let os = self.os();
    let linux_abi = |f| {
      Capabilities::landlock_abi_for(f)
        .map(|abi| ExactWith::LandlockAbi { abi })
    };
    let windows = os == TargetOs::Windows;
    if !read.is_all() && !self.caps.supports(Feature::FsRead) {
      let (detail, resolution, exact) = if windows {
        (
          "Deno only: reads aren't kernel-confined on Windows",
          GapResolution::Documented,
          Some(ExactWith::Never),
        )
      } else {
        (
          "Deno only: no OS mechanism here confines reads",
          GapResolution::FailOpen,
          linux_abi(Feature::FsRead),
        )
      };
      self.gap(
        GapClass::Read,
        read_rule(self.opts()),
        detail,
        resolution,
        exact,
      );
    }
    if !write.is_all()
      && !self.caps.supports(Feature::FsWrite)
      && !self.caps.supports(Feature::WriteConfinement)
    {
      self.gap(
        GapClass::Write,
        write_rule(self.opts()),
        "Deno only: no OS mechanism here confines writes",
        GapResolution::FailOpen,
        linux_abi(Feature::FsWrite),
      );
    }
    if os == TargetOs::Linux
      && self.caps.landlock_abi.is_some()
      && !write.is_all()
    {
      if !self.caps.supports(Feature::FsRefer) {
        self.gap(
          GapClass::Write,
          write_rule(self.opts()),
          "fail closed: renaming or linking a file across folders is refused (Landlock before REFER)",
          GapResolution::FailClosed,
          linux_abi(Feature::FsRefer),
        );
      }
      if !self.caps.supports(Feature::FsTruncate) {
        if self.ctx().mode.fails_closed() {
          // seccomp closes it (SyscallPolicy::refuse_path_truncate)
          self.gap(
            GapClass::Write,
            write_rule(self.opts()),
            "fail closed: truncate(2) by path, read-only truncating opens and openat2 are refused (Landlock before TRUNCATE); ftruncate on a file opened for writing works",
            GapResolution::FailClosed,
            linux_abi(Feature::FsTruncate),
          );
        } else {
          self.gap(
            GapClass::Write,
            write_rule(self.opts()),
            "truncating a readable file outside the write scope isn't refused (Landlock before TRUNCATE)",
            GapResolution::FailOpen,
            linux_abi(Feature::FsTruncate),
          );
        }
      }
    }
  }

  fn metadata_gaps(&mut self, read: &PathScope, write: &PathScope) {
    if !self.native {
      return;
    }
    let os = self.os();
    if !read.is_all() && !self.caps.supports(Feature::MetadataRead) {
      self.gap(
        GapClass::Metadata,
        read_rule(self.opts()),
        "native code can see whether any file exists, and its size and times",
        GapResolution::Documented,
        Some(ExactWith::Never),
      );
    } else if !read.is_all() && os == TargetOs::Macos {
      self.gap(
        GapClass::Metadata,
        read_rule(self.opts()),
        "file metadata stays readable everywhere (path resolution needs it): native code can see whether any file exists, and its size",
        GapResolution::Documented,
        Some(ExactWith::Never),
      );
    }
    if os == TargetOs::Linux
      && !write.is_all()
      && !self.ctx().mode.fails_closed()
    {
      self.gap(
        GapClass::Metadata,
        write_rule(self.opts()),
        "native code can change the mode, group, times and extended attributes of the user's files outside the write scope (refused under --kernel-sandbox=enforce)",
        GapResolution::Documented,
        Some(ExactWith::Never),
      );
    }
  }
}

fn read_rule(options: &PermissionsOptions) -> String {
  match &options.allow_read {
    None => "no --allow-read".to_string(),
    Some(list) if list.is_empty() => "--allow-read".to_string(),
    Some(list) => format!("--allow-read={}", list.join(",")),
  }
}

fn write_rule(options: &PermissionsOptions) -> String {
  match &options.allow_write {
    None => "no --allow-write".to_string(),
    Some(list) if list.is_empty() => "--allow-write".to_string(),
    Some(list) => format!("--allow-write={}", list.join(",")),
  }
}

/// Sorts rules by path and keeps one per path, preferring the user's grant.
fn sort_rules(rules: &mut Vec<PathRule>) {
  rules.sort_by(|a, b| {
    a.path
      .cmp(&b.path)
      .then_with(|| a.implicit.is_some().cmp(&b.implicit.is_some()))
      .then_with(|| a.implicit.cmp(&b.implicit))
  });
  rules.dedup_by(|b, a| a.path == b.path);
}

#[cfg(all(test, unix))]
mod tests;
