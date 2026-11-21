// Copyright 2018-2026 the Deno authors. MIT license.

//! The enforcement plan: what a process applies to itself under
//! `--kernel-sandbox=enforce`, in the terms each mechanism takes. The CLI
//! compiles its `KernelPolicy` (the policy model, `cli/sandbox/policy.rs`)
//! into one; on Linux the plan is what the first image hands the re-executed
//! one in a sealed memfd, and on macOS what the Seatbelt profile is written
//! from. It holds no decisions of its own: every rule here was decided by the
//! policy model, and a plan that doesn't parse is refused, never widened.

use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

/// Bumped whenever the serialized shape changes; a plan of another format is
/// refused.
pub const PLAN_FORMAT: u32 = 2;

/// The largest plan a hand-over carries (a read expansion is at most some
/// tens of thousands of paths).
pub const MAX_PLAN_BYTES: usize = 16 * 1024 * 1024;

/// The most entries a read expansion may allow (each is a Landlock rule).
pub const MAX_EXPANDED: usize = 20_000;

/// The internal variable naming the descriptor that holds the plan across
/// the re-exec (Linux: a sealed memfd; macOS `sandbox-exec` fallback: an
/// unlinked file). Never trusted on its own: the plan it names is applied
/// (it can only narrow) and must equal the plan the new image compiles.
pub const PLAN_FD_ENV_VAR: &str = "DENO_INTERNAL_KERNEL_PLAN_FD";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnforcePlan {
  pub format: u32,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub linux: Option<LinuxPlan>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub macos: Option<MacosPlan>,
  /// Windows, the small tier (DENEXT.md item 474).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub windows: Option<WindowsPlan>,
  /// The environment the re-executed image keeps (names; a trailing `*`
  /// matches a prefix). `None`: kept as it is.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub env_keep: Option<Vec<String>>,
  /// A path that opens before the plan applies and must not open after
  /// (the measured check that the sandbox holds).
  #[serde(skip_serializing_if = "Option::is_none")]
  pub probe: Option<PathBuf>,
  /// The local files of the program's static module graph, readable
  /// (regular files only, never a folder): Deno loads its own modules
  /// without a read grant. Not part of the equality the re-executed image
  /// checks; it verifies instead that each is a module of its own graph.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub module_files: Vec<PathBuf>,
  /// The entries a read expansion allows (Linux, [`LinuxPlan::expand`]):
  /// walked by the first image, which can still list the expanded folders
  /// (the re-executed one can't). Not part of the equality either; the new
  /// image checks each against the roots and denies it compares
  /// ([`EnforcePlan::check_expanded`]), and they are applied with no symlink
  /// anywhere in their resolution.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub expanded: Vec<PathBuf>,
  /// The runtime configuration variables the first image saw (registries,
  /// resolution, unstable features), name and value: carried through the
  /// re-exec and part of the equality, so an image that would resolve or
  /// behave differently refuses to start.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub config_env: Vec<(String, String)>,
  /// The policy-keyed cache overlay (`DENO_DIR/sandboxed/<policy hash>`)
  /// the confined image keeps its caches in (DENEXT.md item 451); its write
  /// rule is among the plan's rules. Part of the equality.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub cache_overlay: Option<PathBuf>,
  /// `--kernel-sandbox=broker`: the process runs under a supervising
  /// broker, which holds these grants (Deno's own read and write rules,
  /// exactly). Part of the equality: the confined image runs only under the
  /// broker policy it compiles itself.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub broker: Option<BrokerPlan>,
  /// The entries a write expansion allows (Linux,
  /// [`LinuxPlan::expand_write`]): like [`Self::expanded`], measured by the
  /// first image and checked against the roots by the new one.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub expanded_write: Vec<PathBuf>,
  /// The modules a `deno test` / `deno bench` runs, as the first image
  /// found, filtered and type-checked them (WP-A of denext.30): the confined
  /// image runs exactly these instead of walking folders it may not read or
  /// asking git which files changed. Not part of the equality; the new image
  /// checks each against its own include patterns, and the local ones are
  /// among [`Self::module_files`].
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub entries: Vec<PlanEntry>,
  /// The config files of the workspace members the first image found by
  /// expanding a glob member (`"workspaces": ["packages/*"]`), in the order
  /// found: the confined image can't list the folders a glob walks, so it
  /// expands its globs from these (each glob keeps only the files its own
  /// patterns match). Not part of the equality; the config files both
  /// images discover are (the policy reads them).
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub member_configs: Vec<PathBuf>,
}

/// A module a test or bench run runs ([`EnforcePlan::entries`]).
#[derive(
  Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanEntry {
  /// The module's specifier (a `file:` URL, or a remote one an include
  /// names).
  pub specifier: String,
  /// Its tests or benchmarks run.
  pub run: bool,
  /// The code blocks of its documentation run (`deno test --doc`).
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub doc: bool,
}

/// What the supervising broker holds: the run's read and write grants and
/// denies as Deno's checks hold them (the user's, never the runtime's
/// implicit set). `None` lists are unscoped grants. Every scope appears as
/// written and, where it differs, resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrokerPlan {
  pub read: Option<Vec<PathBuf>>,
  pub deny_read: Vec<PathBuf>,
  pub write: Option<Vec<PathBuf>>,
  pub deny_write: Vec<PathBuf>,
  /// The run's network grant, when the broker holds the network (a
  /// host-scoped `--allow-net`): the confined process then makes no IP
  /// socket of its own and asks the broker for every one.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub net: Option<BrokerNet>,
}

/// The network lists of a run, as its flags wrote them (`None`: the flag
/// wasn't given; empty: unscoped). The broker reads them with Deno's own
/// permission checks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrokerNet {
  pub allow: Option<Vec<String>>,
  pub deny: Option<Vec<String>>,
  pub allow_connect: Option<Vec<String>>,
  pub deny_connect: Option<Vec<String>>,
  pub allow_listen: Option<Vec<String>>,
  pub deny_listen: Option<Vec<String>>,
}

impl EnforcePlan {
  /// The plan without the lists the first image measured on the file
  /// system (module files, expanded entries): what two images compare.
  pub fn without_measured_lists(&self) -> Self {
    Self {
      module_files: Vec::new(),
      expanded: Vec::new(),
      expanded_write: Vec::new(),
      entries: Vec::new(),
      member_configs: Vec::new(),
      ..self.clone()
    }
  }

  /// Checks the expanded entries a hand-over carries against the plan's
  /// own expansion roots: each strictly beneath a root, absolute with no
  /// `..` part, neither a denied path nor beneath or
  /// above one, at most [`MAX_EXPANDED`] of them. Anything else could widen
  /// the read scope past what the policy says, so it is an error.
  pub fn check_expanded(&self) -> Result<(), String> {
    let linux = self.linux.as_ref();
    Self::check_entries(
      &self.expanded,
      linux.map(|l| &l.expand[..]).unwrap_or(&[]),
    )?;
    Self::check_entries(
      &self.expanded_write,
      linux.map(|l| &l.expand_write[..]).unwrap_or(&[]),
    )
  }

  fn check_entries(
    expanded: &[PathBuf],
    roots: &[ExpandRoot],
  ) -> Result<(), String> {
    use std::path::Component;
    if expanded.len() > MAX_EXPANDED {
      return Err(format!(
        "{} expanded entries, more than {MAX_EXPANDED}",
        expanded.len()
      ));
    }
    for path in expanded {
      let plain = path.is_absolute()
        && path
          .components()
          .all(|c| matches!(c, Component::RootDir | Component::Normal(_)));
      let ok = plain
        && roots.iter().any(|r| {
          path != &r.root
            && path.starts_with(&r.root)
            && !r
              .deny
              .iter()
              .any(|d| path.starts_with(d) || d.starts_with(path))
        });
      if !ok {
        return Err(format!(
          "{} is not an entry of an expanded folder",
          path.display()
        ));
      }
    }
    Ok(())
  }

  /// The hand-over's bytes. An error, never a panic, when the plan names a
  /// path that isn't UTF-8 (JSON can't hold it): the caller refuses to
  /// re-execute.
  pub fn to_bytes(&self) -> std::io::Result<Vec<u8>> {
    serde_json::to_vec(self).map_err(|err| {
      std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("the sandbox's plan can't be handed over: {err}"),
      )
    })
  }

  /// Parses a hand-over; anything malformed is an error (the caller refuses
  /// to start).
  pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
    if bytes.len() > MAX_PLAN_BYTES {
      return Err(format!(
        "the plan is {} bytes, more than {MAX_PLAN_BYTES}",
        bytes.len()
      ));
    }
    let plan: Self = serde_json::from_slice(bytes)
      .map_err(|err| format!("the plan does not parse: {err}"))?;
    if plan.format != PLAN_FORMAT {
      return Err(format!(
        "the plan has format {}, this runtime reads {PLAN_FORMAT}",
        plan.format
      ));
    }
    Ok(plan)
  }

  /// Whether the variable `name` stays in a scrubbed environment.
  pub fn keeps_env(&self, name: &str) -> bool {
    match &self.env_keep {
      None => true,
      Some(keep) => keep.iter().any(|k| match k.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => k == name,
      }),
    }
  }
}

/// What a Windows process applies to itself (the small tier): a job object
/// and, for a scoped write grant, a Low integrity level. Reads and the
/// network are not in it (Deno-only on Windows).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WindowsPlan {
  /// No `--allow-run`: the job's active-process limit is 1.
  pub no_children: bool,
  /// The folders and files the process may write, labelled Low before the
  /// process lowers its own integrity level. `None`: writes aren't scoped
  /// (the level stays as it is).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub write_roots: Option<Vec<PathBuf>>,
}

/// What Landlock handles: a handled class with no rule for a path is
/// refused there.
#[derive(
  Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Handled {
  pub read: bool,
  pub write: bool,
  /// `execve` of files (no `--allow-run`: handled with no rules).
  pub exec: bool,
}

/// One Landlock path-beneath rule.
#[derive(
  Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FsRule {
  pub path: PathBuf,
  pub read: bool,
  pub write: bool,
  /// The file may be executed (a strict exec scope's programs).
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub exec: bool,
}

/// TCP ports for one direction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Ports {
  Any,
  /// These ports only (empty: none).
  List {
    ports: Vec<u16>,
  },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum NetPlan {
  /// No IP sockets.
  Denied,
  /// IP sockets unrestricted.
  Open,
  /// TCP only, by port (Landlock ABI 4+ / Seatbelt).
  Ports { connect: Ports, bind: Ports },
}

/// The seccomp program's switches (Linux).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SeccompPlan {
  /// The attack-surface list (bpf, perf_event_open, userfaultfd, keyrings,
  /// ptrace, process_vm_*, kcmp, the mount family, kexec, modules, setns,
  /// namespace flags of clone / unshare).
  pub attack_surface: bool,
  /// `clone3` answers ENOSYS (its flags are behind a pointer; glibc falls
  /// back to `clone`, whose flags are filtered).
  pub clone3_enosys: bool,
  pub refuse_exec: bool,
  pub refuse_listen: bool,
  pub refuse_io_uring: bool,
  /// The path forms of chmod / chown / utime / xattr writes.
  pub refuse_path_metadata: bool,
  /// Truncation Landlock doesn't hold before ABI 3: `truncate(2)` by path,
  /// `O_RDONLY | O_TRUNC` opens, and `openat2` (its flags are behind a
  /// pointer: ENOSYS, callers fall back to `openat`).
  #[serde(default)]
  pub refuse_path_truncate: bool,
  /// Signals reach this process only (`kill`, `tgkill` and the queued
  /// forms with another target refused; `tkill` and `pidfd_send_signal`
  /// too): where Landlock's signal scope is missing and nothing may start a
  /// child.
  #[serde(default)]
  pub signal_self: bool,
  /// `connect(2)` refused outright: under a broker that holds the
  /// network, the process tree makes no connection of its own (its sockets
  /// arrive connected), and a socket the broker handed over must not be
  /// pointed at another address (`connect` to `AF_UNSPEC` dissolves a TCP
  /// association, and Landlock holds the next connect only from ABI 4).
  #[serde(default)]
  pub refuse_connect: bool,
  /// `socket(AF_UNIX)` allowed (`socketpair` always is).
  pub unix_sockets: bool,
  /// `socket(AF_NETLINK, *, NETLINK_ROUTE)` allowed (other protocols never).
  pub netlink_route: bool,
  pub vsock: bool,
}

/// A read allow held by expansion (a read deny inside it): the
/// folder gets no rule of its own; its entries do, but not the way to a
/// denied path.
#[derive(
  Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExpandRoot {
  /// Canonical.
  pub root: PathBuf,
  /// Canonical; each beneath `root`.
  pub deny: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinuxPlan {
  pub handled: Handled,
  pub rules: Vec<FsRule>,
  /// The read expansions; their entries are [`EnforcePlan::expanded`].
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub expand: Vec<ExpandRoot>,
  /// The write expansions (a write deny inside a write allow, under the
  /// broker): the folder and the folders on the way to the deny get no
  /// write rule, so the kernel refuses changes directly in them (the
  /// broker makes those for Deno's own operations); every other entry gets
  /// one. Their entries are [`EnforcePlan::expanded_write`].
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub expand_write: Vec<ExpandRoot>,
  pub net: NetPlan,
  /// TCP listeners to bind (and listen on) before the plan applies: the
  /// addresses the grant names for listening. The operations that listen
  /// there are handed these (`deno_net_route`), so the confined process
  /// binds nothing itself.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub prebound: Vec<std::net::SocketAddr>,
  /// Landlock ABI 6 scopes.
  pub scope_signal: bool,
  pub scope_abstract_unix: bool,
  pub seccomp: SeccompPlan,
}

/// A Seatbelt path filter, already validated as UTF-8 and quoted when the
/// profile is written.
#[derive(
  Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum SbFilter {
  /// The path and everything beneath it.
  Subpath(String),
  /// Exactly this path.
  Literal(String),
  /// A regular expression (SBPL `regex #"..."`), for the case-folded
  /// spelling of a path that doesn't exist yet on a case-insensitive volume.
  Regex(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum SbScope {
  All,
  Filters { filters: Vec<SbFilter> },
}

impl SbScope {
  pub fn is_empty(&self) -> bool {
    matches!(self, Self::Filters { filters } if filters.is_empty())
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum SbUnix {
  Denied,
  Filters { filters: Vec<SbFilter> },
  Open,
}

/// The cli Seatbelt profile's inputs (macOS).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MacosPlan {
  pub read: SbScope,
  pub write: SbScope,
  pub deny_read: Vec<SbFilter>,
  pub deny_write: Vec<SbFilter>,
  /// Allowed again after the denies (implicit entries inside a deny; a gap
  /// says so).
  pub reallow_read: Vec<SbFilter>,
  pub reallow_write: Vec<SbFilter>,
  /// Where executable code may be mapped from (dyld, dlopen).
  pub map_exec: SbScope,
  pub exec: bool,
  /// With `exec`: only these files may be executed (canonical; a strict
  /// exec scope). Empty: any.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub exec_only: Vec<String>,
  pub net: NetPlan,
  pub unix: SbUnix,
  /// sysctl names refused (a trailing `*`: a prefix).
  pub denied_sysctls: Vec<String>,
  /// The system trust store is used (`DENO_TLS_CA_STORE=system`):
  /// `trustd` / `SecurityServer` are reachable.
  pub system_ca: bool,
  /// The terminals readable and writable (never `ioctl`-able): `/dev/tty`
  /// when the parent has one, and the standard streams' own terminals;
  /// every other terminal is refused.
  pub ttys: Vec<String>,
  /// A deny ceiling (D1 / D2): the profile allows everything by default and
  /// refuses only the denies ([`crate::seatbelt::deny_only_profile`]); the
  /// scopes above are then unscoped and unused.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub deny_only: bool,
}

// The plan expansion only applies on Unix (the Linux enforcer), so its tests are Unix-only.
#[cfg(all(test, unix))]
mod tests {
  use super::*;

  fn plan(expand: Vec<ExpandRoot>, expanded: &[&str]) -> EnforcePlan {
    EnforcePlan {
      format: PLAN_FORMAT,
      linux: Some(LinuxPlan {
        handled: Handled {
          read: true,
          write: false,
          exec: true,
        },
        rules: Vec::new(),
        expand,
        expand_write: Vec::new(),
        net: NetPlan::Denied,
        prebound: Vec::new(),
        scope_signal: false,
        scope_abstract_unix: false,
        seccomp: SeccompPlan::default(),
      }),
      macos: None,
      windows: None,
      env_keep: None,
      probe: None,
      module_files: Vec::new(),
      expanded: expanded.iter().map(PathBuf::from).collect(),
      config_env: Vec::new(),
      cache_overlay: None,
      broker: None,
      expanded_write: Vec::new(),
      entries: Vec::new(),
      member_configs: Vec::new(),
    }
  }

  // a module path that isn't UTF-8 can't be written into the JSON
  // hand-over: an error for the caller to report, never a panic
  #[test]
  fn a_non_utf8_path_is_an_error_not_a_panic() {
    use std::os::unix::ffi::OsStrExt;
    let mut bad = plan(Vec::new(), &[]);
    bad
      .module_files
      .push(PathBuf::from(std::ffi::OsStr::from_bytes(
        b"/w/bad-\xff/m.cjs",
      )));
    let err = bad.to_bytes().unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(plan(Vec::new(), &[]).to_bytes().is_ok());
  }

  // a hand-over's expanded entries can only be entries of the expanded
  // folders it compares, never a denied path, its folder, or anything else
  // (Unix only: the entries are the Linux enforcer's, on POSIX paths, and a
  // drive-less /p isn't absolute on Windows)
  #[cfg(unix)]
  #[test]
  fn expanded_entries_are_checked_against_their_roots() {
    let roots = vec![ExpandRoot {
      root: "/p".into(),
      deny: vec!["/p/a/.env".into()],
    }];
    assert!(
      plan(roots.clone(), &["/p/b", "/p/a/main.ts"])
        .check_expanded()
        .is_ok()
    );
    for bad in [
      "/p",          // the folder itself (it holds the deny)
      "/p/a",        // the way to the deny
      "/p/a/.env",   // the deny
      "/p/a/.env/x", // beneath it
      "/etc/passwd", // elsewhere
      "/p/../etc",   // a dot-dot
      "p/b",         // relative
    ] {
      assert!(
        plan(roots.clone(), &[bad]).check_expanded().is_err(),
        "{bad}"
      );
    }
    // no roots: no entry is acceptable
    assert!(plan(vec![], &["/p/b"]).check_expanded().is_err());
    // the measured lists are not part of what two images compare
    let a = plan(roots.clone(), &["/p/b"]);
    let b = plan(roots, &[]);
    assert_eq!(a.without_measured_lists(), b.without_measured_lists());
    assert_ne!(a, b);
  }

  // the write expansion's entries are held to their own roots the same way,
  // and the broker's policy is part of what two images compare
  #[cfg(unix)]
  #[test]
  fn write_expansion_and_broker_are_checked_and_compared() {
    let root = ExpandRoot {
      root: PathBuf::from("/out"),
      deny: vec![PathBuf::from("/out/keep")],
    };
    let mut p = plan(Vec::new(), &[]);
    p.linux.as_mut().unwrap().expand_write = vec![root];
    p.expanded_write = vec![PathBuf::from("/out/sub"), PathBuf::from("/out/f")];
    p.check_expanded().unwrap();
    for bad in [
      "/out",
      "/out/keep",
      "/out/keep/x",
      "/elsewhere",
      "/out/../x",
    ] {
      let mut wrong = p.clone();
      wrong.expanded_write.push(PathBuf::from(bad));
      assert!(wrong.check_expanded().is_err(), "{bad}");
    }
    // a read root doesn't vouch for a write entry
    let mut crossed = plan(
      vec![ExpandRoot {
        root: PathBuf::from("/r"),
        deny: vec![PathBuf::from("/r/d")],
      }],
      &[],
    );
    crossed.expanded_write = vec![PathBuf::from("/r/x")];
    assert!(crossed.check_expanded().is_err());
    // measured lists aside, the broker's policy decides equality
    let mut with_broker = p.clone();
    with_broker.broker = Some(BrokerPlan {
      read: None,
      deny_read: Vec::new(),
      write: Some(vec![PathBuf::from("/out")]),
      deny_write: vec![PathBuf::from("/out/keep")],
      net: None,
    });
    assert_ne!(
      with_broker.without_measured_lists(),
      p.without_measured_lists()
    );
    let mut other_entries = with_broker.clone();
    other_entries.expanded_write.clear();
    assert_eq!(
      with_broker.without_measured_lists(),
      other_entries.without_measured_lists()
    );
    let bytes = with_broker.to_bytes().unwrap();
    assert_eq!(EnforcePlan::from_bytes(&bytes).unwrap(), with_broker);
  }
}
