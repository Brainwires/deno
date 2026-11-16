// Copyright 2018-2026 the Deno authors. MIT license.

//! Confined npm lifecycle scripts (`jail-scripts`, DENEXT.md item 83).
//!
//! An approved dependency's lifecycle script normally runs with the user's
//! whole environment, their real HOME, and a `node` that is `deno run -A`. A
//! jailed script instead runs in its own process (`deno
//! __deno_internal_jailed_lifecycle_script`, the script read from stdin) with:
//!
//! - an allow-listed environment (no tokens: `npm_config_*` keys naming
//!   credentials are dropped too), and HOME, TMPDIR, DENO_DIR (and on Windows
//!   USERPROFILE / APPDATA / LOCALAPPDATA) pointing at a scratch directory
//!   under `node_modules/.deno/.jail/`;
//! - the OS confining the whole process tree, whatever the script starts
//!   (`sh`, `curl`, a compiler, `deno run -A`): writes only beneath
//!   node_modules (the scratch directory, its TMPDIR, is inside) and the
//!   package's own `write` paths (not the shared system temp directory: a
//!   project, or another program's files, can live there); no reading of
//!   credential files (`~/.ssh`, `~/.aws`, `~/.npmrc`, the project's
//!   `.npmrc` / `.env*` / `.dev.vars` / `.netrc` / `.pypirc` /
//!   `.git-credentials` at any depth, ...); no network unless the config
//!   allows it; no Unix sockets. Landlock + seccomp on Linux
//!   (`sandbox_linux`, the seccomp rules from [`seccomp_spec`]), Seatbelt on
//!   macOS (`sandbox_macos`), both in the `deno_sandbox` crate. Since item 111 a
//!   script writes only beneath its own package's folder (not the rest of
//!   node_modules), and one that may reach only some hosts reaches them
//!   through an allow-listing proxy (`jail_proxy`), the only address the OS
//!   lets it connect to. The download host a package declares for itself
//!   (node-pre-gyp's `binary.host`) is a request, not a grant: it is
//!   honoured only when the project's `jailScripts.knownPackages` lists it
//!   for that package, and then never when it is (or resolves to) an
//!   internal address (DENEXT.md item 124);
//! - its own session and process group (`setsid`): no controlling terminal
//!   to type into (`TIOCSTI`), and when the script ends the whole group is
//!   killed, so nothing it started outlives it (Linux also adopts and ends
//!   orphaned descendants, as the installer is a child subreaper there);
//! - every `deno` started inside it clamping its permissions to the same set
//!   (`DENO_INTERNAL_LIFECYCLE_JAIL`): `-A` from the `node` shim and the bin
//!   wrappers becomes write access to those roots only, FFI (native addons)
//!   only from the package's own folder (none where the OS can't confine),
//!   network per the jail;
//!   package-manager subcommands (`deno install`, `x`, `add`, ...) are
//!   refused, as are `npm` / `npx` falling back to a real npm.
//!
//! Where the OS can't confine (Windows, Linux without Landlock, macOS without
//! a working `sandbox-exec`), only the environment, HOME and the Deno
//! permission clamp apply; a warning on every install says so (and `deno
//! info --json` reports it as `lifecycleJail`), and `paranoid` refuses to
//! run the script instead.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::SystemTime;

use deno_config::deno_json::JailNetConfig;
use deno_config::deno_json::JailPackageConfig;
use deno_config::deno_json::is_home_relative_path;
use deno_core::anyhow::Context;
use deno_core::anyhow::bail;
use deno_core::error::AnyError;
use deno_core::serde_json;
use deno_runtime::deno_permissions::PermissionsOptions;
use deno_task_shell::KillSignal;
use serde::Deserialize;
use serde::Serialize;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

use crate::args::DenoSubcommand;
use crate::args::Flags;
use crate::colors;
use crate::task_runner::TaskResult;
use crate::util::fs::canonicalize_path;

/// Set in a jailed script's environment: the permissions every `deno` it
/// starts clamps itself to (JSON [`JailPermissions`]).
pub const JAIL_ENV_VAR: &str = "DENO_INTERNAL_LIFECYCLE_JAIL";
/// argv\[1\] of the jailed script's process.
pub const CHILD_ARG: &str = "__deno_internal_jailed_lifecycle_script";

/// Network access of a jailed script.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JailNet {
  #[default]
  None,
  All,
  /// The hosts the package declares; the OS can't tell hosts apart, so it
  /// allows the network, and Deno only these hosts.
  Hosts(Vec<String>),
}

impl JailNet {
  fn allowed(&self) -> bool {
    !matches!(self, JailNet::None)
  }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JailPermissions {
  pub write: Vec<PathBuf>,
  pub deny_write: Vec<PathBuf>,
  pub deny_read: Vec<PathBuf>,
  pub net: JailNet,
  /// The script's allow-listing proxy (`127.0.0.1:<port>`) when `net` is a
  /// host list: a `deno` may connect to it besides the hosts (DENEXT.md item
  /// 111; Node's `http` with `NODE_USE_ENV_PROXY` connects there itself).
  #[serde(default)]
  pub proxy: Option<String>,
  /// Where a `deno` may load native code (FFI, Node-API addons) from: the
  /// package's own folder, so its install check can load the prebuilt
  /// `.node` it ships (`node-gyp-build-test`), as it will at run time.
  /// Nothing else: a script could already run any program it can write
  /// there, under the same OS sandbox, so this adds no reach. Empty where
  /// the OS can't confine (no native code at all).
  #[serde(default)]
  pub ffi: Vec<PathBuf>,
  /// An OS sandbox (Landlock, Seatbelt) confines the script's whole process
  /// tree. The clamp then lets a `deno` create symlinks whose link and
  /// target both lie within `write` (DENEXT.md item 129: electron's
  /// postinstall unpacks an app bundle full of them); never set where the OS
  /// can't confine (Windows), where symlinks keep needing unscoped grants.
  #[serde(default)]
  pub os_confined: bool,
}

/// The roots a jailed `deno` may create symlinks within (DENEXT.md item
/// 129): the jail's write roots, when an OS sandbox confines it.
pub fn jail_symlink_roots(jail: &JailPermissions) -> Option<&[PathBuf]> {
  (jail.os_confined && !cfg!(windows) && !jail.write.is_empty())
    .then_some(jail.write.as_slice())
}

/// What a jailed script with network may read under the user's real home
/// directory (DENEXT.md item 437).
pub use deno_sandbox::HomeReadPolicy;
/// What the OS sandbox lets a jailed script's processes connect to.
pub use deno_sandbox::OsNet;

/// The seccomp program of a jailed script's processes (DENEXT.md items 111
/// and 124): `io_uring_setup` fails (io_uring opens sockets and does I/O
/// past the syscall filter), `ioctl(TIOCSTI)` / `ioctl(TIOCLINUX)` fail with
/// EPERM (they type into a terminal), `listen` fails with EACCES unless the
/// network is open (an unbound socket's `listen` binds an ephemeral port on
/// every address itself, which Landlock's `BIND_TCP` doesn't mediate;
/// GHSA-9363-h8qp-jg52), through the proxy the TCP Fast Open
/// sends fail (`MSG_FASTOPEN` connects a socket without `connect(2)`, past
/// Landlock), and `socket(2)` creates netlink sockets, plus IPv4 / IPv6
/// ones when the network is open (TCP streams only through the proxy).
/// Netlink is `NETLINK_ROUTE` only (`getifaddrs`); the other families list
/// every socket on the host (`NETLINK_SOCK_DIAG`) or reach kernel code a
/// script has no use for. `socketpair(2)` makes stream and seqpacket pairs
/// but no datagram pair, whose sockets could `sendto` any datagram socket on
/// the host. `ptrace`, `process_vm_readv` / `process_vm_writev` and
/// `pidfd_getfd` fail with EPERM: Landlock already confines them to the
/// script's own processes, and a script has no use for them. Naming another
/// process as a descriptor's SIGIO owner (`fcntl` `F_SETOWN` /
/// `F_SETOWN_EX`, `ioctl` `FIOSETOWN` / `SIOCSPGRP`) fails with EPERM, as
/// do the inode-flag `ioctl`s (`FS_IOC_SETFLAGS`, `FS_IOC_FSSETXATTR`),
/// which change a file's attributes through a read-only descriptor. The
/// signals themselves, and the other metadata changes, go to the jail's
/// supervisor (`deno_sandbox::notify`).
#[cfg(target_os = "linux")]
pub fn seccomp_spec(net: OsNet) -> deno_sandbox::SeccompSpec {
  seccomp_spec_for(net, u32::MAX)
}

/// [`seccomp_spec`] for a kernel with Landlock ABI `landlock_abi`: before
/// ABI 3 (Linux 6.2) Landlock can't refuse truncation, and `open(2)` with
/// `O_TRUNC` and a read-only access mode truncates whatever the user may
/// write, so it fails with EPERM there, and `openat2(2)`, whose flags the
/// filter can't see, with ENOSYS (libc falls back to `openat`). `truncate`
/// goes to the supervisor there.
#[cfg(target_os = "linux")]
pub fn seccomp_spec_for(
  net: OsNet,
  landlock_abi: u32,
) -> deno_sandbox::SeccompSpec {
  use deno_sandbox::seccomp::AF_NETLINK;
  use deno_sandbox::seccomp::ArgTest;
  use deno_sandbox::seccomp::InetSockets;
  use deno_sandbox::seccomp::MSG_FASTOPEN;
  use deno_sandbox::seccomp::NR_IO_URING_SETUP;
  use deno_sandbox::seccomp::NR_IOCTL;
  use deno_sandbox::seccomp::NR_LISTEN;
  use deno_sandbox::seccomp::NR_SENDMMSG;
  use deno_sandbox::seccomp::NR_SENDMSG;
  use deno_sandbox::seccomp::NR_SENDTO;
  use deno_sandbox::seccomp::NR_SOCKET;
  use deno_sandbox::seccomp::SOCK_TYPE_MASK;
  use deno_sandbox::seccomp::SyscallRule;
  use deno_sandbox::seccomp::TIOCLINUX;
  use deno_sandbox::seccomp::TIOCSTI;

  let mut spec = deno_sandbox::SeccompSpec::new()
    .rule(SyscallRule::Deny {
      nr: NR_IO_URING_SETUP,
      errno: libc::ENOSYS,
    })
    // the ioctl request is argument 1
    .rule(SyscallRule::DenyArgIn {
      nr: NR_IOCTL,
      arg: 1,
      values: vec![TIOCSTI, TIOCLINUX],
      errno: libc::EPERM,
    });
  if !matches!(net, OsNet::Open) {
    // no serving: Landlock refuses an explicit bind, but listen(2) on an
    // unbound socket auto-binds 0.0.0.0:<ephemeral> past it, reachable from
    // the host and the network wherever the script has no namespace of its
    // own (GHSA-9363-h8qp-jg52). The proxy's listener in a namespace is
    // bound and listening before this filter is installed (NetnsHandoff
    // runs first).
    spec = spec.rule(SyscallRule::Deny {
      nr: NR_LISTEN,
      errno: libc::EACCES,
    });
  }
  if matches!(net, OsNet::ProxyOnly(_)) {
    // the flags: argument 3 of sendto / sendmmsg, 2 of sendmsg
    for (nr, arg) in [(NR_SENDTO, 3), (NR_SENDMMSG, 3), (NR_SENDMSG, 2)] {
      spec = spec.rule(SyscallRule::DenyArgBits {
        nr,
        arg,
        mask: MSG_FASTOPEN,
        errno: libc::EOPNOTSUPP,
      });
    }
  }
  // netlink: NETLINK_ROUTE (0) only
  spec = spec.rule(SyscallRule::DenyArgsAll {
    nr: NR_SOCKET,
    tests: vec![
      ArgTest {
        arg: 0,
        mask: u32::MAX,
        value: AF_NETLINK,
        equal: true,
      },
      ArgTest {
        arg: 2,
        mask: u32::MAX,
        value: libc::NETLINK_ROUTE as u32,
        equal: false,
      },
    ],
    errno: libc::EACCES,
  });
  // no datagram socket pair (the type's low bits, past SOCK_CLOEXEC /
  // SOCK_NONBLOCK)
  spec = spec.rule(SyscallRule::DenyArgsAll {
    nr: libc::SYS_socketpair as u32,
    tests: vec![ArgTest {
      arg: 1,
      mask: SOCK_TYPE_MASK,
      value: libc::SOCK_DGRAM as u32,
      equal: true,
    }],
    errno: libc::EACCES,
  });
  for nr in [
    libc::SYS_ptrace,
    libc::SYS_process_vm_readv,
    libc::SYS_process_vm_writev,
    libc::SYS_pidfd_getfd,
  ] {
    spec = spec.rule(SyscallRule::Deny {
      nr: nr as u32,
      errno: libc::EPERM,
    });
  }
  // a descriptor's SIGIO owner: another process to signal
  spec = spec
    .rule(SyscallRule::DenyArgIn {
      nr: libc::SYS_fcntl as u32,
      arg: 1,
      values: vec![libc::F_SETOWN as u32, F_SETOWN_EX],
      errno: libc::EPERM,
    })
    .rule(SyscallRule::DenyArgIn {
      nr: NR_IOCTL,
      arg: 1,
      values: vec![
        FIOSETOWN,
        SIOCSPGRP,
        FS_IOC_SETFLAGS,
        FS_IOC32_SETFLAGS,
        FS_IOC_FSSETXATTR,
      ],
      errno: libc::EPERM,
    });
  if landlock_abi < 3 {
    let read_only_trunc =
      |nr: libc::c_long, flags_arg: u32| SyscallRule::DenyArgsAll {
        nr: nr as u32,
        tests: vec![
          ArgTest {
            arg: flags_arg,
            mask: libc::O_ACCMODE as u32,
            value: libc::O_RDONLY as u32,
            equal: true,
          },
          ArgTest {
            arg: flags_arg,
            mask: libc::O_TRUNC as u32,
            value: libc::O_TRUNC as u32,
            equal: true,
          },
        ],
        errno: libc::EPERM,
      };
    #[cfg(target_arch = "x86_64")]
    {
      spec = spec.rule(read_only_trunc(libc::SYS_open, 1));
    }
    spec =
      spec
        .rule(read_only_trunc(libc::SYS_openat, 2))
        .rule(SyscallRule::Deny {
          nr: NR_OPENAT2,
          errno: libc::ENOSYS,
        });
  }
  spec.inet_sockets(match net {
    OsNet::Denied => InetSockets::Denied,
    OsNet::Open => InetSockets::Any,
    OsNet::ProxyOnly(_) => InetSockets::TcpStream,
  })
}

/// `fcntl`'s `F_SETOWN_EX` (not in every libc crate).
#[cfg(target_os = "linux")]
const F_SETOWN_EX: u32 = 15;
/// `ioctl` requests that name a SIGIO owner, or set inode flags.
#[cfg(target_os = "linux")]
const FIOSETOWN: u32 = 0x8901;
#[cfg(target_os = "linux")]
const SIOCSPGRP: u32 = 0x8902;
#[cfg(target_os = "linux")]
const FS_IOC_SETFLAGS: u32 = 0x4008_6602;
#[cfg(target_os = "linux")]
const FS_IOC32_SETFLAGS: u32 = 0x4004_6602;
#[cfg(target_os = "linux")]
const FS_IOC_FSSETXATTR: u32 = 0x401c_6620;
/// The same on every architecture.
#[cfg(target_os = "linux")]
const NR_OPENAT2: u32 = 437;

/// The jail of the current process, when it runs inside a jailed script.
pub fn jail_from_env() -> Option<JailPermissions> {
  static JAIL: OnceLock<Option<JailPermissions>> = OnceLock::new();
  JAIL
    .get_or_init(|| {
      #[allow(clippy::disallowed_methods, reason = "process-wide setting")]
      let value = std::env::var(JAIL_ENV_VAR).ok()?;
      // every process this one starts gets the jail as read here, whatever
      // environment its caller passes (a `node:child_process` spawn with
      // `env: {}`, or a value set later through `Deno.env`)
      deno_runtime::deno_process::pin_child_env_var(
        JAIL_ENV_VAR.into(),
        value.clone().into(),
      );
      match serde_json::from_str(&value) {
        Ok(jail) => Some(jail),
        // unreadable: the narrowest jail rather than none
        Err(_) => Some(JailPermissions::default()),
      }
    })
    .clone()
}

/// Inside a jailed script, refuses the subcommands that install or fetch
/// packages, or write outside the permission system.
pub fn check_subcommand(flags: &Flags) -> Result<(), AnyError> {
  if jail_from_env().is_none() {
    return Ok(());
  }
  let allowed = matches!(
    flags.subcommand,
    DenoSubcommand::Run(_)
      | DenoSubcommand::Eval(_)
      | DenoSubcommand::Task(_)
      | DenoSubcommand::Repl(_)
      | DenoSubcommand::Test(_)
      | DenoSubcommand::Bench(_)
      | DenoSubcommand::Check(_)
      | DenoSubcommand::Info(_)
      | DenoSubcommand::Types
      | DenoSubcommand::Help(_)
      | DenoSubcommand::Completions(_)
      | DenoSubcommand::Serve(_)
  );
  if !allowed {
    bail!(
      "This deno subcommand is not allowed in a jailed npm lifecycle script (jail-scripts)."
    );
  }
  Ok(())
}

fn path_within(path: &Path, roots: &[PathBuf]) -> bool {
  roots.iter().any(|root| path.starts_with(root))
}

/// Where `path` really is: its longest existing prefix canonicalized (a
/// symlink in it, and a `..` after one, resolved as the OS resolves them),
/// and the rest appended. A path that doesn't exist yet beneath a symlink
/// (`/tmp/new` on macOS, where `/tmp` leads to `/private/tmp`) then resolves
/// to the real location the OS sandbox and Deno's own checks see, as an
/// existing one does. A `..` in the part that doesn't exist is folded and
/// the result resolved again (what it lands on may exist, or be a link), so
/// `../../etc/x` can't pass as beneath a root.
fn resolve_existing_prefix(path: &Path) -> PathBuf {
  fn resolve(path: &Path, again: bool) -> PathBuf {
    let mut prefix = path;
    let mut rest = Vec::new();
    let real = loop {
      if let Ok(real) = canonicalize_path(prefix) {
        break real;
      }
      match prefix.components().next_back() {
        Some(last) if prefix.parent().is_some() => {
          rest.push(last.as_os_str().to_os_string());
          prefix = prefix.parent().unwrap();
        }
        // nothing of it exists, not even a root: as written
        _ => {
          return deno_path_util::normalize_path(std::borrow::Cow::Borrowed(
            path,
          ))
          .into_owned();
        }
      }
    };
    let joined = rest.iter().rev().fold(real, |acc, part| acc.join(part));
    if again && rest.iter().any(|part| part == "..") {
      let folded =
        deno_path_util::normalize_path(std::borrow::Cow::Borrowed(&joined))
          .into_owned();
      // the folded path has no `..` left: one more pass settles it
      return resolve(&folded, false);
    }
    deno_path_util::normalize_path(std::borrow::Cow::Borrowed(&joined))
      .into_owned()
  }
  resolve(path, true)
}

/// Keeps the entries of a permission path list that lie within `roots`:
/// each resolved against `cwd` and through its existing prefix
/// ([`resolve_existing_prefix`]).
fn clamp_path_list(
  list: Vec<String>,
  roots: &[PathBuf],
  cwd: &Path,
) -> Vec<String> {
  list
    .into_iter()
    .filter(|entry| {
      path_within(&resolve_existing_prefix(&cwd.join(entry)), roots)
    })
    .collect()
}

/// Clamps a `deno` process's permissions to the jail.
pub fn clamp_permissions(
  options: &mut PermissionsOptions,
  jail: &JailPermissions,
  cwd: &Path,
) {
  let roots_text = || {
    jail
      .write
      .iter()
      .map(|p| p.to_string_lossy().into_owned())
      .collect::<Vec<_>>()
  };
  options.allow_write = match options.allow_write.take() {
    None => None,
    Some(list) if list.is_empty() => Some(roots_text()),
    Some(list) => {
      let kept = clamp_path_list(list, &jail.write, cwd);
      // an empty list would mean "everything"
      (!kept.is_empty()).then_some(kept)
    }
  };
  let with_proxy = |mut list: Vec<String>| {
    if let Some(proxy) = &jail.proxy
      && !list.contains(proxy)
    {
      list.push(proxy.clone());
    }
    list
  };
  let clamp_net = |list: Option<Vec<String>>| match (&jail.net, list) {
    (_, None) | (JailNet::None, _) => None,
    (JailNet::All, list) => list,
    (JailNet::Hosts(hosts), Some(list)) if list.is_empty() => {
      Some(with_proxy(hosts.clone()))
    }
    (JailNet::Hosts(hosts), Some(list)) => {
      let kept = list
        .into_iter()
        .filter(|h| hosts.contains(h))
        .collect::<Vec<_>>();
      (!kept.is_empty()).then(|| with_proxy(kept))
    }
  };
  options.allow_net = clamp_net(options.allow_net.take());
  // The direction-specific grants are clamped the same way, so a `deno`
  // started with `--allow-net-connect` can't reach past the jail either.
  options.allow_net_connect = clamp_net(options.allow_net_connect.take());
  options.allow_net_listen = clamp_net(options.allow_net_listen.take());
  if !jail.net.allowed() {
    options.allow_import = None;
    options.deny_import = Some(Vec::new());
  }
  // denext: a lifecycle script never needs MIDI devices
  options.allow_midi = None;
  // FFI only from the package's own folder (`-A` / `--allow-ffi` become
  // that; a path list keeps the entries inside it), never more
  options.allow_ffi = match options.allow_ffi.take() {
    None => None,
    Some(_) if jail.ffi.is_empty() => None,
    Some(list) if list.is_empty() => Some(
      jail
        .ffi
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect(),
    ),
    Some(list) => {
      let kept = clamp_path_list(list, &jail.ffi, cwd);
      // an empty list would mean "everything"
      (!kept.is_empty()).then_some(kept)
    }
  };
  for (deny, extra) in [
    (&mut options.deny_write, &jail.deny_write),
    (&mut options.deny_read, &jail.deny_read),
  ] {
    if extra.is_empty() {
      continue;
    }
    match deny {
      // denied entirely already
      Some(list) if list.is_empty() => {}
      Some(list) => {
        list.extend(extra.iter().map(|p| p.to_string_lossy().into_owned()))
      }
      None => {
        *deny = Some(
          extra
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
        )
      }
    }
  }
  options.prompt = false;
  if let Some(roots) = jail_symlink_roots(jail) {
    deno_runtime::deno_permissions::set_jail_symlink_roots(roots.to_vec());
  }
}

/// The jail of an install (`jail-scripts`).
#[derive(Debug)]
pub struct JailConfig {
  pub paranoid: bool,
  /// `supplyChain.jailScripts.net`
  pub net: bool,
  pub packages: BTreeMap<String, JailPackageConfig>,
  /// `supplyChain.jailScripts.knownPackages`: per package, the download
  /// hosts it may declare for itself (DENEXT.md item 124).
  pub known_packages: BTreeMap<String, Vec<String>>,
  /// `supplyChain.jailScripts.upstreamByName`: the installer's upstream
  /// proxy is asked for a checked host by name, not by address.
  pub upstream_by_name: bool,
  /// `supplyChain` lives in package.json's `"deno"` object (a project with
  /// no deno.json, DENEXT.md item 125): what the warnings tell users to edit.
  pub settings_in_package_json: bool,
  /// The project directories (the workspace root and the initial cwd) and
  /// the `.npmrc` the installer resolved: where the secret files are looked
  /// for, the first time a script is jailed.
  project_dirs: Vec<PathBuf>,
  npmrc_path: Option<PathBuf>,
  /// The installer's DENO_DIR: a script with network may still read it
  /// (DENEXT.md item 437).
  deno_dir: Option<PathBuf>,
  project_secret_files: OnceLock<Vec<PathBuf>>,
  outside_links: OnceLock<Vec<PathBuf>>,
  warned: std::sync::atomic::AtomicBool,
  warned_net: std::sync::atomic::AtomicBool,
  /// Linux: the missing-network-namespace warning was printed.
  #[cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "read on the Linux namespace path")
  )]
  warned_netns: std::sync::atomic::AtomicBool,
  /// The packages whose refused declared hosts were reported.
  warned_declared: std::sync::Mutex<std::collections::BTreeSet<String>>,
  /// Node's headers for a native build from source, handed to the script
  /// as `npm_config_nodedir` (DENEXT.md item 288).
  node_headers: Option<Arc<super::node_headers::NodeHeaders>>,
}

/// How deep below a project directory secret files are looked for.
const SECRET_SCAN_DEPTH: usize = 12;
/// How many directories the scan reads at most, over every project
/// directory: a bound on the work an install does, not a security limit a
/// script could lean on (a tree that large is reported, below).
const SECRET_SCAN_MAX_DIRS: usize = 20_000;

/// Whether a file name is one a project keeps secrets in: `.env`, `.env.*`,
/// `.envrc`, `.dev.vars`, `.npmrc`, `.netrc`, `.pypirc`, `.git-credentials`.
fn is_secret_file_name(name: &str) -> bool {
  name.starts_with(".env")
    || matches!(
      name,
      ".dev.vars" | ".npmrc" | ".netrc" | ".pypirc" | ".git-credentials"
    )
}

/// Whether a project directory is one to look into only one level deep: a
/// filesystem root or the user's home directory (a project that resolved to
/// one of those isn't a project tree, and walking it would read the disk).
fn shallow_project_dir(dir: &Path) -> bool {
  dir.parent().is_none()
    || home_dir().is_some_and(|home| {
      canonicalize_path(&home).unwrap_or(home)
        == canonicalize_path(dir).unwrap_or_else(|_| dir.to_path_buf())
    })
}

/// The secret files under `dirs` (workspace members included), to a bounded
/// depth and a bounded number of directories, not entering `node_modules`
/// or `.git` and not following symlinks. Returns the files and whether the
/// bound cut the scan short.
fn project_secret_files(dirs: &[PathBuf]) -> (Vec<PathBuf>, bool) {
  fn visit(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    budget: &mut usize,
    out: &mut Vec<PathBuf>,
  ) {
    if *budget == 0 {
      return;
    }
    *budget -= 1;
    let Ok(entries) = std::fs::read_dir(dir) else {
      return;
    };
    for entry in entries.flatten() {
      let Ok(file_type) = entry.file_type() else {
        continue;
      };
      let name = entry.file_name();
      let name = name.to_string_lossy();
      if file_type.is_dir() {
        if depth < max_depth
          && name != "node_modules"
          && name != ".git"
          && name != ".jail"
        {
          visit(&entry.path(), depth + 1, max_depth, budget, out);
        }
      } else if !file_type.is_symlink() && is_secret_file_name(&name) {
        out.push(entry.path());
      }
    }
  }
  let mut out = Vec::new();
  let mut budget = SECRET_SCAN_MAX_DIRS;
  for dir in dirs {
    let max_depth = if shallow_project_dir(dir) {
      0
    } else {
      SECRET_SCAN_DEPTH
    };
    visit(dir, 0, max_depth, &mut budget, &mut out);
  }
  (out, budget == 0)
}

impl JailConfig {
  pub fn new(
    paranoid: bool,
    net: bool,
    packages: BTreeMap<String, JailPackageConfig>,
    project_dirs: &[PathBuf],
    npmrc_path: Option<&Path>,
  ) -> Self {
    Self {
      paranoid,
      net,
      packages,
      known_packages: BTreeMap::new(),
      upstream_by_name: false,
      settings_in_package_json: false,
      project_dirs: project_dirs.to_vec(),
      npmrc_path: npmrc_path.map(Path::to_path_buf),
      deno_dir: None,
      project_secret_files: Default::default(),
      outside_links: Default::default(),
      warned: Default::default(),
      warned_net: Default::default(),
      warned_netns: Default::default(),
      warned_declared: Default::default(),
      node_headers: None,
    }
  }

  /// Sets where Node's headers for node-gyp come from.
  pub fn with_node_headers(
    mut self,
    node_headers: Option<Arc<super::node_headers::NodeHeaders>>,
  ) -> Self {
    self.node_headers = node_headers;
    self
  }

  /// The project's own files with secrets (`.npmrc`, `.env*`, `.dev.vars`,
  /// `.netrc`, `.pypirc`, `.git-credentials`) at any depth, canonical,
  /// found the first time they are needed (the scan is work every install
  /// shouldn't repeat, and `deno` runs that never jail a script shouldn't
  /// do at all).
  pub fn project_secret_files(&self) -> &[PathBuf] {
    self.project_secret_files.get_or_init(|| {
      let (mut files, cut_short) = project_secret_files(&self.project_dirs);
      if cut_short {
        log::warn!(
          "{} jail-scripts: the project has more than {SECRET_SCAN_MAX_DIRS} directories; secret files (.env*, .npmrc, ...) deeper in it than the scan reached are not hidden from lifecycle scripts.",
          colors::yellow("Warning")
        );
      }
      if let Some(path) = &self.npmrc_path {
        files.push(path.clone());
      }
      let mut files = files
        .into_iter()
        .filter_map(|p| canonicalize_path(&p).ok())
        .collect::<Vec<_>>();
      files.sort();
      files.dedup();
      files
    })
  }

  /// Sets `upstream_by_name`.
  pub fn with_upstream_by_name(mut self, upstream_by_name: bool) -> Self {
    self.upstream_by_name = upstream_by_name;
    self
  }

  /// Sets the installer's DENO_DIR (`deno_dir`).
  pub fn with_deno_dir(mut self, deno_dir: PathBuf) -> Self {
    self.deno_dir = Some(deno_dir);
    self
  }

  /// Sets `known_packages`.
  pub fn with_known_packages(
    mut self,
    known_packages: BTreeMap<String, Vec<String>>,
  ) -> Self {
    self.known_packages = known_packages;
    self
  }

  /// Sets `settings_in_package_json`.
  pub fn with_settings_in_package_json(mut self, value: bool) -> Self {
    self.settings_in_package_json = value;
    self
  }

  /// The `supplyChain` setting `inner` (JSON text of its value) as the
  /// project writes it, with the file to add it to.
  fn supply_chain_line(&self, inner: &str) -> String {
    if self.settings_in_package_json {
      format!("add to package.json: \"deno\": {{ \"supplyChain\": {inner} }}")
    } else {
      format!("add to deno.json: \"supplyChain\": {inner}")
    }
  }

  /// The file that holds the project's `supplyChain`, for messages.
  fn settings_file(&self) -> &'static str {
    if self.settings_in_package_json {
      "package.json"
    } else {
      "deno.json"
    }
  }

  /// What to do about a jailed script of a native package that failed
  /// (DENEXT.md item 288): the package's own fix, or `None` for a package
  /// that builds and downloads nothing native.
  pub fn native_failure_hint(
    &self,
    package_name: &str,
    package_folder: &Path,
    script: &str,
  ) -> Option<String> {
    let package_json =
      std::fs::read_to_string(package_folder.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
    let depends_on = |dep: &str| {
      package_json.as_ref().is_some_and(|json| {
        ["dependencies", "optionalDependencies"]
          .iter()
          .any(|key| json.get(key).and_then(|d| d.get(dep)).is_some())
      })
    };
    let builtin = super::builtin_exceptions::builtin_exception(package_name);
    if depends_on("nan") {
      return Some(format!(
        "npm:{package_name} is a NAN addon: it is built against Node's V8 C++ API, which this runtime doesn't provide (only Node-API), so it can't load here even when it builds. Use a Node-API package instead, or leave its install script unapproved if nothing loads it."
      ));
    }
    let native = builtin.is_some()
      || super::node_headers::needs_node_gyp(package_folder, script);
    if !native {
      return None;
    }
    let mut hint = String::new();
    if self.paranoid {
      hint.push_str(&format!(
        "paranoid is on, so npm:{package_name}'s script gets no network and no built-in exception. "
      ));
    }
    match builtin {
      Some(builtin) => {
        let hosts = builtin.host_list();
        hint.push_str(&format!(
          "npm:{package_name}'s script may reach {} (the built-in exception) to download its prebuilt binary",
          hosts.join(", ")
        ));
        if builtin.node_gyp {
          hint.push_str(", and otherwise builds it from source with node-gyp, which needs Python, make and a C++ compiler (Node's headers are provided)");
        }
        hint.push_str(&format!(
          ". Behind a proxy or a mirror, add its host to the package's list: {}",
          self.supply_chain_line(&format!(
            "{{ \"jailScripts\": {{ \"packages\": {{ \"{package_name}\": {{ \"net\": [{}, \"<mirror host>\"] }} }} }} }}",
            hosts
              .iter()
              .map(|h| format!("\"{h}\""))
              .collect::<Vec<_>>()
              .join(", ")
          ))
        ));
        if package_name == "better-sqlite3" {
          hint.push_str(". Leaving its install script unapproved works too: this runtime serves better-sqlite3 over its built-in SQLite");
        }
        hint.push('.');
      }
      None => {
        hint.push_str(&format!(
          "npm:{package_name} builds a native addon. A build from source needs Python, make and a C++ compiler (Node's headers are provided); if its script downloads a prebuilt binary, allow the host it downloads from: {}.",
          self.supply_chain_line(&format!(
            "{{ \"jailScripts\": {{ \"packages\": {{ \"{package_name}\": {{ \"net\": [\"<download host>\"] }} }} }} }}"
          ))
        ));
      }
    }
    Some(hint)
  }
}

/// Whether the OS sandbox can hold a script's connections to one loopback
/// port (its proxy): Seatbelt can; Linux in a network namespace of its own
/// (the proxy the only thing on its loopback), or Landlock from ABI 4
/// (Linux 6.7) by port.
fn can_confine_to_proxy() -> bool {
  #[cfg(target_os = "linux")]
  {
    super::sandbox_linux::netns_available().is_ok()
      || super::sandbox_linux::can_confine_to_port()
  }
  #[cfg(target_os = "macos")]
  {
    true
  }
  #[cfg(not(any(target_os = "linux", target_os = "macos")))]
  {
    false
  }
}

/// What the OS confines a lifecycle script here, for `deno info --json`
/// (`lifecycleJail`) and denext's doctor (DENEXT.md item 124).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Confinement {
  /// `"seatbelt"`, `"landlock"` or `"none"`.
  pub os_sandbox: &'static str,
  /// Why there is none.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub reason: Option<String>,
  /// Whether every program a script starts is held to its host list:
  /// `"proxy"` (only its allow-listing proxy is reachable), or
  /// `"unconfined"` (the proxy and the Deno clamp apply, other programs
  /// reach any host; a package's own hosts then get no network).
  pub host_lists: &'static str,
  /// Linux: whether a script with a host list runs in its own network
  /// namespace (nothing but the proxy on its loopback, no route elsewhere).
  #[serde(skip_serializing_if = "Option::is_none")]
  pub network_namespace: Option<bool>,
  /// The warnings an install prints here, one line each.
  pub warnings: Vec<String>,
}

/// The confinement this machine gives lifecycle scripts.
pub fn confinement() -> Confinement {
  let sandbox = os_sandbox();
  let mut warnings = Vec::new();
  let os_sandbox = match &sandbox {
    Ok(()) if cfg!(target_os = "macos") => "seatbelt",
    Ok(()) => "landlock",
    Err(reason) => {
      warnings.push(unconfined_warning(reason));
      "none"
    }
  };
  let host_lists = if sandbox.is_ok() && can_confine_to_proxy() {
    "proxy"
  } else {
    "unconfined"
  };
  #[cfg(target_os = "linux")]
  let network_namespace = {
    let netns = super::sandbox_linux::netns_available();
    if sandbox.is_ok()
      && let Err(reason) = &netns
    {
      warnings.push(no_netns_warning(reason));
    }
    if sandbox.is_ok()
      && let Err(reason) = super::sandbox_linux::private_shm_available()
    {
      warnings.push(no_shm_warning(&reason));
    }
    Some(netns.is_ok())
  };
  #[cfg(not(target_os = "linux"))]
  let network_namespace = None;
  Confinement {
    os_sandbox,
    reason: sandbox.err(),
    host_lists,
    network_namespace,
    warnings,
  }
}

/// The one-line warning for a platform the OS can't confine on.
fn unconfined_warning(reason: &str) -> String {
  format!(
    "jail-scripts: the OS can't confine lifecycle scripts here ({reason}). They still get a scrubbed environment, a temporary HOME and a clamped Deno, but any other program they start (a shell, a compiler, curl) can read and write anything and reach any host."
  )
}

/// One `/dev/shm` warning per process.
#[cfg(target_os = "linux")]
static NO_SHM_WARNED: std::sync::atomic::AtomicBool =
  std::sync::atomic::AtomicBool::new(false);

/// The one-line warning for Linux where a script can't get a private
/// `/dev/shm`: it gets none (the host's holds every same-user process's
/// shared memory and semaphores).
#[cfg(target_os = "linux")]
fn no_shm_warning(reason: &str) -> String {
  format!(
    "jail-scripts: lifecycle scripts can't get a private /dev/shm here ({reason}), so they get no /dev/shm at all: POSIX shared memory and named semaphores (shm_open, sem_open, Python's multiprocessing locks) fail in them."
  )
}

/// Whether a script's output names POSIX shared memory or a named
/// semaphore, so its failure may come from having no `/dev/shm`.
#[cfg(target_os = "linux")]
fn mentions_shared_memory(output: &[u8]) -> bool {
  const MARKERS: &[&[u8]] = &[
    b"/dev/shm",
    b"shm_open",
    b"sem_open",
    b"SemLock",
    b"semaphore",
  ];
  MARKERS
    .iter()
    .any(|m| output.windows(m.len()).any(|w| w == *m))
}

/// The warning a failing script gets on Linux where it had no `/dev/shm`
/// and its output names shared memory or a semaphore.
#[cfg(target_os = "linux")]
fn no_shm_hint(reason: &str) -> String {
  format!(
    "jail-scripts: this host gives lifecycle scripts no /dev/shm ({reason}); a script that uses POSIX shared memory or named semaphores (shm_open, sem_open, Python's multiprocessing locks) fails without it."
  )
}

/// The one-line warning for Linux without an unprivileged network namespace.
#[cfg(target_os = "linux")]
fn no_netns_warning(reason: &str) -> String {
  format!(
    "jail-scripts: lifecycle scripts can't get a network namespace of their own here ({reason}); a script held to a host list shares this host's network: it may connect only to its proxy's port number, which another machine could answer on, and may not listen for connections."
  )
}

/// What the OS sandbox allows a script's processes to connect to
/// (DENEXT.md item 111). A host list goes through the proxy where the OS can
/// hold every program to it. Where it can't (Linux before 6.7), hosts the
/// package declares for itself, and any list under `paranoid`, get no
/// network (fail closed: the package granted itself that, or the project
/// asked for every gate to fail closed); a list the project's deno.json
/// grants keeps the network open, with a warning, as before item 111.
fn choose_os_net(
  net: &JailNet,
  declared: bool,
  paranoid: bool,
  proxy_port: u16,
  can_confine_to_proxy: bool,
) -> OsNet {
  match net {
    JailNet::None => OsNet::Denied,
    JailNet::All => OsNet::Open,
    JailNet::Hosts(_) if can_confine_to_proxy => OsNet::ProxyOnly(proxy_port),
    JailNet::Hosts(_) if declared || paranoid => OsNet::Denied,
    JailNet::Hosts(_) => OsNet::Open,
  }
}

/// What the OS can confine here, or why it can't.
fn os_sandbox() -> Result<(), String> {
  #[cfg(target_os = "linux")]
  {
    linux_sandbox(
      super::sandbox_linux::landlock_abi(),
      !seccomp_spec(OsNet::Denied).program().is_empty(),
    )
  }
  #[cfg(target_os = "macos")]
  {
    super::sandbox_macos::available()
  }
  #[cfg(not(any(target_os = "linux", target_os = "macos")))]
  {
    Err(format!(
      "{} has no OS sandbox Deno can apply to lifecycle scripts",
      std::env::consts::OS
    ))
  }
}

/// The Linux sandbox is Landlock and the jail's seccomp program together:
/// on an architecture the seccomp assembler doesn't know the program is
/// empty, and Landlock alone would leave Unix sockets and `io_uring` open,
/// so there is no OS sandbox there (the script is refused under `paranoid`,
/// and warned about otherwise) rather than half of one.
#[cfg(target_os = "linux")]
fn linux_sandbox(
  landlock: Result<u32, String>,
  seccomp: bool,
) -> Result<(), String> {
  landlock?;
  if !seccomp {
    return Err(format!(
      "the jail's seccomp filter isn't built for {}",
      std::env::consts::ARCH
    ));
  }
  Ok(())
}

/// The write roots and read denies as a Seatbelt profile can name them: its
/// paths are text, so a path that isn't UTF-8 has no exact spelling there. A
/// write root that isn't is left out (an allow that can't be written grants
/// nothing); a deny that isn't is replaced by its nearest ancestor that is
/// (a deny that can't be written must not deny nothing).
#[cfg_attr(
  not(target_os = "macos"),
  allow(dead_code, reason = "the Seatbelt profile is macOS only")
)]
fn seatbelt_paths(
  write: &[PathBuf],
  deny_read: &[PathBuf],
) -> (Vec<PathBuf>, Vec<PathBuf>) {
  let write = write
    .iter()
    .filter(|p| p.to_str().is_some())
    .cloned()
    .collect();
  let mut deny = Vec::with_capacity(deny_read.len());
  for path in deny_read {
    let named = path
      .ancestors()
      .find(|p| p.to_str().is_some())
      .unwrap_or(Path::new("/"))
      .to_path_buf();
    if !deny.contains(&named) {
      deny.push(named);
    }
  }
  (write, deny)
}

/// The syscalls the jail's supervisor answers on a kernel with Landlock ABI
/// `abi`: signals and metadata changes always, `truncate` before ABI 3.
#[cfg(target_os = "linux")]
pub(crate) fn supervised_syscalls(abi: u32) -> Vec<u32> {
  use deno_sandbox::notify;
  let mut syscalls = notify::signal_syscalls();
  syscalls.extend(notify::metadata_syscalls());
  if abi < 3 {
    syscalls.push(notify::nr::TRUNCATE);
  }
  syscalls
}

/// The paths the Linux OS layer refuses to read: the jail's list plus `/dev`,
/// whose few devices a script uses Landlock allows by name. Deno's own clamp keeps the jail's list: its `/dev/null` and
/// `/dev/stdin` stay readable to a jailed `deno`.
#[cfg(target_os = "linux")]
pub(crate) fn linux_os_deny_read(deny_read: &[PathBuf]) -> Vec<PathBuf> {
  let mut paths = deny_read.to_vec();
  paths.push(PathBuf::from("/dev"));
  paths
}

/// Credential files under the user's real home directories
/// ([`real_homes`]).
fn credential_paths() -> Vec<PathBuf> {
  let mut paths = Vec::new();
  for home in real_homes() {
    for path in CREDENTIAL_PATHS_IN_HOME
      .iter()
      .filter_map(|p| canonicalize_path(&home.join(p)).ok())
    {
      if !paths.contains(&path) {
        paths.push(path);
      }
    }
  }
  paths
}

/// The credential files and directories [`credential_paths`] hides, relative
/// to the home directory.
const CREDENTIAL_PATHS_IN_HOME: &[&str] = &[
  ".ssh",
  ".aws",
  ".azure",
  ".gnupg",
  ".kube",
  ".docker",
  ".npmrc",
  ".yarnrc",
  ".yarnrc.yml",
  ".netrc",
  ".git-credentials",
  ".pgpass",
  ".pypirc",
  ".gem/credentials",
  ".cargo/credentials",
  ".cargo/credentials.toml",
  ".config/gcloud",
  ".config/gh",
  ".config/hub",
  ".config/op",
  ".config/git/credentials",
  ".local/share/keyrings",
  ".password-store",
  ".terraform.d/credentials.tfrc.json",
  ".deno/auth",
  "Library/Keychains",
  "Library/Application Support/gh",
  // deploy and hosting CLIs' tokens (DENEXT.md item 437)
  ".fly",
  ".config/configstore",
  ".vercel",
  ".supabase",
  ".appstoreconnect",
  "private_keys",
  // shell histories (commands typed with secrets in them)
  ".zsh_history",
  ".bash_history",
  ".local/share/fish/fish_history",
  // browser profiles (cookies, saved logins)
  ".mozilla",
  "Library/Application Support/Firefox",
  "Library/Application Support/Google/Chrome",
  ".config/google-chrome",
  ".config/chromium",
];

/// Toolchain directories in the home directory a networked script may still
/// read, when they exist: compilers and version managers a build script
/// runs (their credential files are on the credential list).
const TOOLCHAIN_HOMES: &[&str] = &[
  ".rustup",
  // not all of `.cargo`: its `config.toml` can hold registry tokens
  ".cargo/bin",
  ".cargo/registry",
  ".cargo/git",
  ".nvm",
  ".volta",
  ".fnm",
  ".asdf",
  ".pyenv",
  ".rbenv",
  ".bun",
  ".deno/bin",
  ".local/share/mise",
  ".local/share/fnm",
  ".local/share/pnpm",
  "Library/pnpm",
];

/// The paths under the home directories a networked script may read: its
/// writable roots, the project and its node_modules, the package, the parts
/// of the installer's DENO_DIR it needs and the running deno, the toolchain
/// homes ([`TOOLCHAIN_HOMES`]), and the toolchains on its PATH: each PATH
/// directory in a home directory, and the prefix above a `bin` or `shims`
/// one deeper in it (`~/.nvm/versions/node/<v>`), or just its `lib` for one
/// directly in a home directory (`~/.npm-global/lib` for
/// `~/.npm-global/bin`, whose commands link there; not all of `~/.local` for
/// `~/.local/bin`). Each candidate is resolved (links followed) before it is
/// checked; one that is a home directory or holds one is dropped (a project
/// at `~` itself would make the whole home directory readable).
fn home_read_allowed(
  homes: &[PathBuf],
  roots: impl IntoIterator<Item = PathBuf>,
  path_var: Option<&std::ffi::OsStr>,
) -> Vec<PathBuf> {
  let canonical =
    |p: &Path| canonicalize_path(p).unwrap_or_else(|_| p.to_path_buf());
  let mut candidates: Vec<PathBuf> = roots.into_iter().collect();
  for home in homes {
    candidates.extend(
      TOOLCHAIN_HOMES
        .iter()
        .map(|dir| home.join(dir))
        .filter(|dir| dir.exists()),
    );
  }
  if let Some(path_var) = path_var {
    for dir in std::env::split_paths(path_var) {
      if !dir.is_absolute() {
        continue;
      }
      let dir = canonical(&dir);
      if dir
        .file_name()
        .is_some_and(|name| name == "bin" || name == "shims")
        && let Some(prefix) = dir.parent()
      {
        if prefix
          .parent()
          .is_some_and(|p| homes.iter().any(|h| h == p))
        {
          let lib = prefix.join("lib");
          if lib.is_dir() {
            candidates.push(lib);
          }
        } else {
          candidates.push(prefix.to_path_buf());
        }
      }
      candidates.push(dir);
    }
  }
  let mut allowed = Vec::new();
  for path in candidates {
    let path = canonical(&path);
    if homes.iter().any(|home| path.starts_with(home))
      && !homes.iter().any(|home| home.starts_with(&path))
      && !allowed.contains(&path)
    {
      allowed.push(path);
    }
  }
  allowed.sort();
  allowed
}

/// The files a tool looks for in each directory above the one it runs in
/// (config discovery, version-manager pins). In a home directory or between
/// it and a project they stay readable, so such a tool finds them, or goes
/// on as if they were missing, rather than failing on a refused read.
const DISCOVERY_FILES: &[&str] = &[
  "package.json",
  "deno.json",
  "deno.jsonc",
  "tsconfig.json",
  "jsconfig.json",
  "bunfig.toml",
  "pnpm-workspace.yaml",
  "lerna.json",
  "nx.json",
  "turbo.json",
  ".nvmrc",
  ".node-version",
  ".tool-versions",
  ".python-version",
  ".ruby-version",
];

/// The [`DISCOVERY_FILES`] in each directory from above `dirs` up to the
/// home directory that holds them, the home directory included. Only
/// regular files: a link there could lead anywhere in the home directory.
fn discovery_files(homes: &[PathBuf], dirs: &[PathBuf]) -> Vec<PathBuf> {
  let mut out = Vec::new();
  for dir in dirs {
    let Some(home) = homes
      .iter()
      .filter(|home| dir.starts_with(home))
      .max_by_key(|home| home.components().count())
    else {
      continue;
    };
    for ancestor in dir.ancestors().skip(1) {
      if !ancestor.starts_with(home) {
        break;
      }
      for name in DISCOVERY_FILES {
        let file = ancestor.join(name);
        if std::fs::symlink_metadata(&file).is_ok_and(|m| m.is_file())
          && !out.contains(&file)
        {
          out.push(file);
        }
      }
    }
  }
  out
}

/// Where the package managers keep globally installed packages and their
/// commands, as their own configuration says (the installer's environment,
/// `~/.npmrc`, pnpm's `rc`, `~/.yarnrc`, `~/.bunfig.toml`), and the user
/// site of Python (which node-gyp runs). A command on the PATH links into
/// these: a script that runs one needs to read them.
fn package_manager_dirs(
  homes: &[PathBuf],
  path_var: Option<&std::ffi::OsStr>,
  env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> Vec<PathBuf> {
  let env_path = |name: &str| {
    env(name)
      .or_else(|| env(&name.to_ascii_lowercase()))
      .filter(|v| !v.is_empty())
      .map(PathBuf::from)
  };
  // npm's global packages are in `<prefix>/lib/node_modules` (Unix) or
  // `<prefix>/node_modules` (Windows), their commands in `<prefix>/bin`
  fn npm_prefix(prefix: PathBuf, out: &mut Vec<PathBuf>) {
    out.push(prefix.join("lib"));
    out.push(prefix.join("bin"));
    if cfg!(windows) {
      out.push(prefix.join("node_modules"));
    }
  }
  let mut out = Vec::new();
  if let Some(prefix) = env_path("NPM_CONFIG_PREFIX") {
    npm_prefix(prefix, &mut out);
  }
  for name in ["NPM_CONFIG_GLOBAL_DIR", "NPM_CONFIG_GLOBAL_BIN_DIR"] {
    out.extend(env_path(name));
  }
  for name in ["PNPM_HOME", "BUN_INSTALL", "BUN_INSTALL_GLOBAL_DIR"] {
    out.extend(env_path(name));
  }
  out.extend(env_path("BUN_INSTALL_BIN"));
  out.extend(env_path("YARN_GLOBAL_FOLDER"));
  if let Some(prefix) = env_path("YARN_PREFIX") {
    out.push(prefix.join("bin"));
  }
  // npm's default prefix: the directory above the `node` it runs with (each
  // `node` on the PATH: a shim earlier on it may stand in for the first)
  if let Some(path_var) = path_var {
    #[allow(clippy::disallowed_methods, reason = "this executable")]
    let exe = std::env::current_exe()
      .ok()
      .and_then(|p| canonicalize_path(&p).ok());
    for dir in std::env::split_paths(path_var) {
      let node = dir.join(if cfg!(windows) { "node.exe" } else { "node" });
      let Ok(node) = canonicalize_path(&node) else {
        continue;
      };
      // the jail's own `node` is this deno
      if Some(&node) == exe.as_ref() {
        continue;
      }
      if let Some(prefix) = dir.parent() {
        npm_prefix(prefix.to_path_buf(), &mut out);
      }
    }
  }
  let xdg_config = env_path("XDG_CONFIG_HOME");
  for home in homes {
    let expand = |value: &str| expand_config_path(value, home, env);
    let npmrc =
      env_path("NPM_CONFIG_USERCONFIG").unwrap_or_else(|| home.join(".npmrc"));
    let pnpm_rcs = [
      xdg_config
        .clone()
        .unwrap_or_else(|| home.join(".config"))
        .join("pnpm/rc"),
      home.join("Library/Preferences/pnpm/rc"),
    ];
    for (file, ini) in std::iter::once((npmrc, true))
      .chain(pnpm_rcs.into_iter().map(|rc| (rc, true)))
      .chain([(home.join(".yarnrc"), false)])
    {
      let Ok(text) = std::fs::read_to_string(&file) else {
        continue;
      };
      for (key, value) in config_entries(&text, ini) {
        let Some(path) = expand(&value) else {
          continue;
        };
        match key.as_str() {
          "prefix" => npm_prefix(path, &mut out),
          "global-dir" | "global-bin-dir" | "global-folder" => out.push(path),
          _ => {}
        }
      }
    }
    for bunfig in [
      xdg_config.clone().map(|dir| dir.join(".bunfig.toml")),
      Some(home.join(".bunfig.toml")),
    ]
    .into_iter()
    .flatten()
    {
      let Ok(text) = std::fs::read_to_string(&bunfig) else {
        continue;
      };
      let mut section = String::new();
      for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[') {
          section = name.trim_end_matches(']').trim().to_string();
          continue;
        }
        if section != "install" {
          continue;
        }
        if let Some((key, value)) = line.split_once('=')
          && matches!(key.trim(), "globalDir" | "globalBinDir")
          && let Some(path) = expand(unquote(value.trim()))
        {
          out.push(path);
        }
      }
    }
    // yarn 1's default global folder: `$XDG_DATA_HOME/yarn/global`, else
    // `~/.config/yarn/global`
    out.push(
      env_path("XDG_DATA_HOME")
        .map(|dir| dir.join("yarn"))
        .unwrap_or_else(|| home.join(".config/yarn"))
        .join("global"),
    );
    // Python's user site: `<user base>/lib/python<v>` (`~/.local` by
    // default), and on macOS `~/Library/Python/<v>`
    let user_base = env_path("PYTHONUSERBASE").unwrap_or_else(|| {
      if cfg!(target_os = "macos") {
        home.join("Library/Python")
      } else {
        home.join(".local")
      }
    });
    if cfg!(target_os = "macos") && env_path("PYTHONUSERBASE").is_none() {
      out.push(user_base);
    } else if let Ok(entries) = std::fs::read_dir(user_base.join("lib")) {
      out.extend(
        entries
          .flatten()
          .filter(|e| e.file_name().to_string_lossy().starts_with("python"))
          .map(|e| e.path()),
      );
    }
  }
  out.retain(|path| path.is_absolute() && path.exists());
  out
}

/// The `key = value` entries of an npm-style ini file (`ini`), or of a yarn
/// 1 `.yarnrc` (`key "value"` or `key value`), comments and sections
/// skipped.
fn config_entries(text: &str, ini: bool) -> Vec<(String, String)> {
  text
    .lines()
    .map(str::trim)
    .filter(|line| {
      !line.is_empty()
        && !line.starts_with('#')
        && !line.starts_with(';')
        && !line.starts_with('[')
    })
    .filter_map(|line| {
      let (key, value) = if ini {
        line.split_once('=')?
      } else {
        line.split_once(char::is_whitespace)?
      };
      Some((key.trim().to_string(), unquote(value.trim()).to_string()))
    })
    .collect()
}

fn unquote(value: &str) -> &str {
  let value = value.trim();
  value
    .strip_prefix('"')
    .and_then(|v| v.strip_suffix('"'))
    .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
    .unwrap_or(value)
}

/// A path from a package manager's configuration: `~/` is `home`, and
/// `${NAME}` (npm's syntax) an environment variable; `None` when a variable
/// is unset or the result isn't absolute.
fn expand_config_path(
  value: &str,
  home: &Path,
  env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> Option<PathBuf> {
  let mut text = String::new();
  let mut rest = value;
  while let Some(start) = rest.find("${") {
    let end = rest[start..].find('}')? + start;
    text.push_str(&rest[..start]);
    let name = &rest[start + 2..end];
    if name == "HOME" {
      text.push_str(home.to_str()?);
    } else {
      text.push_str(env(name)?.to_str()?);
    }
    rest = &rest[end + 1..];
  }
  text.push_str(rest);
  let path = match text.strip_prefix("~/") {
    Some(rest) => home.join(rest),
    None => PathBuf::from(text),
  };
  path.is_absolute().then_some(path)
}

fn home_dir() -> Option<PathBuf> {
  #[allow(clippy::disallowed_methods, reason = "the user's real home")]
  let home = if cfg!(windows) {
    std::env::var_os("USERPROFILE")
  } else {
    std::env::var_os("HOME")
  };
  home.filter(|h| !h.is_empty()).map(PathBuf::from)
}

/// The user's real home directories, for what a script may read: the
/// account's own (from the user database, which no environment variable
/// moves), and `$HOME` (`USERPROFILE` on Windows) when it names another
/// absolute directory. Neither is ever `/`.
fn real_homes() -> Vec<PathBuf> {
  real_homes_from(account_home(), home_dir())
}

fn real_homes_from(
  account: Option<PathBuf>,
  home_var: Option<PathBuf>,
) -> Vec<PathBuf> {
  let mut homes: Vec<PathBuf> = account.into_iter().collect();
  if let Some(home) = home_var
    && home.is_absolute()
    && home.parent().is_some()
    && !homes.contains(&home)
  {
    homes.push(home);
  }
  homes
}

/// The home directory of the account this process runs as, from the user
/// database (`getpwuid_r`).
#[cfg(unix)]
fn account_home() -> Option<PathBuf> {
  use std::os::unix::ffi::OsStrExt;
  // SAFETY: getuid has no failure case
  let uid = unsafe { libc::getuid() };
  let mut buf = vec![0u8; 1024];
  loop {
    // SAFETY: a plain C struct, filled in by getpwuid_r
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: valid pointers to a passwd struct and a buffer of the given
    // length, which outlive the call
    let rc = unsafe {
      libc::getpwuid_r(
        uid,
        &mut pwd,
        buf.as_mut_ptr() as *mut libc::c_char,
        buf.len(),
        &mut result,
      )
    };
    if rc == libc::ERANGE && buf.len() < 1 << 20 {
      buf.resize(buf.len() * 2, 0);
      continue;
    }
    if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
      return None;
    }
    // SAFETY: a NUL-terminated string in `buf`, which is still alive
    let dir = unsafe { std::ffi::CStr::from_ptr(pwd.pw_dir) };
    let dir = PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes()));
    return (dir.is_absolute() && dir.parent().is_some()).then_some(dir);
  }
}

/// No user database here: `USERPROFILE` alone ([`real_homes`]).
#[cfg(not(unix))]
fn account_home() -> Option<PathBuf> {
  None
}

/// Symlinks in node_modules that lead outside the writable roots (workspace
/// members, `file:` dependencies): the Deno permission layer denies writing
/// through them (the OS layer checks real paths anyway).
fn outside_links(root_node_modules: &Path, roots: &[PathBuf]) -> Vec<PathBuf> {
  fn visit(dir: &Path, roots: &[PathBuf], out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
      return;
    };
    for entry in entries.flatten() {
      let path = entry.path();
      let Ok(meta) = std::fs::symlink_metadata(&path) else {
        continue;
      };
      if meta.file_type().is_symlink()
        && let Ok(target) = canonicalize_path(&path)
        && !path_within(&target, roots)
        && !out.contains(&target)
      {
        out.push(target);
      }
    }
  }
  let mut out = Vec::new();
  let mut dirs = vec![root_node_modules.to_path_buf()];
  if let Ok(entries) = std::fs::read_dir(root_node_modules.join(".deno")) {
    for entry in entries.flatten() {
      dirs.push(entry.path().join("node_modules"));
    }
  }
  for dir in dirs {
    visit(&dir, roots, &mut out);
    if let Ok(entries) = std::fs::read_dir(&dir) {
      for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with('@')
          && entry.file_type().is_ok_and(|t| t.is_dir())
        {
          visit(&entry.path(), roots, &mut out);
        }
      }
    }
  }
  out
}

/// The download hosts a package declares: node-pre-gyp's `binary.host`
/// (and `staging_host` / `production_host`).
fn declared_net_hosts(package_folder: &Path) -> Vec<String> {
  let Ok(text) = std::fs::read_to_string(package_folder.join("package.json"))
  else {
    return Vec::new();
  };
  let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
    return Vec::new();
  };
  let mut hosts = Vec::new();
  if let Some(binary) = json.get("binary").and_then(|b| b.as_object()) {
    for key in ["host", "staging_host", "production_host"] {
      if let Some(url) = binary.get(key).and_then(|v| v.as_str())
        && let Ok(url) = deno_core::url::Url::parse(url)
        && let Some(host) = url.host_str()
      {
        let host = match url.port() {
          Some(port) => format!("{host}:{port}"),
          None => host.to_string(),
        };
        if !hosts.contains(&host) {
          hosts.push(host);
        }
      }
    }
  }
  hosts
}

/// The network a package's scripts get.
#[derive(Debug, PartialEq, Eq)]
struct NetChoice {
  net: JailNet,
  /// Whether `net` is the hosts the package declares for itself.
  declared: bool,
  /// Declared hosts the project hasn't allowed (`knownPackages`), which the
  /// script can't reach.
  refused_declared: Vec<String>,
}

/// Whether a declared `host[:port]` is covered by a `knownPackages` entry
/// (`host` covers every port, `host:port` that port), compared as the
/// proxy's allow-list compares hosts.
fn declared_host_known(declared: &str, known: &[String]) -> bool {
  let Ok(declared) = super::jail_proxy::AllowList::parse(std::slice::from_ref(
    &declared.to_string(),
  )) else {
    return false;
  };
  let Some(entry) = declared.entries().first() else {
    return false;
  };
  super::jail_proxy::AllowList::parse(known).is_ok_and(|known| {
    known.entries().iter().any(|k| {
      k.host == entry.host && k.port.is_none_or(|p| Some(p) == entry.port)
    })
  })
}

/// Network access of a package's scripts: the package's own `net` (a host
/// list is the narrower grant and wins over `jailScripts.net`; `false` wins
/// over both `jailScripts.net` and the declared hosts, DENEXT.md item 111),
/// the config's, the built-in exception's hosts (DENEXT.md item 288), or the
/// hosts the package declares for itself, each only when
/// `jailScripts.knownPackages` (or the built-in list of reviewed ones) lists
/// it for the package (DENEXT.md item 124: a declaration is a request, not a
/// grant), and none under paranoid.
fn jail_net(
  config: &JailConfig,
  package: Option<&JailPackageConfig>,
  package_name: &str,
  package_folder: &Path,
) -> NetChoice {
  let plain = |net: JailNet| NetChoice {
    net,
    declared: false,
    refused_declared: Vec::new(),
  };
  match package.and_then(|p| p.net.as_ref()) {
    Some(JailNetConfig::Hosts(hosts)) => {
      return plain(JailNet::Hosts(hosts.clone()));
    }
    Some(JailNetConfig::All(true)) => return plain(JailNet::All),
    Some(JailNetConfig::All(false)) => return plain(JailNet::None),
    None if config.net => return plain(JailNet::All),
    None => {}
  }
  if config.paranoid {
    return plain(JailNet::None);
  }
  // a package known to download at install: its reviewed hosts, whatever it
  // declares for itself
  if let Some(builtin) =
    super::builtin_exceptions::builtin_exception(package_name)
  {
    return plain(JailNet::Hosts(builtin.host_list()));
  }
  let known = config
    .known_packages
    .get(package_name)
    .cloned()
    .unwrap_or_default();
  let builtin_known =
    super::builtin_exceptions::builtin_known_hosts(package_name);
  // a host the project's `knownPackages` covers is granted as declared; one
  // only the built-in list covers, on port 443 alone (a built-in grant)
  let mut allowed = Vec::new();
  let mut refused = Vec::new();
  for host in declared_net_hosts(package_folder) {
    let grant = if declared_host_known(&host, &known) {
      Some(host.clone())
    } else {
      super::builtin_exceptions::builtin_known_grant(&host, builtin_known)
    };
    match grant {
      Some(grant) if !allowed.contains(&grant) => allowed.push(grant),
      Some(_) => {}
      None => refused.push(host),
    }
  }
  // an allowed declared host's redirect targets (GitHub's release assets)
  for host in allowed.clone() {
    for redirect in super::builtin_exceptions::declared_host_redirects(&host) {
      if !allowed.iter().any(|h| h == redirect) {
        allowed.push(redirect.to_string());
      }
    }
  }
  NetChoice {
    net: if allowed.is_empty() {
      JailNet::None
    } else {
      JailNet::Hosts(allowed)
    },
    declared: true,
    refused_declared: refused,
  }
}

/// Strips the credentials (`user:password@`) out of a URL-valued setting,
/// so a proxy or registry URL passed into the jail carries none (DENEXT.md
/// item 124). A value that isn't a URL, or has no credentials, is returned as
/// it is.
fn strip_url_credentials(value: &str) -> String {
  if !value.contains("://") {
    return value.to_string();
  }
  match deno_core::url::Url::parse(value.trim()) {
    Ok(mut url) if !url.username().is_empty() || url.password().is_some() => {
      let _ = url.set_username("");
      let _ = url.set_password(None);
      url.to_string()
    }
    _ => value.to_string(),
  }
}

/// Whether an environment variable's value may be a URL with credentials:
/// npm's config keys and the proxy settings.
fn url_valued_env_var(name: &str) -> bool {
  let lower = name.to_ascii_lowercase();
  lower.starts_with("npm_config_")
    || lower.ends_with("_proxy")
    || lower.starts_with("global_agent_")
}

/// A package's extra `write` paths, `~` resolved against the user's home (a
/// `~` path is dropped when there is no home: never a wider grant), each
/// created when missing. A download cache a package keeps outside
/// node_modules (puppeteer's `PUPPETEER_CACHE_DIR`, playwright's
/// `PLAYWRIGHT_BROWSERS_PATH`, cypress's `CYPRESS_CACHE_FOLDER`) usually
/// doesn't exist before the first install, and the OS sandbox can only grant
/// a directory that does: Landlock opens it, and Seatbelt matches the real
/// path, which `canonicalize` only knows for an existing one.
fn package_write_paths(
  package: &JailPackageConfig,
  home: Option<&Path>,
) -> Vec<PathBuf> {
  package
    .write
    .iter()
    .filter_map(|path| {
      if is_home_relative_path(path) {
        let rest = path.strip_prefix("~").ok()?;
        Some(home?.join(rest))
      } else {
        Some(path.clone())
      }
    })
    .inspect(|path| {
      let _ = std::fs::create_dir_all(path);
    })
    .collect()
}

#[cfg(unix)]
/// The `~` paths among a package's `write` paths, resolved: download caches
/// other projects use, where a new executable file is refused unless the
/// package lists `executables: true` (DENEXT.md item 111).
fn home_write_paths(
  package: &JailPackageConfig,
  home: Option<&Path>,
) -> Vec<PathBuf> {
  let Some(home) = home else {
    return Vec::new();
  };
  package
    .write
    .iter()
    .filter(|path| is_home_relative_path(path))
    .filter_map(|path| Some(home.join(path.strip_prefix("~").ok()?)))
    .collect()
}

/// Removes what a script left in `dirs` since `since` that other programs
/// could run: regular files with an execute (or set-id) bit, and symlinks
/// to such files. Returns the paths removed. Removing rather than clearing
/// the bit also takes out a hard link to a file elsewhere without changing
/// that file. The walk is relative to directory descriptors opened with
/// `O_NOFOLLOW` (`openat` / `fstatat` / `unlinkat`), so a directory swapped
/// for a symlink while it runs can't make it unlink a file elsewhere
/// (DENEXT.md item 124); it runs once the script's process group is gone.
#[cfg(unix)]
fn remove_new_executables(dirs: &[PathBuf], since: SystemTime) -> Vec<PathBuf> {
  use std::ffi::CStr;
  use std::ffi::CString;
  use std::os::fd::AsRawFd;
  use std::os::fd::FromRawFd;
  use std::os::fd::OwnedFd;
  use std::os::unix::ffi::OsStrExt;
  use std::os::unix::ffi::OsStringExt;

  let since = since
    .duration_since(SystemTime::UNIX_EPOCH)
    .map(|d| d.as_secs() as i64)
    .unwrap_or(0);

  fn open_dir(parent: Option<&OwnedFd>, name: &CStr) -> Option<OwnedFd> {
    let dirfd = parent.map(|p| p.as_raw_fd()).unwrap_or(libc::AT_FDCWD);
    // SAFETY: a valid directory fd and C string; the fd returned is owned
    let fd = unsafe {
      libc::openat(
        dirfd,
        name.as_ptr(),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
      )
    };
    if fd < 0 {
      return None;
    }
    // SAFETY: just opened
    Some(unsafe { OwnedFd::from_raw_fd(fd) })
  }

  fn stat_at(dir: &OwnedFd, name: &CStr, follow: bool) -> Option<libc::stat> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    let flags = if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW };
    // SAFETY: a valid directory fd, C string and out-pointer
    let res = unsafe {
      libc::fstatat(dir.as_raw_fd(), name.as_ptr(), st.as_mut_ptr(), flags)
    };
    if res != 0 {
      return None;
    }
    // SAFETY: fstatat filled it
    Some(unsafe { st.assume_init() })
  }

  fn executable(st: &libc::stat) -> bool {
    st.st_mode & libc::S_IFMT == libc::S_IFREG && st.st_mode & 0o7111 != 0
  }

  /// The entry names of an open directory (not `.` / `..`).
  fn entry_names(dir: &OwnedFd) -> Vec<CString> {
    // fdopendir takes the descriptor over, so it gets a duplicate
    // SAFETY: a valid fd
    let dup = unsafe { libc::fcntl(dir.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if dup < 0 {
      return Vec::new();
    }
    // SAFETY: a valid, owned descriptor; closedir below releases it
    let stream = unsafe { libc::fdopendir(dup) };
    if stream.is_null() {
      // SAFETY: the duplicate is still ours
      unsafe { libc::close(dup) };
      return Vec::new();
    }
    let mut names = Vec::new();
    loop {
      // SAFETY: a valid stream; the entry pointer is read before the next call
      let entry = unsafe { libc::readdir(stream) };
      if entry.is_null() {
        break;
      }
      // SAFETY: readdir returned a valid entry with a NUL-terminated name
      let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
      let bytes = name.to_bytes();
      if bytes != b"." && bytes != b".." {
        names.push(name.to_owned());
      }
    }
    // SAFETY: a valid stream, closed once
    unsafe { libc::closedir(stream) };
    names
  }

  fn visit(
    dir: &OwnedFd,
    dir_path: &Path,
    since: i64,
    depth: usize,
    out: &mut Vec<PathBuf>,
  ) {
    for name in entry_names(dir) {
      let path =
        dir_path.join(std::ffi::OsString::from_vec(name.to_bytes().to_vec()));
      let Some(st) = stat_at(dir, &name, false) else {
        continue;
      };
      if st.st_mode & libc::S_IFMT == libc::S_IFDIR {
        if depth < 64
          && let Some(child) = open_dir(Some(dir), &name)
        {
          visit(&child, &path, since, depth + 1, out);
        }
        continue;
      }
      // the inode's change time, which a script can't set back
      if st.st_ctime < since {
        continue;
      }
      let runnable = if st.st_mode & libc::S_IFMT == libc::S_IFLNK {
        stat_at(dir, &name, true).is_some_and(|target| executable(&target))
      } else {
        executable(&st)
      };
      if !runnable {
        continue;
      }
      // SAFETY: a valid directory fd and C string
      if unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) } == 0 {
        out.push(path);
      }
    }
  }

  let mut out = Vec::new();
  for dir in dirs {
    // the root is opened as it really is (a cache on another volume may be
    // a link; what's beneath is walked without following any) and reported
    // as given
    let real = canonicalize_path(dir).unwrap_or_else(|_| dir.clone());
    let Ok(name) = CString::new(real.as_os_str().as_bytes()) else {
      continue;
    };
    if let Some(fd) = open_dir(None, &name) {
      visit(&fd, dir, since, 0, &mut out);
    }
  }
  out
}

/// Points every HTTP client a jailed script may start at its proxy.
fn set_proxy_env(env: &mut HashMap<OsString, OsString>, proxy_url: &str) {
  // the installer's own settings (a NO_PROXY would send a client around the
  // proxy, where the OS refuses it)
  env.retain(|key, _| {
    let lower = key.to_string_lossy().to_ascii_lowercase();
    !matches!(
      lower.as_str(),
      "no_proxy"
        | "npm_config_proxy"
        | "npm_config_https_proxy"
        | "npm_config_noproxy"
        | "npm_config_no_proxy"
    )
  });
  for key in [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    // npm's config keys, which node-pre-gyp, prebuild-install and others read
    "npm_config_proxy",
    "npm_config_https_proxy",
    // global-agent (@electron/get, ...)
    "GLOBAL_AGENT_HTTP_PROXY",
    "GLOBAL_AGENT_HTTPS_PROXY",
  ] {
    let key = if cfg!(windows) {
      key.to_ascii_uppercase()
    } else {
      key.to_string()
    };
    env.insert(key.into(), proxy_url.into());
  }
  // Node's `http` / `https` (Deno's included) honor the variables only then
  env.insert("NODE_USE_ENV_PROXY".into(), "1".into());
}

/// Environment variables a jailed script keeps (Windows names upper-cased).
/// The installer's proxy settings pass only with the whole network: a host
/// list goes through the jail's own proxy.
fn keep_env_var(name: &str, net: bool) -> bool {
  const EXACT: &[&str] = &[
    "PATH",
    "TERM",
    "COLORTERM",
    "NO_COLOR",
    "FORCE_COLOR",
    "LANG",
    "LANGUAGE",
    "TZ",
    "USER",
    "LOGNAME",
    "USERNAME",
    "SHELL",
    "CI",
    "INIT_CWD",
    "PYTHON",
    "CC",
    "CXX",
    "AR",
    "LD",
    "CFLAGS",
    "CXXFLAGS",
    "CPPFLAGS",
    "LDFLAGS",
    "MAKE",
    "MAKEFLAGS",
    "JOBS",
    "SDKROOT",
    "DEVELOPER_DIR",
    "MACOSX_DEPLOYMENT_TARGET",
    "PKG_CONFIG_PATH",
    "NODEJS_ORG_MIRROR",
    "NODE_OPTIONS",
    "DENO_TLS_CA_STORE",
    "DENO_CERT",
    "DENO_NODE_GYP",
    "DENO_NO_PROMPT",
    "DENO_DISABLE_NODE_SHIM",
    "DENO_NODE_SHIM_ACTIVE",
    "DENO_DONT_USE_INTERNAL_NODE_COMPAT_STATE_FD",
    // Windows
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "COMSPEC",
    "PATHEXT",
    "WINDIR",
    "OS",
    "PROCESSOR_ARCHITECTURE",
    "PROCESSOR_IDENTIFIER",
    "NUMBER_OF_PROCESSORS",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMW6432",
    "COMMONPROGRAMFILES",
    "COMMONPROGRAMFILES(X86)",
    "PROGRAMDATA",
    "VSINSTALLDIR",
    "VCINSTALLDIR",
    "VCTOOLSINSTALLDIR",
    "WINDOWSSDKDIR",
    "WINDOWSSDKVERSION",
    "INCLUDE",
    "LIB",
    "LIBPATH",
  ];
  const PROXY: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
  ];
  if EXACT.contains(&name) || (net && PROXY.contains(&name)) {
    return true;
  }
  if name.starts_with("LC_") || name.starts_with("DENO_INTERNAL_") {
    return true;
  }
  let lower = name.to_ascii_lowercase();
  if lower.starts_with("npm_package_") || lower.starts_with("npm_lifecycle_") {
    return true;
  }
  // the package manager's identity (`deno/<version> npm/? ...`), which
  // install scripts read to tell package managers apart; it names no
  // credential despite the `user` in its name
  if lower == "npm_config_user_agent" {
    return true;
  }
  if lower.starts_with("npm_config_") {
    // `npm_config_//registry/:_authToken` and friends carry credentials
    const SECRET: &[&str] = &[
      "auth", "token", "pass", "secret", "cert", "key", "email", "user", "otp",
      "//",
    ];
    return !SECRET.iter().any(|s| lower.contains(s));
  }
  false
}

struct JailDirs {
  root: PathBuf,
  home: PathBuf,
  tmp: PathBuf,
  deno_dir: PathBuf,
}

impl JailDirs {
  fn create(
    root_node_modules: &Path,
    package_id: &str,
  ) -> std::io::Result<Self> {
    let name = package_id
      .chars()
      .map(|c| {
        if c.is_ascii_alphanumeric() || "@.-_".contains(c) {
          c
        } else {
          '+'
        }
      })
      .collect::<String>();
    let root = root_node_modules.join(".deno").join(".jail").join(name);
    let dirs = Self {
      home: root.join("home"),
      tmp: root.join("tmp"),
      deno_dir: root.join("deno_dir"),
      root,
    };
    for dir in [&dirs.home, &dirs.tmp, &dirs.deno_dir] {
      std::fs::create_dir_all(dir)?;
    }
    Ok(dirs)
  }
}

/// One lifecycle script to run jailed.
pub struct JailedScript<'a> {
  pub package_name: &'a str,
  pub package_id: String,
  pub package_folder: &'a Path,
  pub root_node_modules: &'a Path,
  pub task_name: &'a str,
  pub script: &'a str,
  pub init_cwd: &'a Path,
  /// The environment the executor built for the script.
  pub env_vars: &'a HashMap<OsString, OsString>,
  /// The bins the script can run by name.
  pub bins: Vec<(String, PathBuf)>,
  pub node_modules_bin_dirs: Vec<PathBuf>,
  pub kill_signal: KillSignal,
}

/// The jailed script's process reads this from stdin.
#[derive(Serialize, Deserialize)]
struct ChildSpec {
  task_name: String,
  script: String,
  cwd: PathBuf,
  init_cwd: PathBuf,
  env: Vec<(String, String)>,
  bins: Vec<(String, PathBuf)>,
  node_modules_bin_dirs: Vec<PathBuf>,
}

/// Runs one lifecycle script jailed, with its output captured.
pub async fn run_jailed_script(
  config: &JailConfig,
  s: JailedScript<'_>,
) -> Result<TaskResult, AnyError> {
  let sandbox = os_sandbox();
  if let Err(reason) = &sandbox {
    if config.paranoid {
      bail!(
        "paranoid is on, but npm:{}'s '{}' script can't be confined here: {reason}. It was not run.",
        s.package_id,
        s.task_name
      );
    }
    // where the OS can't confine, the environment, HOME and the Deno clamp
    // still apply; one warning per install says what doesn't (DENEXT.md
    // item 124: asked for or on by default alike)
    if !config
      .warned
      .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
      log::warn!(
        "{} {}",
        colors::yellow("Warning"),
        unconfined_warning(reason)
      );
    }
  }
  #[cfg(target_os = "linux")]
  if sandbox.is_ok() {
    super::sandbox_linux::become_subreaper();
  }

  let dirs = JailDirs::create(s.root_node_modules, &s.package_id)
    .context("Failed to create the jail's scratch directory")?;
  let result = run_in_dirs(config, &s, &dirs, sandbox.is_ok()).await;
  let _ = std::fs::remove_dir_all(&dirs.root);
  result
}

/// The package's own folder, as its script reaches it and resolved (an
/// isolated `node_modules` links it from `node_modules/.deno/`): where a
/// jailed `deno` may load a native addon from. Only where the OS confines
/// what the addon does, as it confines the programs a script writes there;
/// without an OS sandbox, no native code at all.
fn package_ffi_roots(package_folder: &Path, os_sandbox: bool) -> Vec<PathBuf> {
  if !os_sandbox {
    return Vec::new();
  }
  let mut roots = vec![package_folder.to_path_buf()];
  if let Ok(canonical) = canonicalize_path(package_folder)
    && canonical != package_folder
  {
    roots.push(canonical);
  }
  roots
}

async fn run_in_dirs(
  config: &JailConfig,
  s: &JailedScript<'_>,
  dirs: &JailDirs,
  os_sandbox: bool,
) -> Result<TaskResult, AnyError> {
  let package = config.packages.get(s.package_name);
  let canonical = |p: &Path| resolve_existing_prefix(p);

  // writable roots: the package's own folder, not the rest of node_modules
  // (DENEXT.md item 111), so one package's script can't rewrite another's
  // files, which run later outside the jail
  let mut write = vec![canonical(s.package_folder), canonical(&dirs.root)];
  if let Some(package) = package {
    write.extend(
      package_write_paths(package, home_dir().as_deref())
        .iter()
        .map(|p| canonical(p)),
    );
  }
  write.dedup();

  let NetChoice {
    net,
    declared,
    refused_declared,
  } = jail_net(config, package, s.package_name, s.package_folder);
  if !refused_declared.is_empty()
    && config
      .warned_declared
      .lock()
      .map(|mut seen| seen.insert(s.package_id.clone()))
      .unwrap_or(true)
  {
    // a declaration is a request: say what it asked for and the exact line
    // that grants it (DENEXT.md item 124)
    let hosts = refused_declared
      .iter()
      .map(|h| format!("\"{h}\""))
      .collect::<Vec<_>>()
      .join(", ");
    let line = config.supply_chain_line(&format!(
      "{{ \"jailScripts\": {{ \"packages\": {{ \"{}\": {{ \"net\": [{hosts}] }} }} }} }}",
      s.package_name
    ));
    log::warn!(
      "{} jail-scripts: npm:{}'s package.json declares a download host this project hasn't allowed: {}. Its scripts can't reach it. To allow it, {line}",
      colors::yellow("Warning"),
      s.package_id,
      refused_declared.join(", "),
    );
  }
  // a host list: the allow-listing proxy, the only address the OS lets the
  // script connect to where it can hold it there
  let allow = match &net {
    JailNet::Hosts(hosts) => {
      // the project's opt-in for a mirror on the local network covers the
      // host list it wrote, never the hosts a package declares for itself,
      // which get no exemption at all (DENEXT.md item 124)
      let allow_private =
        !declared && package.is_some_and(|package| package.allow_private);
      Some(
        super::jail_proxy::AllowList::parse(hosts)
          .map_err(|err| deno_core::anyhow::anyhow!("jail-scripts: {err}"))?
          .with_allow_private(allow_private)
          .with_declared(declared),
      )
    }
    _ => None,
  };
  let upstream = super::jail_proxy::Upstream::from_env(s.env_vars)
    .with_by_name(config.upstream_by_name);
  // Linux: a script with a host list gets a network namespace of its own,
  // where the proxy listens on a port chosen here (nothing else is on that
  // loopback); the listener is handed back from the child (DENEXT.md item
  // 124). Without one, the proxy listens on the host's loopback as before.
  #[cfg(target_os = "linux")]
  let netns = os_sandbox
    && allow.is_some()
    && super::sandbox_linux::netns_available().is_ok();
  #[cfg(not(target_os = "linux"))]
  let netns = false;
  let mut proxy = match &allow {
    Some(allow) if !netns => Some(
      super::jail_proxy::JailProxy::start(allow.clone(), upstream.clone())
        .await
        .context("Failed to start the jail's network proxy")?,
    ),
    _ => None,
  };
  #[cfg(target_os = "linux")]
  let netns_port = netns.then(super::sandbox_linux::pick_proxy_port);
  #[cfg(not(target_os = "linux"))]
  let netns_port: Option<u16> = None;
  let proxy_port = proxy
    .as_ref()
    .map(|p| p.port())
    .or(netns_port)
    .unwrap_or_default();
  let os_net = choose_os_net(
    &net,
    declared,
    config.paranoid,
    proxy_port,
    os_sandbox && can_confine_to_proxy(),
  );
  let net = match (net, os_net) {
    (JailNet::Hosts(hosts), OsNet::Denied) if os_sandbox => {
      log::warn!(
        "{} jail-scripts: this system can't hold npm:{}'s '{}' script to its hosts ({}), so it runs without network. Linux 6.7+ (Landlock ABI 4), or unprivileged user namespaces, can.",
        colors::yellow("Warning"),
        s.package_id,
        s.task_name,
        hosts.join(", ")
      );
      proxy = None;
      JailNet::None
    }
    (JailNet::Hosts(hosts), OsNet::Open) if os_sandbox => {
      if !config
        .warned_net
        .swap(true, std::sync::atomic::Ordering::SeqCst)
      {
        log::warn!(
          "{} jail-scripts: this system can't hold lifecycle scripts to the hosts {} lists for them (npm:{}: {}); programs other than Deno that they start can reach any host. Linux 6.7+ (Landlock ABI 4), or unprivileged user namespaces, can.",
          colors::yellow("Warning"),
          config.settings_file(),
          s.package_id,
          hosts.join(", ")
        );
      }
      JailNet::Hosts(hosts)
    }
    (net, _) => net,
  };
  #[cfg(target_os = "linux")]
  if os_sandbox
    && matches!(os_net, OsNet::ProxyOnly(_))
    && !netns
    && let Err(reason) = super::sandbox_linux::netns_available()
    && !config
      .warned_netns
      .swap(true, std::sync::atomic::Ordering::SeqCst)
  {
    log::warn!(
      "{} {}",
      colors::yellow("Warning"),
      no_netns_warning(&reason)
    );
  }

  let mut deny_read = credential_paths();
  deny_read.extend(config.project_secret_files().iter().cloned());
  // links in node_modules that lead outside it (workspace members, `file:`
  // dependencies)
  let outside = config.outside_links.get_or_init(|| {
    let root = canonical(s.root_node_modules);
    outside_links(&root, std::slice::from_ref(&root))
  });
  // with any network, the rest of the real home directories too, but what
  // the script needs (DENEXT.md item 437): the OS sandbox holds it
  // (Seatbelt, Landlock); a `deno` the script starts keeps the credential
  // paths above
  let homes: Vec<PathBuf> = real_homes()
    .iter()
    .map(|home| canonical(home))
    .filter(|home| home.parent().is_some())
    .fold(Vec::new(), |mut homes, home| {
      if !homes.contains(&home) {
        homes.push(home);
      }
      homes
    });
  let home_read = (os_sandbox && os_net != OsNet::Denied && !homes.is_empty())
    .then(|| {
      // the running deno itself, not the rest of its directory
      #[allow(clippy::disallowed_methods, reason = "spawning this executable")]
      let exe = std::env::current_exe()
        .ok()
        .and_then(|p| canonicalize_path(&p).ok());
      let path_var = s
        .env_vars
        .iter()
        .find(|(k, _)| {
          if cfg!(windows) {
            k.to_string_lossy().eq_ignore_ascii_case("PATH")
          } else {
            k.as_os_str() == "PATH"
          }
        })
        .map(|(_, v)| v.clone());
      let project = config
        .project_dirs
        .iter()
        .map(|dir| canonical(dir))
        .chain([canonical(s.init_cwd)])
        .collect::<Vec<_>>();
      let roots = write
        .iter()
        .cloned()
        .chain(project.iter().cloned())
        .chain([
          s.root_node_modules.to_path_buf(),
          s.package_folder.to_path_buf(),
        ])
        // of DENO_DIR, the npm cache and the node headers, not the other
        // programs' data (`location_data`) or the REPL history
        .chain(
          config
            .deno_dir
            .iter()
            .flat_map(|dir| [dir.join("npm"), dir.join("node_headers")]),
        )
        .chain(exe)
        .chain(outside.iter().cloned())
        .chain(discovery_files(&homes, &project))
        .chain(package_manager_dirs(&homes, path_var.as_deref(), &|name| {
          std::env::var_os(name)
        }));
      let allowed = home_read_allowed(&homes, roots, path_var.as_deref());
      HomeReadPolicy { homes, allowed }
    });
  // links in node_modules that lead outside it, where a package's own
  // `write` paths don't reach
  let deny_write = outside
    .iter()
    .filter(|target| !path_within(target, &write))
    .cloned()
    .collect();
  // the proxy's address: the one started here, or the port the script's
  // process binds in its namespace (none when the host list got no network:
  // the proxy was dropped above)
  let proxy_address = proxy
    .as_ref()
    .map(|p| p.port())
    .or(netns_port)
    .map(|port| format!("127.0.0.1:{port}"));
  let permissions = JailPermissions {
    write: write.clone(),
    deny_write,
    deny_read: deny_read.clone(),
    net: net.clone(),
    proxy: proxy_address.clone(),
    ffi: package_ffi_roots(s.package_folder, os_sandbox),
    os_confined: os_sandbox && !cfg!(windows),
  };

  // the environment
  let mut env: HashMap<OsString, OsString> = HashMap::new();
  let builtin_env =
    super::builtin_exceptions::builtin_exception(s.package_name)
      .filter(|_| !config.paranoid)
      .map(|e| e.env)
      .unwrap_or_default();
  for (key, value) in s.env_vars {
    let name = key.to_string_lossy();
    let same = |n: &str| {
      n == name.as_ref() || (cfg!(windows) && n.eq_ignore_ascii_case(&name))
    };
    let passed = package.is_some_and(|p| p.env.iter().any(|n| same(n)))
      || builtin_env.iter().any(|n| same(n));
    if passed || keep_env_var(&name, net == JailNet::All) {
      // a registry or proxy URL goes in without its credentials
      let value = if url_valued_env_var(&name) {
        OsString::from(strip_url_credentials(&value.to_string_lossy()))
      } else {
        value.clone()
      };
      env.insert(key.clone(), value);
    }
  }
  if let Some(address) = &proxy_address {
    set_proxy_env(&mut env, &format!("http://{address}"));
  }
  // a native build from source gets Node's headers offline, unless the
  // project names its own (DENEXT.md item 288)
  let node_gyp_build = config.node_headers.is_some()
    && super::node_headers::needs_node_gyp(s.package_folder, s.script);
  if let Some(headers) = &config.node_headers
    && node_gyp_build
    && !env.keys().any(|k| {
      k.to_string_lossy()
        .eq_ignore_ascii_case("npm_config_nodedir")
    })
  {
    let mirror = s
      .env_vars
      .get(std::ffi::OsStr::new("NODEJS_ORG_MIRROR"))
      .map(|v| v.to_string_lossy().into_owned());
    if let Some(dir) = headers.dir(mirror.as_deref()).await {
      env.insert("npm_config_nodedir".into(), dir.into_os_string());
    }
    // the pinned node-gyp, installed outside the jail (the script can't
    // reach the registry), unless the project names its own
    if !s
      .env_vars
      .contains_key(std::ffi::OsStr::new("DENO_NODE_GYP"))
      && let Some(entry) = headers.node_gyp().await
    {
      env.insert("DENO_NODE_GYP".into(), entry.into_os_string());
    }
  }
  let mut set = |key: &str, value: &Path| {
    let key = if cfg!(windows) {
      key.to_ascii_uppercase()
    } else {
      key.to_string()
    };
    env.insert(key.into(), value.as_os_str().to_os_string());
  };
  set("HOME", &dirs.home);
  set("TMPDIR", &dirs.tmp);
  set("TEMP", &dirs.tmp);
  set("TMP", &dirs.tmp);
  set("DENO_DIR", &dirs.deno_dir);
  if cfg!(windows) {
    set("USERPROFILE", &dirs.home);
    set("APPDATA", &dirs.home.join("AppData").join("Roaming"));
    set("LOCALAPPDATA", &dirs.home.join("AppData").join("Local"));
  } else {
    set("XDG_CONFIG_HOME", &dirs.home.join(".config"));
    set("XDG_CACHE_HOME", &dirs.home.join(".cache"));
    set("XDG_DATA_HOME", &dirs.home.join(".local").join("share"));
  }
  env.insert("DENO_NO_UPDATE_CHECK".into(), "1".into());
  env.insert(
    JAIL_ENV_VAR.into(),
    serde_json::to_string(&permissions)?.into(),
  );

  let mut spec = ChildSpec {
    task_name: s.task_name.to_string(),
    script: s.script.to_string(),
    cwd: s.package_folder.to_path_buf(),
    init_cwd: s.init_cwd.to_path_buf(),
    env: env
      .iter()
      .map(|(k, v)| {
        (
          k.to_string_lossy().into_owned(),
          v.to_string_lossy().into_owned(),
        )
      })
      .collect(),
    bins: s.bins.clone(),
    node_modules_bin_dirs: s.node_modules_bin_dirs.clone(),
  };

  #[allow(clippy::disallowed_methods, reason = "spawning this executable")]
  let current_exe = std::env::current_exe()
    .and_then(|p| canonicalize_path(&p))
    .context("Failed to resolve the deno executable")?;
  // `node-gyp` on PATH for the build's own children (node-gyp-build), after
  // the dependencies' bins, so a node-gyp the project installs wins
  // (DENEXT.md item 297)
  if node_gyp_build {
    let bin = dirs.root.join("bin");
    match super::node_headers::write_node_gyp_shim(&bin, &current_exe) {
      Ok(()) => spec.node_modules_bin_dirs.push(bin),
      Err(err) => {
        log::debug!(
          "jail-scripts: the node-gyp command couldn't be written: {err}"
        )
      }
    }
  }

  #[cfg(target_os = "macos")]
  let mut command = if os_sandbox {
    let mut command =
      tokio::process::Command::new(super::sandbox_macos::SANDBOX_EXEC);
    let (seatbelt_write, seatbelt_deny) = seatbelt_paths(&write, &deny_read);
    command
      .arg("-p")
      .arg(deno_sandbox::seatbelt::profile_with_home_read(
        &seatbelt_write,
        &seatbelt_deny,
        home_read.as_ref(),
        os_net,
      ));
    command.arg(&current_exe);
    command
  } else {
    tokio::process::Command::new(&current_exe)
  };
  #[cfg(not(target_os = "macos"))]
  let mut command = tokio::process::Command::new(&current_exe);
  command
    .arg(CHILD_ARG)
    .current_dir(s.package_folder)
    .env_clear()
    .envs(&env)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
  // its own session and process group, before anything else: no controlling
  // terminal, and one group to end when the script does
  #[cfg(unix)]
  detach_session(&mut command);

  #[cfg(target_os = "linux")]
  let netns_handoff = match netns_port {
    Some(port) => {
      let handoff = super::sandbox_linux::NetnsHandoff::new(port)
        .context("Failed to set up the jail's network namespace")?;
      handoff.apply_to_command(&mut command);
      Some(handoff)
    }
    None => None,
  };
  #[cfg(target_os = "linux")]
  let private_shm =
    os_sandbox && super::sandbox_linux::private_shm_available().is_ok();
  #[cfg(target_os = "linux")]
  let _ruleset = if os_sandbox {
    let proxy_port = match os_net {
      OsNet::ProxyOnly(port) => Some(port),
      _ => None,
    };
    // never the host's /dev/shm (every same-user process's shared memory
    // and semaphores): a private one where namespaces allow, else none
    let ruleset = super::sandbox_linux::build_ruleset_opts(
      &write,
      &linux_os_deny_read(&deny_read),
      home_read.as_ref(),
      proxy_port,
      false,
    )
    .context("Failed to set up the Landlock sandbox")?;
    if private_shm {
      super::sandbox_linux::PrivateShm::new(true, netns)
        .context("Failed to set up the jail's /dev/shm")?
        .apply_to_command(&mut command, &ruleset);
    } else if !NO_SHM_WARNED.swap(true, std::sync::atomic::Ordering::SeqCst)
      && let Err(reason) = super::sandbox_linux::private_shm_available()
    {
      // no /dev/shm only narrows what a script can do, and hosts that
      // restrict unprivileged user namespaces (Ubuntu 23.10+) are common: a
      // script that fails naming shared memory gets a warning instead
      // (`no_shm_hint`)
      log::debug!("{}", no_shm_warning(&reason));
    }
    let abi = super::sandbox_linux::landlock_abi().unwrap_or(1);
    super::sandbox_linux::apply_to_command(
      &mut command,
      &ruleset,
      &seccomp_spec_for(os_net, abi),
    );
    Some(ruleset)
  } else {
    None
  };
  // what Landlock doesn't confine on every kernel (signals, metadata
  // changes, truncation before ABI 3) goes to the supervisor; its filter is
  // installed after the jail's own
  #[cfg(target_os = "linux")]
  let notify_handoff = if os_sandbox {
    let abi = super::sandbox_linux::landlock_abi().unwrap_or(1);
    let handoff = deno_sandbox::notify::NotifyHandoff::new()
      .context("Failed to set up the jail's supervisor")?;
    handoff.apply_to_command(
      &mut command,
      deno_sandbox::notify::notify_program(&supervised_syscalls(abi)),
    );
    Some(handoff)
  } else {
    None
  };
  #[cfg(not(any(target_os = "linux", target_os = "macos")))]
  let _ = (os_net, &home_read);
  // the installer's children before the script: what it adopts later as a
  // child subreaper and doesn't recognize is a stray of the script
  #[cfg(target_os = "linux")]
  let children_before = os_sandbox.then(super::sandbox_linux::children_of_self);

  let started = SystemTime::now();
  let mut child = command.spawn().with_context(|| {
    format!("Failed to start the jailed '{}' script", s.task_name)
  })?;
  #[cfg(target_os = "linux")]
  drop(_ruleset);
  let child_pid = child.id();
  #[cfg(target_os = "linux")]
  if let Some(handoff) = netns_handoff {
    // the listener the child bound on its own loopback; the proxy accepts
    // on it here and dials upstream from the host's network
    let listener = handoff
      .receive()
      .await
      .context("The jailed script's network namespace wasn't set up")?;
    proxy = Some(
      super::jail_proxy::JailProxy::start_on(
        listener,
        allow.clone().unwrap_or_default(),
        upstream.clone(),
      )
      .context("Failed to start the jail's network proxy")?,
    );
  }
  #[cfg(target_os = "linux")]
  let supervisor = match (notify_handoff, child_pid) {
    (Some(handoff), Some(pid)) => {
      let listener = tokio::task::spawn_blocking(move || handoff.receive())
        .await
        .map_err(std::io::Error::other)
        .and_then(|r| r)
        .context("The jailed script's supervisor wasn't set up")?;
      let roots = write.clone();
      let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
      let thread_stop = stop.clone();
      let thread = std::thread::spawn(move || {
        deno_sandbox::notify::Supervisor::new(roots, pid)
          .with_private_shm(private_shm)
          .serve(listener, &thread_stop)
      });
      Some((stop, thread))
    }
    _ => None,
  };

  let spec = serde_json::to_vec(&spec)?;
  if let Some(mut stdin) = child.stdin.take() {
    // the script gets an empty stdin after the spec
    let _ = stdin.write_all(&spec).await;
    drop(stdin);
  }
  let mut stdout = child.stdout.take().unwrap();
  let mut stderr = child.stderr.take().unwrap();
  let read_out = tokio::spawn(async move {
    read_capped(&mut stdout, MAX_SCRIPT_OUTPUT_BYTES).await
  });
  let read_err = tokio::spawn(async move {
    read_capped(&mut stderr, MAX_SCRIPT_OUTPUT_BYTES).await
  });
  let status = tokio::select! {
    status = child.wait() => status?,
    _ = s.kill_signal.wait_any() => {
      #[cfg(unix)]
      if let Some(pid) = child_pid {
        kill_process_group(pid);
      }
      let _ = child.kill().await;
      child.wait().await?
    }
  };
  // whatever the script started and left behind goes with it, before the
  // sweep below looks at what it wrote (DENEXT.md item 124)
  #[cfg(unix)]
  if let Some(pid) = child_pid {
    end_process_group(pid).await;
  }
  #[cfg(target_os = "linux")]
  if let Some(before) = children_before {
    super::sandbox_linux::end_adopted_strays(&before);
  }
  // nothing under the filter is left: the supervisor ends
  #[cfg(target_os = "linux")]
  if let Some((stop, thread)) = supervisor {
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = tokio::task::spawn_blocking(move || thread.join()).await;
  }
  #[cfg(not(unix))]
  let _ = child_pid;
  let stdout = read_out.await.unwrap_or_default();
  let mut stderr = read_err.await.unwrap_or_default();
  let mut exit_code = status.code().unwrap_or(1);
  drop(proxy);
  #[cfg(target_os = "linux")]
  if exit_code != 0
    && os_sandbox
    && !private_shm
    && (mentions_shared_memory(&stdout) || mentions_shared_memory(&stderr))
    && let Err(reason) = super::sandbox_linux::private_shm_available()
  {
    log::warn!("{} {}", colors::yellow("Warning"), no_shm_hint(&reason));
  }
  refuse_new_executables(s, package, started, &mut stderr, &mut exit_code);
  Ok(TaskResult {
    exit_code,
    stdout: Some(stdout),
    stderr: Some(stderr),
  })
}

/// The most of a jailed script's stdout, and of its stderr, kept for the
/// installer's report: a script that writes without end can't grow the
/// installer's memory with it.
const MAX_SCRIPT_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

/// Reads `reader` to EOF, keeping its first `limit` bytes. The rest is
/// read and dropped, so the writer never blocks on a full pipe, and a
/// line saying how much was kept ends the output when some was dropped.
async fn read_capped<R: tokio::io::AsyncRead + Unpin>(
  reader: &mut R,
  limit: usize,
) -> Vec<u8> {
  let mut kept = Vec::new();
  let mut chunk = vec![0u8; 64 * 1024];
  let mut truncated = false;
  loop {
    let n = match reader.read(&mut chunk).await {
      Ok(0) | Err(_) => break,
      Ok(n) => n,
    };
    let room = limit.saturating_sub(kept.len());
    if n > room {
      truncated = true;
    }
    kept.extend_from_slice(&chunk[..n.min(room)]);
  }
  if truncated {
    if kept.last().is_some_and(|b| *b != b'\n') {
      kept.push(b'\n');
    }
    kept.extend_from_slice(
      format!("[output truncated after {limit} bytes]\n").as_bytes(),
    );
  }
  kept
}

/// Puts the command's process in a session and process group of its own
/// (`setsid`), inherited by everything it starts: no controlling terminal
/// to type into (`TIOCSTI`), and `kill(-pid)` reaches the whole tree.
#[cfg(unix)]
fn detach_session(command: &mut tokio::process::Command) {
  // SAFETY: setsid is async-signal-safe and touches no memory
  unsafe {
    command.pre_exec(|| {
      if libc::setsid() < 0 {
        return Err(std::io::Error::last_os_error());
      }
      Ok(())
    });
  }
}

/// Sends SIGKILL to the process group whose id is `pid` (the jailed
/// script's, a session leader since [`detach_session`]).
#[cfg(unix)]
fn kill_process_group(pid: u32) {
  let Ok(pgid) = i32::try_from(pid) else {
    return;
  };
  // SAFETY: a signal to a group id; a stale id means ESRCH, not harm (the
  // group is the script's own session, created by this install)
  unsafe { libc::kill(-pgid, libc::SIGKILL) };
}

/// Ends the script's process group and waits (briefly) until no member is
/// left: the installer has reaped the script itself already; its orphaned
/// descendants are reaped here where they became the installer's (Linux, a
/// child subreaper) or by init elsewhere.
#[cfg(unix)]
async fn end_process_group(pid: u32) {
  let Ok(pgid) = i32::try_from(pid) else {
    return;
  };
  kill_process_group(pid);
  for _ in 0..300 {
    // reap what's ours in the group (adopted orphans), without blocking
    loop {
      let mut status = 0;
      // SAFETY: waitpid on a group id with WNOHANG
      let reaped = unsafe { libc::waitpid(-pgid, &mut status, libc::WNOHANG) };
      if reaped <= 0 {
        break;
      }
    }
    // SAFETY: signal 0 probes the group
    if unsafe { libc::kill(-pgid, 0) } != 0
      && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    {
      return;
    }
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
  }
  log::debug!("jail-scripts: process group {pgid} did not end in time");
}

/// Removes the executables a script left in its package's `~` write paths,
/// unless the package lists `executables: true`, and fails the script then
/// (DENEXT.md item 111).
#[cfg(unix)]
fn refuse_new_executables(
  s: &JailedScript<'_>,
  package: Option<&JailPackageConfig>,
  started: SystemTime,
  stderr: &mut Vec<u8>,
  exit_code: &mut i32,
) {
  let Some(package) = package else {
    return;
  };
  if package.executables {
    return;
  }
  let removed = remove_new_executables(
    &home_write_paths(package, home_dir().as_deref()),
    started,
  );
  if removed.is_empty() {
    return;
  }
  let list = removed
    .iter()
    .map(|p| format!("  {}", p.display()))
    .collect::<Vec<_>>()
    .join("\n");
  stderr.extend_from_slice(
    format!(
      "\njail-scripts: npm:{}'s '{}' script left executable files in a directory outside the project; they were removed:\n{list}\nOther projects run what such a cache holds outside any jail. If the package needs them (a browser download), add \"executables\": true to its entry in supplyChain.jailScripts.packages.\n",
      s.package_id, s.task_name
    )
    .as_bytes(),
  );
  if *exit_code == 0 {
    *exit_code = 1;
  }
}

#[cfg(not(unix))]
fn refuse_new_executables(
  _s: &JailedScript<'_>,
  _package: Option<&JailPackageConfig>,
  _started: SystemTime,
  _stderr: &mut Vec<u8>,
  _exit_code: &mut i32,
) {
}

/// The jailed script's process: reads the script from stdin and runs it with
/// the task shell, as the lifecycle-script executor would in-process.
pub async fn run_child() -> i32 {
  match run_child_inner().await {
    Ok(code) => code,
    Err(err) => {
      log::error!("{} {err:#}", colors::red_bold("error:"));
      1
    }
  }
}

async fn run_child_inner() -> Result<i32, AnyError> {
  if jail_from_env().is_none() {
    bail!("not a jailed lifecycle script");
  }
  let mut input = Vec::new();
  tokio::io::stdin().read_to_end(&mut input).await?;
  let spec: ChildSpec = serde_json::from_slice(&input)?;
  let mut commands = crate::task_runner::TaskCustomCommands::new();
  commands.insert(
    "npx".to_string(),
    std::rc::Rc::new(crate::task_runner::NpxCommand),
  );
  commands.insert(
    "npm".to_string(),
    std::rc::Rc::new(crate::task_runner::NpmCommand),
  );
  commands.insert(
    "node".to_string(),
    std::rc::Rc::new(crate::task_runner::NodeCommand),
  );
  commands.insert(
    "node-gyp".to_string(),
    std::rc::Rc::new(crate::task_runner::NodeGypCommand),
  );
  for (name, path) in spec.bins {
    commands.insert(
      name.clone(),
      std::rc::Rc::new(crate::task_runner::NodeModulesFileRunCommand {
        command_name: name,
        path,
      }),
    );
  }
  let env_vars = spec
    .env
    .into_iter()
    .map(|(k, v)| (OsString::from(k), OsString::from(v)))
    .collect();
  let result =
    crate::task_runner::run_task(crate::task_runner::RunTaskOptions {
      task_name: &spec.task_name,
      script: &spec.script,
      cwd: spec.cwd,
      env_vars,
      custom_commands: commands,
      init_cwd: &spec.init_cwd,
      argv: &[],
      node_modules_bin_dirs: &spec.node_modules_bin_dirs,
      stdio: None,
      kill_signal: KillSignal::default(),
    })
    .await?;
  Ok(result.exit_code)
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A config with no package entries.
  fn config(net: bool, paranoid: bool) -> JailConfig {
    JailConfig::new(paranoid, net, BTreeMap::new(), &[], None)
  }

  // DENEXT.md item 437: what a networked script may read under the home
  // directory
  #[test]
  #[allow(clippy::disallowed_methods, reason = "test fixtures on disk")]
  fn home_read_allowed_keeps_the_needed_paths_only() {
    let tmp = tempfile::tempdir().unwrap();
    let home = canonicalize_path(tmp.path()).unwrap();
    for dir in [
      "proj/node_modules",
      ".nvm/versions/node/v1/bin",
      "bin",
      ".local/bin",
      ".rustup",
    ] {
      std::fs::create_dir_all(home.join(dir)).unwrap();
    }
    let path_var = std::env::join_paths([
      home.join(".nvm/versions/node/v1/bin"),
      home.join("bin"),
      home.join(".local/bin"),
      PathBuf::from("/usr/bin"),
      PathBuf::from("relative/bin"),
    ])
    .unwrap();
    let allowed = home_read_allowed(
      std::slice::from_ref(&home),
      [
        home.join("proj"),
        home.join("proj/node_modules"),
        home.join("proj"),
        // the home directory itself and what holds it: never
        home.clone(),
        home.parent().unwrap().to_path_buf(),
        // outside it: nothing to allow
        PathBuf::from("/opt/x"),
      ],
      Some(&path_var),
    );
    // `~/bin` and `~/.local/bin` themselves, not what holds them; the
    // toolchain homes that exist (`.nvm`, `.rustup`; not `.cargo`)
    let mut expected = vec![
      home.join(".local/bin"),
      home.join(".nvm"),
      home.join(".nvm/versions/node/v1"),
      home.join(".nvm/versions/node/v1/bin"),
      home.join(".rustup"),
      home.join("bin"),
      home.join("proj"),
      home.join("proj/node_modules"),
    ];
    expected.sort();
    assert_eq!(allowed, expected);
  }

  // a prefix directly in a home directory gives its `lib` (where its
  // commands link), not the rest of it; of `.cargo`, only the commands and
  // the crate caches; candidates are resolved before the check, and in two
  // home directories one inside the other, what holds the inner one is
  // never allowed
  #[allow(clippy::disallowed_methods, reason = "test fixtures on disk")]
  #[cfg(unix)]
  #[test]
  fn home_read_allowed_prefix_lib_and_cargo_parts() {
    let tmp = tempfile::tempdir().unwrap();
    let home = canonicalize_path(tmp.path()).unwrap();
    for dir in [
      ".npm-global/bin",
      ".npm-global/lib/node_modules",
      ".npm-global/etc",
      ".cargo/bin",
      ".cargo/registry",
      "inner/home/proj",
      "sibling",
    ] {
      std::fs::create_dir_all(home.join(dir)).unwrap();
    }
    std::fs::write(home.join(".cargo/config.toml"), "token").unwrap();
    std::os::unix::fs::symlink(home.join("sibling"), home.join("proj_link"))
      .unwrap();
    let inner = home.join("inner/home");
    let path_var =
      std::env::join_paths([home.join(".npm-global/bin")]).unwrap();
    let allowed = home_read_allowed(
      &[home.clone(), inner.clone()],
      [
        inner.join("proj"),
        home.join("inner"),
        // resolved first: the link's target is what is allowed
        home.join("proj_link"),
      ],
      Some(&path_var),
    );
    let mut expected = vec![
      home.join(".cargo/bin"),
      home.join(".cargo/registry"),
      home.join(".npm-global/bin"),
      home.join(".npm-global/lib"),
      inner.join("proj"),
      home.join("sibling"),
    ];
    expected.sort();
    assert_eq!(allowed, expected);
  }

  // the package managers' global folders come from their own configuration
  #[allow(clippy::disallowed_methods, reason = "test fixtures on disk")]
  #[cfg(unix)]
  #[test]
  fn package_manager_dirs_follow_their_config() {
    let tmp = tempfile::tempdir().unwrap();
    let home = canonicalize_path(tmp.path()).unwrap();
    for dir in [
      ".npm-packages/lib/node_modules",
      ".npm-packages/bin",
      "pnpm-global",
      "pnpm-home",
      "yarn-global",
      "bun-global",
      ".config/pnpm",
      ".local/lib/python3.12/site-packages",
      "n/bin",
      "n/lib/node_modules",
    ] {
      std::fs::create_dir_all(home.join(dir)).unwrap();
    }
    std::fs::write(
      home.join(".npmrc"),
      "; comment\n//registry.example/:_authToken=x\nprefix = ~/.npm-packages\n",
    )
    .unwrap();
    std::fs::write(
      home.join(".config/pnpm/rc"),
      "global-dir=${HOME}/pnpm-global\n",
    )
    .unwrap();
    std::fs::write(
      home.join(".yarnrc"),
      format!("global-folder \"{}\"\n", home.join("yarn-global").display()),
    )
    .unwrap();
    std::fs::write(
      home.join(".bunfig.toml"),
      "[run]\nglobalDir = \"~/no\"\n[install]\nglobalDir = \"~/bun-global\"\n",
    )
    .unwrap();
    std::fs::write(home.join("n/bin/node"), "").unwrap();
    let pnpm_home = home.join("pnpm-home").into_os_string();
    let env =
      move |name: &str| (name == "PNPM_HOME").then(|| pnpm_home.clone());
    let path_var = std::env::join_paths([home.join("n/bin")]).unwrap();
    let dirs =
      package_manager_dirs(std::slice::from_ref(&home), Some(&path_var), &env);
    let mut expected = vec![
      home.join(".npm-packages/lib"),
      home.join(".npm-packages/bin"),
      home.join("pnpm-global"),
      home.join("pnpm-home"),
      home.join("yarn-global"),
      home.join("bun-global"),
      home.join("n/lib"),
      home.join("n/bin"),
    ];
    if !cfg!(target_os = "macos") {
      expected.push(home.join(".local/lib/python3.12"));
    }
    for dir in &expected {
      assert!(dirs.contains(dir), "{dir:?} not in {dirs:?}");
    }
    assert!(!dirs.contains(&home.join("no")), "{dirs:?}");
    assert!(!dirs.contains(&home), "{dirs:?}");
  }

  // the files config discovery looks for above a project stay readable up
  // to the home directory, a link among them not
  #[allow(clippy::disallowed_methods, reason = "test fixtures on disk")]
  #[cfg(unix)]
  #[test]
  fn discovery_files_between_the_project_and_home() {
    let tmp = tempfile::tempdir().unwrap();
    let root = canonicalize_path(tmp.path()).unwrap();
    let home = root.join("home");
    let project = home.join("code/app");
    std::fs::create_dir_all(&project).unwrap();
    for file in [
      root.join("package.json"),
      home.join("package.json"),
      home.join("code/deno.json"),
      home.join("notes.txt"),
      project.join("package.json"),
    ] {
      std::fs::write(file, "{}").unwrap();
    }
    std::os::unix::fs::symlink(home.join("notes.txt"), home.join(".nvmrc"))
      .unwrap();
    let mut files = discovery_files(std::slice::from_ref(&home), &[project]);
    files.sort();
    assert_eq!(
      files,
      vec![home.join("code/deno.json"), home.join("package.json")]
    );
  }

  // the account's home directory counts whatever `$HOME` says; `$HOME`
  // counts too when it is another directory, never when it is `/`
  /// An absolute path on this platform: `/a/b`, or `C:/a/b` on Windows
  /// (where `/a/b` has no drive, so it isn't absolute).
  fn abs(path: &str) -> PathBuf {
    if cfg!(windows) {
      PathBuf::from(format!("C:{path}"))
    } else {
      PathBuf::from(path)
    }
  }

  /// [`abs`] as the string a permission list holds.
  fn abs_s(path: &str) -> String {
    abs(path).to_string_lossy().into_owned()
  }

  #[test]
  fn real_homes_do_not_depend_on_the_home_variable() {
    let account = abs("/home/u");
    assert_eq!(
      real_homes_from(Some(account.clone()), None),
      vec![account.clone()]
    );
    assert_eq!(
      real_homes_from(Some(account.clone()), Some(PathBuf::from("/"))),
      vec![account.clone()]
    );
    assert_eq!(
      real_homes_from(Some(account.clone()), Some(PathBuf::from("rel"))),
      vec![account.clone()]
    );
    assert_eq!(
      real_homes_from(Some(account.clone()), Some(account.clone())),
      vec![account.clone()]
    );
    assert_eq!(
      real_homes_from(Some(account.clone()), Some(abs("/tmp/h"))),
      vec![account, abs("/tmp/h")]
    );
    assert!(real_homes_from(None, Some(PathBuf::from("/"))).is_empty());
  }

  #[cfg(unix)]
  #[test]
  fn account_home_is_from_the_user_database() {
    // an account without an entry in the user database (some containers)
    // has none; one that has an entry never gets `/` or a relative path
    if let Some(home) = account_home() {
      assert!(home.is_absolute() && home.parent().is_some(), "{home:?}");
    }
  }

  // a failing script is warned about the missing /dev/shm only when its
  // output names shared memory or a semaphore
  #[cfg(target_os = "linux")]
  #[test]
  fn mentions_shared_memory_only_for_its_markers() {
    for hit in [
      &b"OSError: [Errno 2] No such file or directory: '/dev/shm/x'"[..],
      b"shm_open failed",
      b"_multiprocessing.SemLock(",
      b"sem_open: Permission denied",
    ] {
      assert!(
        mentions_shared_memory(hit),
        "{}",
        String::from_utf8_lossy(hit)
      );
    }
    assert!(!mentions_shared_memory(b"npm_config_user_agent: deno"));
    assert!(!mentions_shared_memory(b""));
  }

  // the warning names the missing /dev/shm and why
  #[cfg(target_os = "linux")]
  #[test]
  fn no_shm_hint_names_dev_shm_and_the_reason() {
    let hint = no_shm_hint("a user and mount namespace failed");
    assert!(!hint.contains('\n'), "one line: {hint:?}");
    assert!(hint.contains("no /dev/shm"), "{hint}");
    assert!(
      hint.contains("(a user and mount namespace failed)"),
      "{hint}"
    );
  }

  #[test]
  fn credential_paths_name_the_deploy_tokens_and_histories() {
    for name in [
      ".fly",
      ".config/configstore",
      ".vercel",
      ".supabase",
      ".appstoreconnect",
      "private_keys",
      ".zsh_history",
      ".bash_history",
      ".local/share/fish/fish_history",
      ".mozilla",
      "Library/Application Support/Firefox",
      "Library/Application Support/Google/Chrome",
      ".config/google-chrome",
      ".config/chromium",
      ".netrc",
      ".git-credentials",
      ".docker",
      ".npmrc",
    ] {
      assert!(
        CREDENTIAL_PATHS_IN_HOME.contains(&name),
        "{name} is not a credential path"
      );
    }
  }

  /// The parts of a [`NetChoice`] the older tests compare.
  fn net_of(choice: NetChoice) -> (JailNet, bool) {
    (choice.net, choice.declared)
  }

  #[test]
  fn env_allow_list() {
    for name in [
      "PATH",
      "LC_ALL",
      "npm_package_name",
      "npm_config_registry",
      "npm_config_user_agent",
    ] {
      assert!(keep_env_var(name, false), "{name}");
    }
    for name in [
      "GITHUB_TOKEN",
      "AWS_SECRET_ACCESS_KEY",
      "NPM_TOKEN",
      "SSH_AUTH_SOCK",
      "DENO_AUTH_TOKENS",
      "DENO_DIR",
      "npm_config__authToken",
      "npm_config_//registry.npmjs.org/:_authToken",
      "NPM_CONFIG__AUTH",
      "npm_config_email",
      "HTTP_PROXY",
    ] {
      assert!(!keep_env_var(name, false), "{name}");
    }
    assert!(keep_env_var("HTTPS_PROXY", true));
  }

  #[test]
  fn declared_hosts() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
      dir.path().join("package.json"),
      r#"{ "binary": { "host": "https://github.com/a/b/releases/", "staging_host": "https://s3.example.com:8443/x", "module_name": "m" } }"#,
    )
    .unwrap();
    assert_eq!(
      declared_net_hosts(dir.path()),
      vec!["github.com".to_string(), "s3.example.com:8443".to_string()]
    );
    std::fs::write(dir.path().join("package.json"), "{}").unwrap();
    assert!(declared_net_hosts(dir.path()).is_empty());
  }

  // DENEXT.md item 108: a per-package host allow-list
  #[test]
  fn package_net() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
      dir.path().join("package.json"),
      r#"{ "binary": { "host": "https://declared.example.com/" } }"#,
    )
    .unwrap();
    let package = |net: Option<JailNetConfig>| JailPackageConfig {
      net,
      ..Default::default()
    };
    let hosts = JailNetConfig::Hosts(vec![
      "registry.npmjs.org".to_string(),
      "github.com".to_string(),
    ]);
    let expected_hosts = JailNet::Hosts(vec![
      "registry.npmjs.org".to_string(),
      "github.com".to_string(),
    ]);
    // the listed hosts, whatever else would apply
    for (net, paranoid) in
      [(false, false), (true, false), (false, true), (true, true)]
    {
      assert_eq!(
        net_of(jail_net(
          &config(net, paranoid),
          Some(&package(Some(hosts.clone()))),
          "pkg",
          dir.path()
        )),
        (expected_hosts.clone(), false),
        "net {net}, paranoid {paranoid}"
      );
    }
    // the boolean forms keep their meaning
    let all = package(Some(JailNetConfig::All(true)));
    let none = package(Some(JailNetConfig::All(false)));
    assert_eq!(
      net_of(jail_net(
        &config(false, true),
        Some(&all),
        "pkg",
        dir.path()
      )),
      (JailNet::All, false)
    );
    assert_eq!(
      net_of(jail_net(
        &config(false, false),
        Some(&none),
        "pkg",
        dir.path()
      )),
      (JailNet::None, false)
    );
    assert_eq!(
      net_of(jail_net(&config(true, false), None, "pkg", dir.path())),
      (JailNet::All, false)
    );
    // the declared host, marked as the package's own, once the project's
    // `knownPackages` allows it (DENEXT.md item 124)
    let known = config(false, false).with_known_packages(BTreeMap::from([(
      "pkg".to_string(),
      vec!["declared.example.com".to_string()],
    )]));
    assert_eq!(
      jail_net(&known, None, "pkg", dir.path()),
      NetChoice {
        net: JailNet::Hosts(vec!["declared.example.com".to_string()]),
        declared: true,
        refused_declared: Vec::new(),
      }
    );
    assert_eq!(
      net_of(jail_net(&config(false, true), None, "pkg", dir.path())),
      (JailNet::None, false)
    );
  }

  // GitHub-hosted node-pre-gyp downloads of the built-in known packages
  // (bcrypt 5, argon2 < 0.28, @discordjs/opus, libxmljs) reach github.com
  // and the hosts it redirects release downloads to; an unknown package
  // declaring the same host is still refused
  #[test]
  fn builtin_known_github_packages_get_redirect_hosts() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
      dir.path().join("package.json"),
      r#"{ "binary": { "host": "https://github.com", "remote_path": "o/r/releases/download/v5.1.1" } }"#,
    )
    .unwrap();
    for name in ["bcrypt", "argon2", "@discordjs/opus", "libxmljs"] {
      assert_eq!(
        jail_net(&config(false, false), None, name, dir.path()),
        NetChoice {
          net: JailNet::Hosts(vec![
            "github.com:443".to_string(),
            "objects.githubusercontent.com:443".to_string(),
            "release-assets.githubusercontent.com:443".to_string(),
          ]),
          declared: true,
          refused_declared: Vec::new(),
        },
        "{name}"
      );
    }
    assert_eq!(
      jail_net(&config(false, false), None, "pkg", dir.path()),
      NetChoice {
        net: JailNet::None,
        declared: true,
        refused_declared: vec!["github.com".to_string()],
      }
    );
    // paranoid still grants nothing
    assert_eq!(
      jail_net(&config(false, true), None, "bcrypt", dir.path()).net,
      JailNet::None
    );
  }

  // DENEXT.md item 124 (M2): a declared host is a request; without the
  // project's say-so it is refused and named
  #[test]
  fn declared_hosts_need_a_grant() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
      dir.path().join("package.json"),
      r#"{ "binary": { "host": "https://declared.example.com/", "staging_host": "https://s3.example.com:8443/x" } }"#,
    )
    .unwrap();
    // nothing known: no network, both hosts refused
    assert_eq!(
      jail_net(&config(false, false), None, "pkg", dir.path()),
      NetChoice {
        net: JailNet::None,
        declared: true,
        refused_declared: vec![
          "declared.example.com".to_string(),
          "s3.example.com:8443".to_string()
        ],
      }
    );
    // one known (any port covers the declared port), the other refused
    let known = |hosts: &[&str]| {
      config(false, false).with_known_packages(BTreeMap::from([(
        "pkg".to_string(),
        hosts.iter().map(|h| h.to_string()).collect(),
      )]))
    };
    assert_eq!(
      jail_net(&known(&["s3.example.com"]), None, "pkg", dir.path()),
      NetChoice {
        net: JailNet::Hosts(vec!["s3.example.com:8443".to_string()]),
        declared: true,
        refused_declared: vec!["declared.example.com".to_string()],
      }
    );
    // a port in the table must match; names compare as the proxy does
    assert_eq!(
      jail_net(&known(&["s3.example.com:8444"]), None, "pkg", dir.path()).net,
      JailNet::None
    );
    assert_eq!(
      jail_net(
        &known(&["DECLARED.example.com.", "s3.example.com:8443"]),
        None,
        "pkg",
        dir.path()
      ),
      NetChoice {
        net: JailNet::Hosts(vec![
          "declared.example.com".to_string(),
          "s3.example.com:8443".to_string()
        ]),
        declared: true,
        refused_declared: Vec::new(),
      }
    );
    // another package's entry doesn't count
    assert_eq!(
      jail_net(&known(&["declared.example.com"]), None, "other", dir.path())
        .net,
      JailNet::None
    );
    // the project's own list for the package replaces the declaration
    let listed = JailPackageConfig {
      net: Some(JailNetConfig::Hosts(vec!["github.com".to_string()])),
      ..Default::default()
    };
    assert_eq!(
      jail_net(&config(false, false), Some(&listed), "pkg", dir.path()),
      NetChoice {
        net: JailNet::Hosts(vec!["github.com".to_string()]),
        declared: false,
        refused_declared: Vec::new(),
      }
    );
  }

  // DENEXT.md item 111 (R3): a package's `net: false` drops the hosts it
  // declares and wins over `jailScripts.net: true`
  #[test]
  fn package_net_false_wins() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
      dir.path().join("package.json"),
      r#"{ "binary": { "host": "https://declared.example.com/" } }"#,
    )
    .unwrap();
    let none = JailPackageConfig {
      net: Some(JailNetConfig::All(false)),
      ..Default::default()
    };
    for (net, paranoid) in
      [(false, false), (true, false), (false, true), (true, true)]
    {
      assert_eq!(
        net_of(jail_net(
          &config(net, paranoid),
          Some(&none),
          "pkg",
          dir.path()
        )),
        (JailNet::None, false),
        "net {net}, paranoid {paranoid}"
      );
    }
    // a package entry without `net` still takes the global setting
    let env_only = JailPackageConfig::default();
    assert_eq!(
      net_of(jail_net(
        &config(true, false),
        Some(&env_only),
        "pkg",
        dir.path()
      )),
      (JailNet::All, false)
    );
  }

  // DENEXT.md item 288: a package known to download at install gets its
  // reviewed hosts by default; the project's own entry, `net: true` and
  // paranoid still decide first
  #[test]
  fn builtin_exceptions() {
    let dir = tempfile::tempdir().unwrap();
    // canvas 2 declares its GitHub host; the built-in list replaces it
    std::fs::write(
      dir.path().join("package.json"),
      r#"{ "binary": { "host": "https://declared.example.com/" } }"#,
    )
    .unwrap();
    let github_and_headers = JailNet::Hosts(
      [
        "github.com:443",
        "objects.githubusercontent.com:443",
        "release-assets.githubusercontent.com:443",
        "nodejs.org:443",
      ]
      .map(String::from)
      .to_vec(),
    );
    for name in ["better-sqlite3", "sharp", "sqlite3", "canvas", "re2"] {
      assert_eq!(
        jail_net(&config(false, false), None, name, dir.path()),
        NetChoice {
          net: github_and_headers.clone(),
          declared: false,
          refused_declared: Vec::new(),
        },
        "{name}"
      );
    }
    // a package entry without `net` (only `env`) keeps the built-in hosts
    assert_eq!(
      net_of(jail_net(
        &config(false, false),
        Some(&JailPackageConfig::default()),
        "sharp",
        dir.path()
      )),
      (github_and_headers.clone(), false)
    );
    // the project's own list, `false`, `net: true` and paranoid come first
    let own = JailPackageConfig {
      net: Some(JailNetConfig::Hosts(vec!["mirror.example.com".into()])),
      ..Default::default()
    };
    assert_eq!(
      net_of(jail_net(
        &config(false, false),
        Some(&own),
        "sharp",
        dir.path()
      )),
      (JailNet::Hosts(vec!["mirror.example.com".into()]), false)
    );
    let none = JailPackageConfig {
      net: Some(JailNetConfig::All(false)),
      ..Default::default()
    };
    assert_eq!(
      net_of(jail_net(
        &config(false, false),
        Some(&none),
        "sharp",
        dir.path()
      )),
      (JailNet::None, false)
    );
    assert_eq!(
      net_of(jail_net(&config(true, false), None, "sharp", dir.path())),
      (JailNet::All, false)
    );
    assert_eq!(
      net_of(jail_net(&config(false, true), None, "sharp", dir.path())),
      (JailNet::None, false)
    );
    // any other package: nothing new
    assert_eq!(
      jail_net(&config(false, false), None, "left-pad", dir.path()).net,
      JailNet::None
    );
  }

  // DENEXT.md item 288: a reviewed declared host is honoured without a
  // `knownPackages` entry
  #[test]
  fn builtin_known_declared_hosts() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
      dir.path().join("package.json"),
      r#"{ "binary": { "host": "https://node-precompiled-binaries.grpc.io/" } }"#,
    )
    .unwrap();
    assert_eq!(
      jail_net(&config(false, false), None, "grpc", dir.path()),
      NetChoice {
        net: JailNet::Hosts(vec![
          "node-precompiled-binaries.grpc.io:443".into()
        ]),
        declared: true,
        refused_declared: Vec::new(),
      }
    );
    // the same declaration from another package is still refused
    assert_eq!(
      jail_net(&config(false, false), None, "grpc-fork", dir.path())
        .refused_declared,
      vec!["node-precompiled-binaries.grpc.io".to_string()]
    );
  }

  #[test]
  fn native_failure_hints() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(false, false).with_settings_in_package_json(true);
    // NAN addon
    std::fs::write(
      dir.path().join("package.json"),
      r#"{ "dependencies": { "nan": "^2.0.0" } }"#,
    )
    .unwrap();
    let hint = config
      .native_failure_hint("re2", dir.path(), "install-from-cache")
      .unwrap();
    assert!(hint.contains("is a NAN addon"), "{hint}");
    // a built-in one: its hosts and the line for a mirror
    std::fs::write(dir.path().join("package.json"), "{}").unwrap();
    let hint = config
      .native_failure_hint(
        "better-sqlite3",
        dir.path(),
        "prebuild-install || node-gyp rebuild",
      )
      .unwrap();
    assert!(hint.contains("may reach github.com:443,"), "{hint}");
    assert!(hint.contains("node-gyp"), "{hint}");
    assert!(
      hint.contains(
        r#"add to package.json: "deno": { "supplyChain": { "jailScripts": { "packages": { "better-sqlite3": { "net": ["github.com:443", "#
      ),
      "{hint}"
    );
    assert!(hint.contains("built-in SQLite"), "{hint}");
    // an unknown native package
    std::fs::write(dir.path().join("binding.gyp"), "{}").unwrap();
    let hint = config
      .native_failure_hint("some-addon", dir.path(), "node-gyp rebuild")
      .unwrap();
    assert!(hint.contains("builds a native addon"), "{hint}");
    assert!(hint.contains(r#""some-addon": { "net": ["<download host>"] }"#));
    // nothing native
    std::fs::remove_file(dir.path().join("binding.gyp")).unwrap();
    assert_eq!(
      config.native_failure_hint("left-pad", dir.path(), "node build.js"),
      None
    );
  }

  // DENEXT.md item 124 (M3): secrets at any depth under the project
  #[cfg(unix)]
  #[test]
  fn secret_files_at_any_depth() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonicalize_path(dir.path()).unwrap();
    for file in [
      ".env",
      ".env.local",
      ".envrc",
      ".npmrc",
      "apps/web/.env.local",
      "apps/web/.dev.vars",
      "packages/x/.netrc",
      "packages/x/deep/er/.pypirc",
      "packages/x/.git-credentials",
      // not secrets, or not looked at
      "package.json",
      "apps/web/environment.ts",
      "node_modules/pkg/.env",
      ".git/.env",
    ] {
      let path = root.join(file);
      std::fs::create_dir_all(path.parent().unwrap()).unwrap();
      std::fs::write(&path, "x").unwrap();
    }
    // a symlink to a secret elsewhere is not followed (the target is
    // outside the project; a link inside node_modules is handled by the OS
    // layer's real-path checks)
    std::os::unix::fs::symlink(root.join(".env"), root.join("apps/.env.link"))
      .unwrap();
    let config = JailConfig::new(
      false,
      false,
      BTreeMap::new(),
      std::slice::from_ref(&root),
      None,
    );
    let mut found = config
      .project_secret_files()
      .iter()
      .map(|p| {
        p.strip_prefix(&root)
          .unwrap()
          .to_string_lossy()
          .into_owned()
      })
      .collect::<Vec<_>>();
    found.sort();
    assert_eq!(
      found,
      vec![
        ".env",
        ".env.local",
        ".envrc",
        ".npmrc",
        "apps/web/.dev.vars",
        "apps/web/.env.local",
        "packages/x/.git-credentials",
        "packages/x/.netrc",
        "packages/x/deep/er/.pypirc",
      ]
    );
    // the .npmrc the installer resolved is added, once
    let config = JailConfig::new(
      false,
      false,
      BTreeMap::new(),
      std::slice::from_ref(&root),
      Some(&root.join(".npmrc")),
    );
    assert_eq!(
      config
        .project_secret_files()
        .iter()
        .filter(|p| p.ends_with(".npmrc"))
        .count(),
      1
    );
  }

  // DENEXT.md item 124: credentials in URL-valued settings stay out
  #[test]
  fn url_credentials_stripped() {
    assert_eq!(
      strip_url_credentials("http://user:secret@corp:3128"),
      "http://corp:3128/"
    );
    assert_eq!(
      strip_url_credentials("https://user:apikey@artifactory.example/api/npm/"),
      "https://artifactory.example/api/npm/"
    );
    assert_eq!(
      strip_url_credentials("https://registry.npmjs.org/"),
      "https://registry.npmjs.org/"
    );
    assert_eq!(strip_url_credentials("true"), "true");
    assert_eq!(strip_url_credentials("corp:3128"), "corp:3128");
    for name in [
      "npm_config_registry",
      "npm_config_proxy",
      "HTTPS_PROXY",
      "all_proxy",
      "GLOBAL_AGENT_HTTP_PROXY",
    ] {
      assert!(url_valued_env_var(name), "{name}");
    }
    assert!(!url_valued_env_var("PATH"));
  }

  // DENEXT.md item 129: symlinks within the write roots only where an OS
  // sandbox confines the script, and never on Windows
  #[test]
  fn symlink_roots_only_when_confined() {
    let mut jail = JailPermissions {
      write: vec![PathBuf::from("/nm/pkg"), PathBuf::from("/nm/.jail/x")],
      ..Default::default()
    };
    assert_eq!(jail_symlink_roots(&jail), None);
    jail.os_confined = true;
    if cfg!(windows) {
      assert_eq!(jail_symlink_roots(&jail), None);
    } else {
      assert_eq!(jail_symlink_roots(&jail), Some(jail.write.as_slice()));
    }
    // no write roots: nothing
    assert_eq!(
      jail_symlink_roots(&JailPermissions {
        os_confined: true,
        ..Default::default()
      }),
      None
    );
    // the flag survives the trip through the environment, and an older
    // installer's JSON (without it) means unconfined
    let json = serde_json::to_string(&jail).unwrap();
    assert!(json.contains("\"osConfined\":true"), "{json}");
    let old: JailPermissions = serde_json::from_str(
      r#"{"write":["/nm"],"denyWrite":[],"denyRead":[],"net":"none"}"#,
    )
    .unwrap();
    assert!(!old.os_confined);
  }

  // DENEXT.md item 124: a path list with `..` can't pass the clamp
  #[test]
  fn clamp_folds_dotdot() {
    let jail = JailPermissions {
      write: vec![abs("/nm/pkg")],
      ..Default::default()
    };
    let mut options = PermissionsOptions {
      allow_write: Some(vec![
        "../../etc/does-not-exist".to_string(),
        abs_s("/nm/pkg/../other/x"),
        "out/../build".to_string(),
      ]),
      ..Default::default()
    };
    clamp_permissions(&mut options, &jail, &abs("/nm/pkg"));
    assert_eq!(options.allow_write, Some(vec!["out/../build".to_string()]));
  }

  // a Seatbelt profile's paths are text: a deny that isn't UTF-8 denies its
  // nearest UTF-8 ancestor, a write root that isn't is left out, and UTF-8
  // paths pass unchanged
  #[cfg(unix)]
  #[test]
  fn seatbelt_paths_never_lossy() {
    use std::os::unix::ffi::OsStrExt;
    let raw = |bytes: &[u8]| PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
    let (write, deny) = seatbelt_paths(
      &[raw(b"/nm/pkg"), raw(b"/nm/\xffbad")],
      &[
        raw(b"/home/u/.ssh"),
        raw(b"/p/sec\xfe/.env"),
        raw(b"/p/other\xfd"),
        raw(b"/q\xff/a/b"),
      ],
    );
    assert_eq!(write, vec![raw(b"/nm/pkg")]);
    assert_eq!(deny, vec![raw(b"/home/u/.ssh"), raw(b"/p"), raw(b"/")]);
    let profile = deno_sandbox::seatbelt::profile(&write, &deny, OsNet::Denied);
    assert!(!profile.contains('\u{fffd}'), "{profile}");
    assert!(profile.contains("(subpath \"/p\")"), "{profile}");
  }

  // a path that doesn't exist yet beneath a symlinked directory resolves
  // through the link, as the OS sandbox and Deno's checks see it: a grant
  // to it is kept, and one that leads out of the roots through a link is
  // not
  #[cfg(unix)]
  #[test]
  fn clamp_resolves_a_new_path_through_a_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let real = canonicalize_path(dir.path()).unwrap();
    let root = real.join("root");
    let outside = real.join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let link = real.join("link");
    std::os::unix::fs::symlink(&root, &link).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();
    assert_eq!(
      resolve_existing_prefix(&link.join("new/deeper")),
      root.join("new/deeper")
    );
    assert_eq!(
      resolve_existing_prefix(&link.join("a/../b")),
      root.join("b")
    );
    // an existing link followed by `..`: the OS's answer (the link's
    // target's parent), not the lexical one
    assert_eq!(
      resolve_existing_prefix(&root.join("out/../x")),
      real.join("x")
    );
    // a `..` after a part that doesn't exist yet lands on the link
    assert_eq!(
      resolve_existing_prefix(&root.join("new/../out/y")),
      outside.join("y")
    );
    let jail = JailPermissions {
      write: vec![root.clone()],
      ..Default::default()
    };
    let new_under_link = link.join("new").to_string_lossy().into_owned();
    let out_through_link = root.join("out/new").to_string_lossy().into_owned();
    let out_after_dotdot =
      root.join("gone/../out/z").to_string_lossy().into_owned();
    let mut options = PermissionsOptions {
      allow_write: Some(vec![
        new_under_link.clone(),
        out_through_link,
        out_after_dotdot,
      ]),
      ..Default::default()
    };
    clamp_permissions(&mut options, &jail, &real);
    assert_eq!(options.allow_write, Some(vec![new_under_link]));
  }

  #[tokio::test]
  async fn read_capped_keeps_the_head_and_drains_the_rest() {
    // under the cap: everything, no marker
    let mut small: &[u8] = b"hello\n";
    assert_eq!(read_capped(&mut small, 16).await, b"hello\n");
    // exactly the cap: nothing dropped, no marker
    let mut exact: &[u8] = b"0123456789";
    assert_eq!(read_capped(&mut exact, 10).await, b"0123456789");
    // over the cap, across many reads: the head, then the marker, and the
    // reader is drained to EOF
    let data = vec![b'x'; 300 * 1024];
    let mut big: &[u8] = &data;
    let out = read_capped(&mut big, 100 * 1024).await;
    let mut expected = vec![b'x'; 100 * 1024];
    expected.extend_from_slice(b"\n[output truncated after 102400 bytes]\n");
    assert_eq!(out, expected);
    assert!(big.is_empty());
    // a writer blocked on a full pipe is unblocked by the drain
    let (mut writer, mut reader) = tokio::io::duplex(1024);
    let write = tokio::spawn(async move {
      for _ in 0..64 {
        writer.write_all(&[b'y'; 1024]).await.unwrap();
      }
    });
    let out = read_capped(&mut reader, 10).await;
    write.await.unwrap();
    assert!(out.starts_with(b"yyyyyyyyyy\n[output truncated after 10 bytes]"));
  }

  // DENEXT.md item 124 (H2, M6): the script's process is a session leader
  // and its whole group is ended, a detached grandchild included
  #[cfg(unix)]
  #[tokio::test]
  async fn detached_session_and_group_ended() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("grandchild.pid");
    let mut command = tokio::process::Command::new("/bin/sh");
    command
      .arg("-c")
      .arg(format!(
        "nohup sleep 300 >/dev/null 2>&1 & echo $! > '{}'; exec sleep 300",
        pid_file.display()
      ))
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .kill_on_drop(true);
    detach_session(&mut command);
    let mut child = command.spawn().unwrap();
    let pid = child.id().unwrap();
    // its own session and group
    // SAFETY: pid lookups
    let (sid, pgid) = unsafe {
      (
        libc::getsid(pid as libc::pid_t),
        libc::getpgid(pid as libc::pid_t),
      )
    };
    assert_eq!(sid as u32, pid);
    assert_eq!(pgid as u32, pid);
    // SAFETY: our own session
    assert_ne!(sid, unsafe { libc::getsid(0) });
    let grandchild = loop {
      if let Ok(text) = std::fs::read_to_string(&pid_file)
        && let Ok(pid) = text.trim().parse::<libc::pid_t>()
      {
        break pid;
      }
      tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    // SAFETY: a probe
    assert_eq!(unsafe { libc::kill(grandchild, 0) }, 0);
    kill_process_group(pid);
    child.wait().await.unwrap();
    end_process_group(pid).await;
    // the grandchild (nohup, in the group) is gone too
    let gone = (0..200).any(|_| {
      // SAFETY: a probe; a reparented zombie is reaped by init shortly
      if unsafe { libc::kill(grandchild, 0) } != 0 {
        return true;
      }
      std::thread::sleep(std::time::Duration::from_millis(10));
      false
    });
    assert!(gone, "the grandchild {grandchild} outlived the group");
  }

  // DENEXT.md item 111: what the OS lets a script connect to
  #[test]
  fn os_net_choice() {
    let hosts = JailNet::Hosts(vec!["example.com".to_string()]);
    assert_eq!(
      choose_os_net(&JailNet::None, false, false, 9, true),
      OsNet::Denied
    );
    assert_eq!(
      choose_os_net(&JailNet::All, false, true, 9, false),
      OsNet::Open
    );
    // a host list goes through the proxy where the OS can hold it there
    for (declared, paranoid) in
      [(false, false), (true, false), (false, true), (true, true)]
    {
      assert_eq!(
        choose_os_net(&hosts, declared, paranoid, 9, true),
        OsNet::ProxyOnly(9)
      );
    }
    // where it can't: closed for the package's own hosts and under paranoid,
    // open (with a warning) for the project's list
    assert_eq!(choose_os_net(&hosts, true, false, 9, false), OsNet::Denied);
    assert_eq!(choose_os_net(&hosts, false, true, 9, false), OsNet::Denied);
    assert_eq!(choose_os_net(&hosts, false, false, 9, false), OsNet::Open);
  }

  #[test]
  fn proxy_env() {
    let mut env = HashMap::new();
    for (k, v) in [
      ("NO_PROXY", "*"),
      ("no_proxy", "*"),
      ("npm_config_noproxy", "*"),
      ("npm_config_proxy", "http://user:secret@corp:3128"),
      ("PATH", "/bin"),
    ] {
      env.insert(OsString::from(k), OsString::from(v));
    }
    set_proxy_env(&mut env, "http://127.0.0.1:9");
    let get = |k: &str| {
      env
        .iter()
        .find(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case(k))
        .map(|(_, v)| v.to_string_lossy().into_owned())
    };
    for key in ["HTTP_PROXY", "HTTPS_PROXY", "npm_config_proxy"] {
      assert_eq!(get(key).as_deref(), Some("http://127.0.0.1:9"), "{key}");
    }
    assert_eq!(get("NODE_USE_ENV_PROXY").as_deref(), Some("1"));
    assert_eq!(get("NO_PROXY"), None);
    assert_eq!(get("npm_config_noproxy"), None);
    assert_eq!(get("PATH").as_deref(), Some("/bin"));
  }

  #[cfg(unix)]
  #[test]
  fn new_executables_removed() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("cache");
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir_all(cache.join("chrome")).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    let old = cache.join("old-tool");
    std::fs::write(&old, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755))
      .unwrap();
    let since = SystemTime::now() + std::time::Duration::from_secs(1);
    // what the script leaves (ctime is in whole seconds)
    std::thread::sleep(std::time::Duration::from_millis(2100));
    let binary = cache.join("chrome/chrome");
    std::fs::write(&binary, "elf").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
      .unwrap();
    let data = cache.join("chrome/data.pak");
    std::fs::write(&data, "data").unwrap();
    let payload = elsewhere.join("payload");
    std::fs::write(&payload, "x").unwrap();
    std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o700))
      .unwrap();
    let link = cache.join("run-me");
    std::os::unix::fs::symlink(&payload, &link).unwrap();
    let data_link = cache.join("data-link");
    std::os::unix::fs::symlink(&data, &data_link).unwrap();
    let mut removed =
      remove_new_executables(std::slice::from_ref(&cache), since);
    removed.sort();
    assert_eq!(removed, vec![binary.clone(), link.clone()]);
    assert!(!binary.exists() && std::fs::symlink_metadata(&link).is_err());
    // the link's target, the data, and what was there before stay
    assert!(payload.exists() && data.exists() && data_link.exists());
    assert!(old.exists());
  }

  #[cfg(unix)]
  #[test]
  fn home_paths() {
    let package = JailPackageConfig {
      write: vec![
        PathBuf::from("~/.cache/puppeteer"),
        PathBuf::from("/abs/cache"),
      ],
      ..Default::default()
    };
    let home = Path::new("/home/u");
    assert_eq!(
      home_write_paths(&package, Some(home)),
      vec![home.join(".cache/puppeteer")]
    );
    assert!(home_write_paths(&package, None).is_empty());
  }

  // DENEXT.md item 108: a download cache outside node_modules
  #[test]
  fn package_write() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let package = JailPackageConfig {
      write: vec![
        PathBuf::from("~/.cache/puppeteer"),
        PathBuf::from("~"),
        dir.path().join("project/.cache/ms-playwright"),
      ],
      ..Default::default()
    };
    let paths = package_write_paths(&package, Some(&home));
    assert_eq!(
      paths,
      vec![
        home.join(".cache/puppeteer"),
        home.clone(),
        dir.path().join("project/.cache/ms-playwright"),
      ]
    );
    // created, so the OS sandbox can grant them
    for path in &paths {
      assert!(path.is_dir(), "{}", path.display());
    }
    // no home: a `~` path is dropped, never widened
    assert_eq!(
      package_write_paths(&package, None),
      vec![dir.path().join("project/.cache/ms-playwright")]
    );
  }

  #[cfg(unix)]
  #[test]
  fn links_leading_outside_node_modules() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonicalize_path(dir.path()).unwrap();
    let nm = root.join("node_modules");
    let member = root.join("member");
    std::fs::create_dir_all(nm.join(".deno/a@1.0.0/node_modules/a")).unwrap();
    std::fs::create_dir_all(nm.join("@scope")).unwrap();
    std::fs::create_dir_all(&member).unwrap();
    // inside node_modules: fine
    std::os::unix::fs::symlink(
      nm.join(".deno/a@1.0.0/node_modules/a"),
      nm.join("a"),
    )
    .unwrap();
    // outside: denied
    std::os::unix::fs::symlink(&member, nm.join("member")).unwrap();
    std::os::unix::fs::symlink(&member, nm.join("@scope/member")).unwrap();
    std::os::unix::fs::symlink(&root, nm.join(".deno/a@1.0.0/node_modules/up"))
      .unwrap();
    let mut links = outside_links(&nm, std::slice::from_ref(&nm));
    links.sort();
    assert_eq!(links, vec![root.clone(), member]);
  }

  #[test]
  fn clamp() {
    let jail = JailPermissions {
      write: vec![abs("/nm")],
      deny_write: vec![abs("/ws/member")],
      deny_read: vec![abs("/home/u/.ssh")],
      net: JailNet::None,
      proxy: None,
      ffi: vec![],
      os_confined: false,
    };
    let mut options = PermissionsOptions {
      allow_write: Some(vec![]),
      allow_net: Some(vec![]),
      allow_ffi: Some(vec![]),
      allow_read: Some(vec![]),
      prompt: true,
      ..Default::default()
    };
    clamp_permissions(&mut options, &jail, &abs("/nm/pkg"));
    assert_eq!(options.allow_write, Some(vec![abs_s("/nm")]));
    assert_eq!(options.allow_net, None);
    assert_eq!(options.allow_ffi, None);
    assert_eq!(options.allow_read, Some(vec![]));
    assert_eq!(options.deny_write, Some(vec![abs_s("/ws/member")]));
    assert_eq!(options.deny_read, Some(vec![abs_s("/home/u/.ssh")]));
    assert_eq!(options.deny_import, Some(vec![]));
    assert!(!options.prompt);

    // a write list outside the roots is dropped, never widened to "all"
    let mut options = PermissionsOptions {
      allow_write: Some(vec![abs_s("/etc")]),
      ..Default::default()
    };
    clamp_permissions(&mut options, &jail, &abs("/nm/pkg"));
    assert_eq!(options.allow_write, None);
    let mut options = PermissionsOptions {
      allow_write: Some(vec!["out".to_string(), abs_s("/etc")]),
      ..Default::default()
    };
    clamp_permissions(&mut options, &jail, &abs("/nm/pkg"));
    assert_eq!(options.allow_write, Some(vec!["out".to_string()]));

    let jail = JailPermissions {
      net: JailNet::Hosts(vec!["example.com".into()]),
      ..jail
    };
    let mut options = PermissionsOptions {
      allow_net: Some(vec![]),
      ..Default::default()
    };
    clamp_permissions(&mut options, &jail, &abs("/nm/pkg"));
    assert_eq!(options.allow_net, Some(vec!["example.com".to_string()]));
    assert_eq!(options.deny_import, None);

    // DENEXT.md item 111: a deno may also connect to the script's proxy
    let proxied = JailPermissions {
      proxy: Some("127.0.0.1:9".to_string()),
      ..jail.clone()
    };
    for list in [vec![], vec!["example.com".to_string()]] {
      let mut options = PermissionsOptions {
        allow_net: Some(list),
        ..Default::default()
      };
      clamp_permissions(&mut options, &proxied, &abs("/nm/pkg"));
      assert_eq!(
        options.allow_net,
        Some(vec!["example.com".to_string(), "127.0.0.1:9".to_string()])
      );
    }
    let mut options = PermissionsOptions {
      allow_net: Some(vec!["evil.com".to_string()]),
      ..Default::default()
    };
    clamp_permissions(&mut options, &proxied, &abs("/nm/pkg"));
    assert_eq!(options.allow_net, None);

    // the direction-specific grants are clamped like `allow_net`
    let mut options = PermissionsOptions {
      allow_net_connect: Some(vec![]),
      allow_net_listen: Some(vec![
        "example.com".to_string(),
        "evil.com".to_string(),
      ]),
      ..Default::default()
    };
    clamp_permissions(&mut options, &jail, &abs("/nm/pkg"));
    assert_eq!(
      options.allow_net_connect,
      Some(vec!["example.com".to_string()])
    );
    assert_eq!(
      options.allow_net_listen,
      Some(vec!["example.com".to_string()])
    );
    let jail = JailPermissions {
      net: JailNet::None,
      ..jail
    };
    let mut options = PermissionsOptions {
      allow_net_connect: Some(vec![]),
      allow_net_listen: Some(vec![]),
      ..Default::default()
    };
    clamp_permissions(&mut options, &jail, &abs("/nm/pkg"));
    assert_eq!(options.allow_net_connect, None);
    assert_eq!(options.allow_net_listen, None);
  }

  #[test]
  fn clamp_ffi_to_package_folder() {
    let jail = JailPermissions {
      write: vec![abs("/nm/pkg")],
      ffi: vec![abs("/nm/pkg")],
      ..Default::default()
    };
    // `-A` (the node shim's) loads native addons from the package only
    let mut options = PermissionsOptions {
      allow_ffi: Some(vec![]),
      ..Default::default()
    };
    clamp_permissions(&mut options, &jail, &abs("/nm/pkg"));
    assert_eq!(options.allow_ffi, Some(vec![abs_s("/nm/pkg")]));
    // a path list keeps what lies inside it, never widened to "all"
    let mut options = PermissionsOptions {
      allow_ffi: Some(vec![
        "prebuilds/addon.node".to_string(),
        abs_s("/usr/lib/libc.so"),
      ]),
      ..Default::default()
    };
    clamp_permissions(&mut options, &jail, &abs("/nm/pkg"));
    assert_eq!(
      options.allow_ffi,
      Some(vec!["prebuilds/addon.node".to_string()])
    );
    let mut options = PermissionsOptions {
      allow_ffi: Some(vec![abs_s("/usr/lib/libc.so")]),
      ..Default::default()
    };
    clamp_permissions(&mut options, &jail, &abs("/nm/pkg"));
    assert_eq!(options.allow_ffi, None);
    // `..` can't climb out of the package
    let mut options = PermissionsOptions {
      allow_ffi: Some(vec!["prebuilds/../../other/addon.node".to_string()]),
      ..Default::default()
    };
    clamp_permissions(&mut options, &jail, &abs("/nm/pkg"));
    assert_eq!(options.allow_ffi, None);
    // not asked for, not granted
    let mut options = PermissionsOptions::default();
    clamp_permissions(&mut options, &jail, &abs("/nm/pkg"));
    assert_eq!(options.allow_ffi, None);
    // no FFI roots (no OS sandbox): `-A` gets no native code
    let unconfined = JailPermissions {
      write: vec![abs("/nm/pkg")],
      ..Default::default()
    };
    let mut options = PermissionsOptions {
      allow_ffi: Some(vec![]),
      ..Default::default()
    };
    clamp_permissions(&mut options, &unconfined, &abs("/nm/pkg"));
    assert_eq!(options.allow_ffi, None);
  }

  #[test]
  fn supply_chain_line_names_the_home() {
    let config = JailConfig::new(false, false, BTreeMap::new(), &[], None);
    assert_eq!(
      config.supply_chain_line("{}"),
      "add to deno.json: \"supplyChain\": {}"
    );
    assert_eq!(config.settings_file(), "deno.json");
    let config = config.with_settings_in_package_json(true);
    assert_eq!(
      config.supply_chain_line("{}"),
      "add to package.json: \"deno\": { \"supplyChain\": {} }"
    );
    assert_eq!(config.settings_file(), "package.json");
  }

  #[test]
  fn ffi_roots_only_under_an_os_sandbox() {
    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("pkg");
    std::fs::create_dir(&package).unwrap();
    let roots = package_ffi_roots(&package, true);
    assert!(roots.contains(&package));
    assert!(roots.contains(&canonicalize_path(&package).unwrap()));
    assert_eq!(package_ffi_roots(&package, false), Vec::<PathBuf>::new());
  }
}
