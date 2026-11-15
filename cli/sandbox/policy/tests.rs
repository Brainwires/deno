// Copyright 2018-2026 the Deno authors. MIT license.

//! Table-driven tests of the policy model: one row per mapping of plans/
//! kernel-enforcement.md §1.1–§1.5 (as amended by the review), flags →
//! policy + gaps.

use std::path::Path;
use std::path::PathBuf;

use deno_core::serde_json;
use deno_lib::kernel_sandbox::KernelSandboxMode;
use deno_runtime::deno_permissions::PermissionsOptions;
use pretty_assertions::assert_eq;

use super::*;

fn linux(abi: Option<u32>) -> Capabilities {
  Capabilities {
    os: TargetOs::Linux,
    landlock_abi: abi,
    seccomp: true,
    user_namespaces: false,
    seatbelt: false,
    job_object: false,
    low_integrity: false,
    enforcer_built: true,
  }
}

fn linux_userns(abi: u32) -> Capabilities {
  Capabilities {
    user_namespaces: true,
    ..linux(Some(abi))
  }
}

fn macos() -> Capabilities {
  Capabilities {
    seatbelt: true,
    ..Capabilities::none(TargetOs::Macos)
  }
  .built()
}

fn windows() -> Capabilities {
  Capabilities {
    job_object: true,
    low_integrity: true,
    ..Capabilities::none(TargetOs::Windows)
  }
  .built()
}

trait Built {
  fn built(self) -> Self;
}

impl Built for Capabilities {
  fn built(mut self) -> Self {
    self.enforcer_built = true;
    self
  }
}

fn s(list: &[&str]) -> Option<Vec<String>> {
  Some(list.iter().map(|s| s.to_string()).collect())
}

fn all() -> Option<Vec<String>> {
  Some(Vec::new())
}

fn allow_all() -> PermissionsOptions {
  PermissionsOptions {
    allow_read: all(),
    allow_write: all(),
    allow_net: all(),
    allow_env: all(),
    allow_run: all(),
    allow_ffi: all(),
    allow_sys: all(),
    allow_import: all(),
    ..Default::default()
  }
}

fn ctx(caps: Capabilities, mode: KernelSandboxMode) -> Context {
  Context::new(caps, mode, PathBuf::from("/project"))
}

fn compile_in(options: &PermissionsOptions, ctx: &Context) -> CompiledPolicy {
  let implicit = ImplicitSet::build(options, ctx);
  KernelPolicy::compile(options, &implicit, ctx)
}

fn compile(
  options: &PermissionsOptions,
  caps: Capabilities,
  mode: KernelSandboxMode,
) -> CompiledPolicy {
  compile_in(options, &ctx(caps, mode))
}

fn auto(options: &PermissionsOptions, caps: Capabilities) -> CompiledPolicy {
  compile(options, caps, KernelSandboxMode::Auto)
}

fn grants(scope: &PathScope) -> Vec<PathBuf> {
  match scope {
    PathScope::All => vec![PathBuf::from("*")],
    PathScope::Paths { paths } => paths
      .iter()
      .filter(|r| r.implicit.is_none())
      .map(|r| r.path.clone())
      .collect(),
  }
}

fn has_gap(
  c: &CompiledPolicy,
  class: GapClass,
  resolution: GapResolution,
) -> bool {
  c.gaps
    .iter()
    .any(|g| g.class == class && g.resolution == resolution)
}

fn gap(c: &CompiledPolicy, class: GapClass) -> &PolicyGap {
  c.gaps
    .iter()
    .find(|g| g.class == class)
    .unwrap_or_else(|| panic!("no {class:?} gap in {:#?}", c.gaps))
}

fn deny(rules: &[DenyRule], path: &str) -> DenyEnforcement {
  rules
    .iter()
    .find(|r| r.path == Path::new(path))
    .unwrap_or_else(|| panic!("no deny for {path}"))
    .enforcement
}

/// One row: a name, the options, the machine, the mode, and the checks.
struct Row {
  name: &'static str,
  options: PermissionsOptions,
  caps: Capabilities,
  mode: KernelSandboxMode,
  check: fn(&CompiledPolicy),
}

fn run_rows(rows: Vec<Row>) {
  for row in rows {
    let compiled = compile(&row.options, row.caps, row.mode);
    let result = std::panic::catch_unwind(|| (row.check)(&compiled));
    if let Err(err) = result {
      panic!(
        "row '{}' failed: {:?}\npolicy: {:#?}\ngaps: {:#?}",
        row.name,
        err.downcast_ref::<String>(),
        compiled.policy,
        compiled.gaps
      );
    }
  }
}

use KernelSandboxMode::Auto;
use KernelSandboxMode::Broker;
use KernelSandboxMode::Enforce;

/// §1.1: read and write path scopes, and their denies.
#[test]
fn fs_rows() {
  run_rows(vec![
    Row {
      name: "no read grant: only the implicit set",
      options: PermissionsOptions::default(),
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert_eq!(grants(&c.policy.fs.read), Vec::<PathBuf>::new());
        assert!(c.policy.fs.read.covers(Path::new("/usr/lib/libc.so.6")));
        assert!(!c.policy.fs.read.covers(Path::new("/home/me/.ssh/id")));
      },
    },
    Row {
      name: "--allow-read=P: P plus the implicit set",
      options: PermissionsOptions {
        allow_read: s(&["/data", "rel"]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert_eq!(
          grants(&c.policy.fs.read),
          vec![PathBuf::from("/data"), PathBuf::from("/project/rel")]
        );
      },
    },
    Row {
      name: "unscoped --allow-read: all",
      options: PermissionsOptions {
        allow_read: all(),
        ..Default::default()
      },
      caps: macos(),
      mode: Auto,
      check: |c| assert!(c.policy.fs.read.is_all()),
    },
    Row {
      name: "--allow-write=P: P plus writable devices",
      options: PermissionsOptions {
        allow_write: s(&["/out"]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert_eq!(grants(&c.policy.fs.write), vec![PathBuf::from("/out")]);
        assert!(c.policy.fs.write.covers(Path::new("/dev/null")));
        assert!(!c.policy.fs.write.covers(Path::new("/etc/passwd")));
      },
    },
    Row {
      name: "macOS: a read deny inside an allow is exact",
      options: PermissionsOptions {
        allow_read: s(&["/project"]),
        deny_read: s(&["/project/.env"]),
        allow_write: s(&["/project"]),
        ..Default::default()
      },
      caps: macos(),
      mode: Auto,
      check: |c| {
        assert_eq!(
          deny(&c.policy.fs.deny_read, "/project/.env"),
          DenyEnforcement::Exact
        );
        assert!(!c.gaps.iter().any(|g| g.rule.contains(".env")));
      },
    },
    Row {
      name: "Linux: a stable read deny is held by expansion",
      options: PermissionsOptions {
        allow_read: s(&["/project"]),
        deny_read: s(&["/project/secrets/key"]),
        ..Default::default()
      },
      caps: linux(Some(4)),
      mode: Auto,
      check: |c| {
        assert_eq!(
          deny(&c.policy.fs.deny_read, "/project/secrets/key"),
          DenyEnforcement::Expand
        );
      },
    },
    Row {
      name: "Linux auto: an unstable read deny is Deno-only (review F7)",
      options: PermissionsOptions {
        allow_read: s(&["/project"]),
        allow_write: s(&["/project"]),
        deny_read: s(&["/project/.env"]),
        ..Default::default()
      },
      caps: linux(Some(4)),
      mode: Auto,
      check: |c| {
        assert_eq!(
          deny(&c.policy.fs.deny_read, "/project/.env"),
          DenyEnforcement::DenoOnly
        );
        let g = c
          .gaps
          .iter()
          .find(|g| g.rule == "--deny-read=/project/.env")
          .unwrap();
        assert_eq!(g.resolution, GapResolution::FailOpen);
        assert_eq!(g.exact_with, Some(ExactWith::UserNamespaces));
      },
    },
    Row {
      name: "Linux with user namespaces: overmount",
      options: PermissionsOptions {
        allow_read: s(&["/project"]),
        allow_write: s(&["/project"]),
        deny_read: s(&["/project/.env"]),
        ..Default::default()
      },
      caps: linux_userns(4),
      mode: Auto,
      check: |c| {
        assert_eq!(
          deny(&c.policy.fs.deny_read, "/project/.env"),
          DenyEnforcement::Overmount
        );
      },
    },
    Row {
      name: "Linux enforce: an unstable read deny is expanded, documented",
      options: PermissionsOptions {
        allow_read: s(&["/project"]),
        allow_write: s(&["/project"]),
        deny_read: s(&["/project/.env"]),
        ..Default::default()
      },
      caps: linux(Some(4)),
      mode: Enforce,
      check: |c| {
        assert_eq!(
          deny(&c.policy.fs.deny_read, "/project/.env"),
          DenyEnforcement::Expand
        );
        let g = c
          .gaps
          .iter()
          .find(|g| g.rule == "--deny-read=/project/.env")
          .unwrap();
        assert_eq!(g.resolution, GapResolution::Documented);
      },
    },
    Row {
      name: "a deny outside every allow is implied",
      options: PermissionsOptions {
        allow_read: s(&["/data"]),
        deny_read: s(&["/elsewhere"]),
        ..Default::default()
      },
      caps: linux(Some(4)),
      mode: Auto,
      check: |c| {
        assert_eq!(
          deny(&c.policy.fs.deny_read, "/elsewhere"),
          DenyEnforcement::Implied
        );
      },
    },
    Row {
      name: "--ignore-read is a deny; the kernel can't fake NotFound",
      options: PermissionsOptions {
        allow_read: s(&["/data"]),
        ignore_read: s(&["/data/x"]),
        ..Default::default()
      },
      caps: macos(),
      mode: Auto,
      check: |c| {
        let rule = &c.policy.fs.deny_read[0];
        assert!(rule.ignore);
        assert_eq!(rule.enforcement, DenyEnforcement::Exact);
        assert!(has_gap(c, GapClass::Read, GapResolution::Documented));
      },
    },
    Row {
      name: "Linux -A --deny-read: deny-only, Deno-only in auto (D2 as reviewed)",
      options: PermissionsOptions {
        deny_read: s(&["/home/me/.ssh"]),
        ..allow_all()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert!(c.policy.deny_only);
        assert!(!c.policy.syscalls.attack_surface);
        assert!(c.policy.fs.read.is_all());
        assert_eq!(
          deny(&c.policy.fs.deny_read, "/home/me/.ssh"),
          DenyEnforcement::DenoOnly
        );
      },
    },
    Row {
      name: "macOS -A --deny-read: exact",
      options: PermissionsOptions {
        deny_read: s(&["/Users/me/.ssh"]),
        ..allow_all()
      },
      caps: macos(),
      mode: Auto,
      check: |c| {
        assert!(c.policy.deny_only);
        assert_eq!(
          deny(&c.policy.fs.deny_read, "/Users/me/.ssh"),
          DenyEnforcement::Exact
        );
      },
    },
    Row {
      name: "Linux auto: a write deny inside an allow is Deno-only, exact with the broker",
      options: PermissionsOptions {
        allow_write: s(&["/out"]),
        deny_write: s(&["/out/keep"]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert_eq!(c.policy.shape, Shape::ModeA);
        assert_eq!(
          deny(&c.policy.fs.deny_write, "/out/keep"),
          DenyEnforcement::DenoOnly
        );
        let g = gap(c, GapClass::Write);
        assert_eq!(g.resolution, GapResolution::FailOpen);
        assert_eq!(g.exact_with, Some(ExactWith::Broker));
      },
    },
    Row {
      name: "Linux broker: a write deny is held by the broker",
      options: PermissionsOptions {
        allow_write: s(&["/out"]),
        deny_write: s(&["/out/keep"]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Broker,
      check: |c| {
        assert_eq!(c.policy.shape, Shape::ModeB);
        assert_eq!(
          deny(&c.policy.fs.deny_write, "/out/keep"),
          DenyEnforcement::Broker
        );
        assert_eq!(c.fatal_gaps().count(), 0);
      },
    },
    Row {
      name: "Linux enforce: the broker is chosen to hold a write deny",
      options: PermissionsOptions {
        allow_write: s(&["/out"]),
        deny_write: s(&["/out/keep"]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Enforce,
      check: |c| {
        assert_eq!(c.policy.shape, Shape::ModeB);
        assert_eq!(
          deny(&c.policy.fs.deny_write, "/out/keep"),
          DenyEnforcement::Broker
        );
      },
    },
    Row {
      name: "macOS: a write deny is exact",
      options: PermissionsOptions {
        allow_write: s(&["/out"]),
        deny_write: s(&["/out/keep"]),
        ..Default::default()
      },
      caps: macos(),
      mode: Auto,
      check: |c| {
        assert_eq!(
          deny(&c.policy.fs.deny_write, "/out/keep"),
          DenyEnforcement::Exact
        );
      },
    },
    Row {
      name: "Linux before REFER: cross-folder renames fail closed; before TRUNCATE: open",
      options: PermissionsOptions {
        allow_write: s(&["/out"]),
        ..Default::default()
      },
      caps: linux(Some(1)),
      mode: Auto,
      check: |c| {
        assert!(c.gaps.iter().any(|g| g.class == GapClass::Write
          && g.resolution == GapResolution::FailClosed
          && g.exact_with == Some(ExactWith::LandlockAbi { abi: 2 })));
        assert!(c.gaps.iter().any(|g| g.class == GapClass::Write
          && g.resolution == GapResolution::FailOpen
          && g.exact_with == Some(ExactWith::LandlockAbi { abi: 3 })));
      },
    },
    Row {
      name: "Linux without Landlock: reads and writes are Deno-only",
      options: PermissionsOptions {
        allow_read: s(&["/data"]),
        allow_write: s(&["/out"]),
        ..Default::default()
      },
      caps: linux(None),
      mode: Auto,
      check: |c| {
        assert!(has_gap(c, GapClass::Read, GapResolution::FailOpen));
        assert!(has_gap(c, GapClass::Write, GapResolution::FailOpen));
      },
    },
    Row {
      name: "an implicit path inside a user's deny is kept and reported",
      options: PermissionsOptions {
        allow_read: s(&["/"]),
        deny_read: s(&["/etc"]),
        ..Default::default()
      },
      caps: macos(),
      mode: Auto,
      check: |c| {
        // nothing implicit under /etc on macOS (/private/etc is the real path)
        assert!(!c.gaps.iter().any(|g| g.rule == "--deny-read=/etc"
          && g.resolution == GapResolution::FailOpen));
      },
    },
    Row {
      name: "Linux: an implicit path inside a user's deny is reported fail-open",
      options: PermissionsOptions {
        allow_read: s(&["/"]),
        deny_read: s(&["/etc"]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert!(c.gaps.iter().any(|g| g.rule == "--deny-read=/etc"
          && g.resolution == GapResolution::FailOpen
          && g.detail.contains("/etc/passwd")));
      },
    },
  ]);
}

/// §1.2: network, as amended (review F2: names are a broker feature on
/// Linux; ports only for addresses and localhost).
#[test]
fn net_rows() {
  run_rows(vec![
    Row {
      name: "no net: denied, listen refused",
      options: PermissionsOptions::default(),
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert_eq!(c.policy.net, NetPolicy::Denied);
        assert!(c.policy.syscalls.refuse_listen);
      },
    },
    Row {
      name: "unscoped --allow-net: open",
      options: PermissionsOptions {
        allow_net: all(),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert_eq!(c.policy.net, NetPolicy::Open);
        assert!(!c.policy.syscalls.refuse_listen);
      },
    },
    Row {
      name: "Linux ABI 4: address:port is held by port, coarser",
      options: PermissionsOptions {
        allow_net: s(&["127.0.0.1:5432", "localhost:6379"]),
        ..Default::default()
      },
      caps: linux(Some(4)),
      mode: Auto,
      check: |c| {
        assert_eq!(
          c.policy.net,
          NetPolicy::Ports {
            connect: PortScope::Ports {
              ports: vec![5432, 6379]
            },
            bind: PortScope::Ports {
              ports: vec![5432, 6379]
            },
          }
        );
        let g = gap(c, GapClass::Net);
        assert_eq!(g.resolution, GapResolution::FailOpen);
        assert!(g.detail.contains("169.254.169.254"));
      },
    },
    Row {
      name: "Linux before ABI 4: a port list is Deno-only",
      options: PermissionsOptions {
        allow_net: s(&["127.0.0.1:5432"]),
        ..Default::default()
      },
      caps: linux(Some(2)),
      mode: Auto,
      check: |c| {
        assert!(
          c.gaps
            .iter()
            .any(|g| g.class == GapClass::Net && g.detail.contains("ABI 4"))
        );
      },
    },
    Row {
      name: "Linux: a host name goes to the broker from the start",
      options: PermissionsOptions {
        allow_net: s(&["example.com:443"]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert_eq!(c.policy.shape, Shape::ModeB);
        assert_eq!(
          c.policy.net,
          NetPolicy::Broker {
            connect: HostScope::Hosts {
              hosts: vec!["example.com:443".to_string()]
            },
            bind: HostScope::Hosts {
              hosts: vec!["example.com:443".to_string()]
            },
            deny: vec![],
          }
        );
        assert!(!c.gaps.iter().any(|g| g.class == GapClass::Net));
      },
    },
    Row {
      name: "macOS: a name with a port is held by port, hosts Deno-only",
      options: PermissionsOptions {
        allow_net_connect: s(&["example.com:443"]),
        ..Default::default()
      },
      caps: macos(),
      mode: Auto,
      check: |c| {
        assert_eq!(
          c.policy.net,
          NetPolicy::Ports {
            connect: PortScope::Ports { ports: vec![443] },
            bind: PortScope::None,
          }
        );
        assert!(c.policy.syscalls.refuse_listen);
        let g = gap(c, GapClass::Net);
        assert!(g.detail.contains("not the hosts"));
        assert_eq!(g.exact_with, Some(ExactWith::Broker));
      },
    },
    Row {
      name: "macOS: a name without a port needs the broker",
      options: PermissionsOptions {
        allow_net: s(&["example.com"]),
        ..Default::default()
      },
      caps: macos(),
      mode: Auto,
      check: |c| {
        assert!(matches!(c.policy.net, NetPolicy::Broker { .. }));
      },
    },
    Row {
      name: "--allow-net-listen=:8000 binds 8000 only",
      options: PermissionsOptions {
        allow_net_listen: s(&[
          "0.0.0.0:8000",
          "127.0.0.1:8000",
          "localhost:8000",
        ]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert_eq!(
          c.policy.net,
          NetPolicy::Ports {
            connect: PortScope::None,
            bind: PortScope::Ports { ports: vec![8000] },
          }
        );
        assert!(!c.policy.syscalls.refuse_listen);
      },
    },
    Row {
      name: "a deny under an open network is Deno-only",
      options: PermissionsOptions {
        allow_net: all(),
        deny_net: s(&["evil.example"]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert_eq!(c.policy.net, NetPolicy::Open);
        let g = gap(c, GapClass::Net);
        assert_eq!(g.rule, "--deny-net=evil.example");
        assert_eq!(g.exact_with, Some(ExactWith::Broker));
      },
    },
    Row {
      name: "broker: denies are held",
      options: PermissionsOptions {
        allow_net: all(),
        deny_net: s(&["evil.example"]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Broker,
      check: |c| {
        assert_eq!(
          c.policy.net,
          NetPolicy::Broker {
            connect: HostScope::Any,
            bind: HostScope::Any,
            deny: vec!["evil.example".to_string()],
          }
        );
      },
    },
    Row {
      name: "Windows: the network is Deno-only, documented",
      options: PermissionsOptions {
        allow_net: s(&["example.com:443"]),
        ..Default::default()
      },
      caps: windows(),
      mode: Auto,
      check: |c| {
        assert!(has_gap(c, GapClass::Net, GapResolution::Documented));
      },
    },
    Row {
      name: "a narrowed network makes uncached remote imports fail closed",
      options: PermissionsOptions {
        allow_import: s(&["jsr.io:443"]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert!(has_gap(c, GapClass::Import, GapResolution::FailClosed));
      },
    },
  ]);
}

/// §1.2 "Unix sockets", as amended (review F1).
#[test]
fn unix_rows() {
  run_rows(vec![
    Row {
      name: "no grant: AF_UNIX refused",
      options: PermissionsOptions {
        allow_write: all(),
        allow_read: s(&["/project"]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| assert_eq!(c.policy.unix, UnixPolicy::Denied),
    },
    Row {
      name: "Linux: a named socket opens AF_UNIX, reported fail-open",
      options: PermissionsOptions {
        allow_net: s(&["unix:/var/run/docker.sock"]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert_eq!(c.policy.unix, UnixPolicy::Open);
        let g = gap(c, GapClass::Unix);
        assert_eq!(g.resolution, GapResolution::FailOpen);
        assert_eq!(g.exact_with, Some(ExactWith::Broker));
      },
    },
    Row {
      name: "macOS: a named socket is held by path",
      options: PermissionsOptions {
        allow_net: s(&["unix:/var/run/docker.sock"]),
        ..Default::default()
      },
      caps: macos(),
      mode: Auto,
      check: |c| {
        assert_eq!(
          c.policy.unix,
          UnixPolicy::Paths {
            paths: vec![PathBuf::from("/var/run/docker.sock")]
          }
        );
      },
    },
    Row {
      name: "Linux broker: a named socket is held by path",
      options: PermissionsOptions {
        allow_net: s(&["unix:/var/run/docker.sock"]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Broker,
      check: |c| {
        assert_eq!(
          c.policy.unix,
          UnixPolicy::Paths {
            paths: vec![PathBuf::from("/var/run/docker.sock")]
          }
        );
      },
    },
    Row {
      name: "Linux: unscoped net with scoped files fails closed for sockets",
      options: PermissionsOptions {
        allow_net: all(),
        allow_read: s(&["/project"]),
        allow_write: s(&["/project"]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert_eq!(c.policy.unix, UnixPolicy::Denied);
        assert!(has_gap(c, GapClass::Unix, GapResolution::FailClosed));
      },
    },
    Row {
      name: "macOS: unscoped net with scoped files: sockets in read ∩ write",
      options: PermissionsOptions {
        allow_net: all(),
        allow_read: s(&["/project"]),
        allow_write: s(&["/project/run"]),
        ..Default::default()
      },
      caps: macos(),
      mode: Auto,
      check: |c| {
        assert_eq!(
          c.policy.unix,
          UnixPolicy::Paths {
            paths: vec![PathBuf::from("/project/run")]
          }
        );
      },
    },
    Row {
      name: "Linux -A with denies: every socket, reported",
      options: PermissionsOptions {
        deny_write: s(&["/etc"]),
        ..allow_all()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert_eq!(c.policy.unix, UnixPolicy::Open);
        assert!(has_gap(c, GapClass::Unix, GapResolution::FailOpen));
      },
    },
  ]);
}

/// §1.3 run and §1.4 ffi.
#[test]
fn run_and_ffi_rows() {
  run_rows(vec![
    Row {
      name: "no --allow-run: no exec",
      options: PermissionsOptions::default(),
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert_eq!(c.policy.fs.exec, ExecScope::None);
        assert!(c.policy.syscalls.refuse_exec);
      },
    },
    Row {
      name: "--allow-run=git: exec unrestricted (D3), the children baseline",
      options: PermissionsOptions {
        allow_run: s(&["git"]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert_eq!(c.policy.fs.exec, ExecScope::Unrestricted);
        assert!(!c.policy.syscalls.refuse_exec);
        assert!(c.policy.fs.read.covers(Path::new("/usr/bin/git")));
        assert!(c.policy.fs.map_exec.covers(Path::new("/usr/bin/git")));
        let g = gap(c, GapClass::Run);
        assert_eq!(g.resolution, GapResolution::Documented);
      },
    },
    Row {
      name: "without --allow-run there is no children baseline",
      options: PermissionsOptions::default(),
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert!(!c.policy.fs.read.covers(Path::new("/usr/bin/git")));
      },
    },
    Row {
      name: "macOS --allow-ffi=P: map-executable scoped to P and the system",
      options: PermissionsOptions {
        allow_ffi: s(&["/project/lib/libfoo.dylib"]),
        allow_read: s(&["/project"]),
        ..Default::default()
      },
      caps: macos(),
      mode: Auto,
      check: |c| {
        let map = &c.policy.fs.map_exec;
        assert!(map.covers(Path::new("/project/lib/libfoo.dylib")));
        assert!(map.covers(Path::new("/usr/lib/libSystem.B.dylib")));
        assert!(!map.covers(Path::new("/project/other.dylib")));
      },
    },
    Row {
      name: "Linux --allow-ffi=P: loading scoped by reads only, documented",
      options: PermissionsOptions {
        allow_ffi: s(&["/project/lib/libfoo.so"]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert!(has_gap(c, GapClass::Ffi, GapResolution::Documented));
      },
    },
    Row {
      name: "a writable library folder leaves the implicit set (the dlopen pivot)",
      options: PermissionsOptions {
        allow_write: s(&["/usr/local/lib"]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert!(!c.policy.fs.read.covers(Path::new("/usr/local/lib/x.so")));
        assert!(has_gap(c, GapClass::Ffi, GapResolution::FailClosed));
      },
    },
    Row {
      name: "Linux before ABI 6 with FFI and no run: signals are a gap",
      options: PermissionsOptions {
        allow_ffi: all(),
        ..Default::default()
      },
      caps: linux(Some(4)),
      mode: Auto,
      check: |c| {
        assert!(!c.policy.scopes.signal);
        let g = gap(c, GapClass::Signal);
        assert_eq!(g.exact_with, Some(ExactWith::LandlockAbi { abi: 6 }));
      },
    },
    Row {
      name: "Linux ABI 6: signals and abstract sockets scoped",
      options: PermissionsOptions {
        allow_ffi: all(),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert!(c.policy.scopes.signal && c.policy.scopes.abstract_unix);
        assert!(!c.gaps.iter().any(|g| g.class == GapClass::Signal));
      },
    },
  ]);
}

/// §1.5 sys and env.
#[test]
fn sys_and_env_rows() {
  run_rows(vec![
    Row {
      name: "Linux no sys: /proc files hidden, no netlink",
      options: PermissionsOptions::default(),
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert_eq!(
          c.policy.sys.hidden_files,
          vec![
            PathBuf::from("/proc/loadavg"),
            PathBuf::from("/proc/meminfo"),
            PathBuf::from("/proc/uptime"),
            PathBuf::from("/proc/net"),
          ]
        );
        assert!(!c.policy.sys.netlink_route);
        assert!(has_gap(c, GapClass::Sys, GapResolution::Documented));
      },
    },
    Row {
      name: "Linux sys=loadavg,networkInterfaces: those files stay",
      options: PermissionsOptions {
        allow_sys: s(&["loadavg", "networkInterfaces"]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert_eq!(
          c.policy.sys.hidden_files,
          vec![
            PathBuf::from("/proc/meminfo"),
            PathBuf::from("/proc/uptime")
          ]
        );
        assert!(c.policy.sys.netlink_route);
      },
    },
    Row {
      name: "macOS no sys: sysctls narrowed",
      options: PermissionsOptions::default(),
      caps: macos(),
      mode: Auto,
      check: |c| {
        assert_eq!(
          c.policy.sys.denied_sysctls,
          vec!["kern.proc*", "vm.loadavg", "hw.memsize"]
        );
      },
    },
    Row {
      name: "unscoped sys: no gap",
      options: PermissionsOptions {
        allow_sys: all(),
        ..Default::default()
      },
      caps: macos(),
      mode: Auto,
      check: |c| {
        assert!(!c.gaps.iter().any(|g| g.class == GapClass::Sys));
        assert_eq!(c.policy.sys.denied_sysctls, vec!["kern.proc*"]);
      },
    },
    Row {
      name: "auto: env is Deno-only, documented (D5)",
      options: PermissionsOptions {
        allow_env: s(&["PORT"]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert_eq!(c.policy.env_scrub, None);
        assert!(has_gap(c, GapClass::Env, GapResolution::Documented));
      },
    },
    Row {
      name: "Linux enforce: the environment is scrubbed to the grant",
      options: PermissionsOptions {
        allow_env: s(&["PORT"]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Enforce,
      check: |c| {
        let keep = c.policy.env_scrub.as_ref().unwrap();
        assert!(keep.contains(&"PORT".to_string()));
        assert!(keep.contains(&"PATH".to_string()));
        assert!(!keep.contains(&"AWS_SECRET_ACCESS_KEY".to_string()));
      },
    },
    Row {
      name: "macOS enforce in Mode A: no scrub (no exec)",
      options: PermissionsOptions {
        allow_env: s(&["PORT"]),
        ..Default::default()
      },
      caps: macos(),
      mode: Enforce,
      check: |c| assert_eq!(c.policy.env_scrub, None),
    },
    Row {
      name: "unscoped env: nothing to scrub, no gap",
      options: PermissionsOptions {
        allow_env: all(),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Enforce,
      check: |c| {
        assert_eq!(c.policy.env_scrub, None);
        assert!(!c.gaps.iter().any(|g| g.class == GapClass::Env));
      },
    },
  ]);
}

#[test]
fn metadata_and_windows_rows() {
  run_rows(vec![
    Row {
      name: "Linux auto with native code and a scoped write: metadata documented (F5)",
      options: PermissionsOptions {
        allow_write: s(&["/out"]),
        allow_run: all(),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert!(!c.policy.syscalls.refuse_path_metadata);
        assert!(has_gap(c, GapClass::Metadata, GapResolution::Documented));
      },
    },
    Row {
      name: "Linux enforce: the path forms of chmod & co are refused",
      options: PermissionsOptions {
        allow_write: s(&["/out"]),
        allow_run: all(),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Enforce,
      check: |c| assert!(c.policy.syscalls.refuse_path_metadata),
    },
    Row {
      name: "Windows: reads documented Deno-only, writes confined",
      options: PermissionsOptions {
        allow_read: s(&["C:/data"]),
        allow_write: s(&["C:/out"]),
        ..Default::default()
      },
      caps: windows(),
      mode: Auto,
      check: |c| {
        assert!(has_gap(c, GapClass::Read, GapResolution::Documented));
        assert!(!c.gaps.iter().any(|g| g.class == GapClass::Write
          && g.resolution == GapResolution::FailOpen));
      },
    },
  ]);
}

#[test]
fn implicit_set_follows_the_grant() {
  let c = ctx(linux(Some(6)), Auto);
  let none = ImplicitSet::build(&PermissionsOptions::default(), &c);
  let has = |set: &ImplicitSet, path: &str| {
    set.entries.iter().any(|e| e.path == Path::new(path))
  };
  assert!(has(&none, "/etc/passwd"));
  assert!(has(&none, "/etc/ld.so.cache"));
  assert!(!has(&none, "/etc/resolv.conf"), "DNS only with network");
  assert!(!has(&none, "/etc/ssl"), "TLS roots only with network");
  assert!(
    !has(&none, "/dev/tty"),
    "the terminal only with a TTY parent"
  );
  let net = ImplicitSet::build(
    &PermissionsOptions {
      allow_net: s(&["example.com"]),
      ..Default::default()
    },
    &c,
  );
  assert!(has(&net, "/etc/resolv.conf") && has(&net, "/etc/ssl"));
  let tty = ImplicitSet::build(
    &PermissionsOptions::default(),
    &Context {
      tty_parent: true,
      ..c.clone()
    },
  );
  assert!(has(&tty, "/dev/tty"));
  // dynamic entries
  let full = ImplicitSet::build(
    &PermissionsOptions::default(),
    &Context {
      exe: Some(PathBuf::from("/nix/store/abc-deno/bin/deno")),
      deno_dir: Some(PathBuf::from("/home/me/.cache/deno")),
      config_files: vec![PathBuf::from("/project/deno.json")],
      node_modules: Some(PathBuf::from("/project/node_modules")),
      ..c.clone()
    },
  );
  assert!(has(&full, "/nix/store"));
  assert!(has(&full, "/home/me/.cache/deno/npm"));
  assert!(has(&full, "/project/deno.json"));
  assert!(has(&full, "/project/node_modules"));
  // a compiled program reads no DENO_DIR caches
  let compiled = ImplicitSet::build(
    &PermissionsOptions::default(),
    &Context {
      deno_dir: Some(PathBuf::from("/home/me/.cache/deno")),
      compiled: true,
      ..c
    },
  );
  assert!(!has(&compiled, "/home/me/.cache/deno/npm"));
  // never a credential folder
  for set in [&none, &net, &full] {
    assert!(!set.entries.iter().any(|e| {
      let p = e.path.to_string_lossy();
      p.contains("/.ssh") || p.contains("/.aws") || p.contains(".npmrc")
    }));
  }
}

#[test]
fn inspector_port_is_bindable() {
  let c = Context {
    inspector_port: Some(9229),
    ..ctx(linux(Some(6)), Auto)
  };
  let compiled = compile_in(&PermissionsOptions::default(), &c);
  assert_eq!(
    compiled.policy.net,
    NetPolicy::Ports {
      connect: PortScope::None,
      bind: PortScope::Ports { ports: vec![9229] },
    }
  );
  assert!(!compiled.policy.syscalls.refuse_listen);
}

#[test]
fn capability_table() {
  let l1 = linux(Some(1));
  assert!(l1.supports(Feature::FsRead));
  assert!(!l1.supports(Feature::FsRefer));
  assert!(!l1.supports(Feature::TcpPorts));
  let l4 = linux(Some(4));
  assert!(l4.supports(Feature::TcpPorts));
  assert!(!l4.supports(Feature::ScopeSignal));
  let l6 = linux(Some(6));
  assert!(l6.supports(Feature::ScopeSignal));
  assert!(l6.supports(Feature::ScopeAbstractUnix));
  assert!(!l6.supports(Feature::AuditLog));
  assert!(linux(Some(7)).supports(Feature::AuditLog));
  assert!(!linux(None).supports(Feature::FsRead));
  assert!(linux(None).supports(Feature::SocketFamilies));
  assert!(!l6.supports(Feature::DenyInsideAllow));
  assert!(!l6.supports(Feature::UnixSocketPaths));
  assert!(!l6.supports(Feature::Overmount));
  assert!(linux_userns(1).supports(Feature::Overmount));
  let mac = macos();
  for feature in [
    Feature::FsRead,
    Feature::FsWrite,
    Feature::DenyInsideAllow,
    Feature::MapExecutable,
    Feature::UnixSocketPaths,
    Feature::TcpPorts,
  ] {
    assert!(mac.supports(feature), "{feature:?}");
  }
  assert!(!Capabilities::none(TargetOs::Macos).supports(Feature::FsRead));
  let win = windows();
  assert!(win.supports(Feature::WriteConfinement));
  assert!(!win.supports(Feature::FsRead));
  assert_eq!(Capabilities::landlock_abi_for(Feature::TcpPorts), Some(4));
  assert_eq!(Capabilities::landlock_abi_for(Feature::FsRefer), Some(2));
  assert_eq!(
    Capabilities::landlock_abi_for(Feature::ScopeSignal),
    Some(6)
  );
  assert_eq!(
    Capabilities::landlock_abi_for(Feature::DenyInsideAllow),
    None
  );
}

#[test]
fn no_enforcer_is_one_fatal_gap() {
  let caps = Capabilities {
    enforcer_built: false,
    ..macos()
  };
  let c = auto(&PermissionsOptions::default(), caps);
  let fatal: Vec<_> = c.fatal_gaps().collect();
  assert_eq!(fatal.len(), 1);
  assert_eq!(fatal[0].class, GapClass::Enforcer);
  // with an enforcer, a plain read grant on macOS has nothing fatal
  let c = auto(
    &PermissionsOptions {
      allow_read: s(&["/data"]),
      ..Default::default()
    },
    macos(),
  );
  assert_eq!(c.fatal_gaps().count(), 0, "{:#?}", c.gaps);
}

#[test]
fn serde_round_trip() {
  for (options, caps) in [
    (PermissionsOptions::default(), linux(Some(6))),
    (
      PermissionsOptions {
        allow_read: s(&["/data"]),
        deny_read: s(&["/data/x"]),
        allow_write: s(&["/out"]),
        deny_write: s(&["/out/keep"]),
        allow_net: s(&["example.com:443", "unix:/run/x.sock"]),
        allow_run: s(&["git"]),
        allow_env: s(&["PORT"]),
        allow_sys: s(&["hostname"]),
        allow_ffi: s(&["/data/lib.so"]),
        ..Default::default()
      },
      linux(Some(4)),
    ),
    (
      PermissionsOptions {
        deny_read: s(&["/Users/me/.ssh"]),
        ..allow_all()
      },
      macos(),
    ),
    (
      PermissionsOptions {
        allow_net: s(&["127.0.0.1:80"]),
        ..Default::default()
      },
      windows(),
    ),
  ] {
    let compiled = auto(&options, caps);
    let json = serde_json::to_string(&compiled).unwrap();
    let back: CompiledPolicy = serde_json::from_str(&json).unwrap();
    assert_eq!(back, compiled);
    assert_eq!(back.hash(), compiled.hash());
  }
}

/// The policy hash is a function of the serialized policy alone: pinned for
/// a fixed policy, independent of input order, and different for different
/// policies.
#[test]
fn policy_hash_is_stable() {
  let policy = KernelPolicy {
    format: POLICY_FORMAT,
    os: TargetOs::Linux,
    shape: Shape::ModeA,
    deny_only: false,
    fs: FsRules {
      read: PathScope::Paths {
        paths: vec![
          PathRule {
            path: PathBuf::from("/data"),
            implicit: None,
          },
          PathRule {
            path: PathBuf::from("/usr/lib"),
            implicit: Some(ImplicitNeed::SharedLibraries),
          },
        ],
      },
      write: PathScope::Paths { paths: vec![] },
      deny_read: vec![],
      deny_write: vec![],
      exec: ExecScope::None,
      map_exec: PathScope::Paths { paths: vec![] },
    },
    net: NetPolicy::Denied,
    unix: UnixPolicy::Denied,
    sys: SysNarrowing::default(),
    env_scrub: None,
    scopes: Scopes::default(),
    syscalls: SyscallPolicy::default(),
  };
  assert_eq!(
    serde_json::to_string(&policy).unwrap(),
    concat!(
      r#"{"format":1,"os":"linux","shape":"modeA","denyOnly":false,"#,
      r#""fs":{"read":{"kind":"paths","paths":[{"path":"/data"},"#,
      r#"{"path":"/usr/lib","implicit":"sharedLibraries"}]},"#,
      r#""write":{"kind":"paths","paths":[]},"denyRead":[],"denyWrite":[],"#,
      r#""exec":"none","mapExec":{"kind":"paths","paths":[]}},"#,
      r#""net":{"kind":"denied"},"unix":{"kind":"denied"},"#,
      r#""sys":{"hiddenFiles":[],"deniedSysctls":[],"netlinkRoute":false},"#,
      r#""envScrub":null,"scopes":{"signal":false,"abstractUnix":false},"#,
      r#""syscalls":{"attackSurface":false,"refuseExec":false,"#,
      r#""refuseListen":false,"clone3Enosys":false,"refuseIoUring":false,"#,
      r#""refusePathMetadata":false,"vsock":false}}"#
    )
  );
  assert_eq!(policy.hash(), HASH_OF_FIXED_POLICY);

  // input order doesn't matter
  let a = auto(
    &PermissionsOptions {
      allow_read: s(&["/a", "/b"]),
      allow_net: s(&["127.0.0.1:2", "127.0.0.1:1"]),
      ..Default::default()
    },
    linux(Some(6)),
  );
  let b = auto(
    &PermissionsOptions {
      allow_read: s(&["/b", "/a", "/a"]),
      allow_net: s(&["127.0.0.1:1", "127.0.0.1:2"]),
      ..Default::default()
    },
    linux(Some(6)),
  );
  assert_eq!(a.hash(), b.hash());
  assert_eq!(a.hash().len(), 16);
  // a different grant, a different hash
  let c = auto(
    &PermissionsOptions {
      allow_read: s(&["/a"]),
      ..Default::default()
    },
    linux(Some(6)),
  );
  assert_ne!(a.hash(), c.hash());
}

/// SHA-256 of the JSON above, first 8 bytes in hex.
const HASH_OF_FIXED_POLICY: &str = "e310efe41517d7d3";
