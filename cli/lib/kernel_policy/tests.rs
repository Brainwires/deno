// Copyright 2018-2026 the Deno authors. MIT license.

//! Table-driven tests of the policy model: one row per mapping of plans/
//! kernel-enforcement.md §1.1–§1.5 (as amended by the review), flags →
//! policy + gaps.

use std::path::Path;
use std::path::PathBuf;

use deno_runtime::deno_permissions::PermissionsOptions;
use pretty_assertions::assert_eq;

use super::*;
use crate::kernel_sandbox::KernelSandboxMode;

// the rows below describe a runtime without the broker or the overmount
const _: () = assert!(BROKER_FS_BUILT && BROKER_NET_BUILT && !OVERMOUNT_BUILT);

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
      // the overmount isn't built yet (OVERMOUNT_BUILT): user namespaces
      // change nothing until it is
      name: "Linux with user namespaces: no overmount yet, Deno-only",
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
          DenyEnforcement::DenoOnly
        );
        let g = gap(c, GapClass::Read);
        assert_eq!(g.resolution, GapResolution::FailOpen);
        assert_eq!(g.exact_with, Some(ExactWith::UserNamespaces));
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
        // the broker's file side holds it: nothing fatal is left
        let fatal: Vec<_> = c.fatal_gaps().collect();
        assert!(fatal.is_empty(), "{fatal:#?}");
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
        assert!(!c.policy.syscalls.refuse_path_truncate);
      },
    },
    Row {
      name: "Linux enforce before TRUNCATE: seccomp refuses the truncations, fail closed",
      options: PermissionsOptions {
        allow_write: s(&["/out"]),
        ..Default::default()
      },
      caps: linux(Some(2)),
      mode: Enforce,
      check: |c| {
        assert!(c.policy.syscalls.refuse_path_truncate);
        assert!(c.gaps.iter().any(|g| g.class == GapClass::Write
          && g.resolution == GapResolution::FailClosed
          && g.exact_with == Some(ExactWith::LandlockAbi { abi: 3 })));
        assert_eq!(c.fatal_gaps().count(), 0, "{:#?}", c.gaps);
      },
    },
    Row {
      name: "Linux enforce with TRUNCATE: nothing extra refused",
      options: PermissionsOptions {
        allow_write: s(&["/out"]),
        ..Default::default()
      },
      caps: linux(Some(3)),
      mode: Enforce,
      check: |c| assert!(!c.policy.syscalls.refuse_path_truncate),
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
        // what Mode A holds: the coarsening is documented, so
        // enforce runs with it
        let g = gap(c, GapClass::Net);
        assert_eq!(g.resolution, GapResolution::Documented);
        assert!(g.detail.contains("169.254.169.254"));
        assert_eq!(c.fatal_gaps().count(), 0, "{:#?}", c.gaps);
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
      name: "macOS: a name goes to the broker too",
      options: PermissionsOptions {
        allow_net_connect: s(&["example.com:443"]),
        ..Default::default()
      },
      caps: macos(),
      mode: Auto,
      check: |c| {
        assert_eq!(c.policy.shape, Shape::ModeB);
        assert_eq!(
          c.policy.net,
          NetPolicy::Broker {
            connect: HostScope::Hosts {
              hosts: vec!["example.com:443".to_string()]
            },
            bind: HostScope::None,
            deny: vec![],
          }
        );
        assert!(!c.gaps.iter().any(|g| g.class == GapClass::Net));
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
      name: "macOS: --allow-net-listen=:8000 binds 8000 only",
      options: PermissionsOptions {
        allow_net_listen: s(&[
          "0.0.0.0:8000",
          "127.0.0.1:8000",
          "localhost:8000",
        ]),
        ..Default::default()
      },
      caps: macos(),
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
        assert!(c.policy.prebound.is_empty());
      },
    },
    Row {
      name: "Linux: a listen port that can't be bound ahead keeps its port rule",
      options: PermissionsOptions {
        // port 0 is a new port for every listener
        allow_net_listen: s(&["127.0.0.1:8000", "127.0.0.1:0"]),
        ..Default::default()
      },
      caps: linux(Some(6)),
      mode: Auto,
      check: |c| {
        assert_eq!(
          c.policy.net,
          NetPolicy::Ports {
            connect: PortScope::None,
            bind: PortScope::Ports {
              ports: vec![0, 8000]
            },
          }
        );
        assert!(c.policy.prebound.is_empty());
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
        // the broker's network side: until it is built, Mode B compiles
        // the network as Mode A does (an open network, the deny Deno-only)
        if !BROKER_NET_BUILT {
          assert_eq!(c.policy.net, NetPolicy::Open);
          return;
        }
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
        // the broker's network side: until it is built the socket opens
        // AF_UNIX as under Mode A, with its fatal gap
        if !BROKER_NET_BUILT {
          assert_eq!(c.policy.unix, UnixPolicy::Open);
          assert!(c.fatal_gaps().any(|g| g.class == GapClass::Unix));
          return;
        }
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
// denext (DENEXT.md item 449): --kernel-sandbox-exec=strict holds the tree
// to the run list's programs and their interpreters (plan D3's opt-in)
#[cfg(unix)]
#[test]
#[allow(
  clippy::disallowed_methods,
  reason = "the real /bin/sh the closure resolves"
)]
fn strict_exec_lists_the_programs() {
  let options = PermissionsOptions {
    allow_run: s(&["sh"]),
    ..Default::default()
  };
  let mut strict = ctx(linux(Some(6)), KernelSandboxMode::Enforce);
  strict.exec_strict = true;
  let c = compile_in(&options, &strict);
  assert_eq!(c.policy.fs.exec, ExecScope::Listed);
  assert!(!c.policy.syscalls.refuse_exec);
  let sh = std::fs::canonicalize("/bin/sh").unwrap();
  assert!(c.policy.fs.exec_programs.contains(&sh), "{c:?}");
  let g = gap(&c, GapClass::Run);
  assert_eq!(g.resolution, GapResolution::Documented);
  assert!(g.detail.contains("only these programs"), "{}", g.detail);
  assert!(c.fatal_gaps().next().is_none(), "{:?}", c.gaps);
  // not strict: unrestricted, the list's gap names the opt-in
  let c =
    compile_in(&options, &ctx(linux(Some(6)), KernelSandboxMode::Enforce));
  assert_eq!(c.policy.fs.exec, ExecScope::Unrestricted);
  assert!(c.policy.fs.exec_programs.is_empty());
  assert!(
    gap(&c, GapClass::Run)
      .detail
      .contains("--kernel-sandbox-exec=strict")
  );
  // strict with an unscoped --allow-run can't list anything: enforce
  // refuses
  let unscoped = PermissionsOptions {
    allow_run: all(),
    ..Default::default()
  };
  let c = compile_in(&unscoped, &strict);
  assert_eq!(c.policy.fs.exec, ExecScope::Unrestricted);
  assert!(
    c.fatal_gaps()
      .any(|g| g.rule.contains("--kernel-sandbox-exec=strict")),
    "{:?}",
    c.gaps
  );
  // strict without a run grant: nothing may start, as without strict
  let c = compile_in(&PermissionsOptions::default(), &strict);
  assert_eq!(c.policy.fs.exec, ExecScope::None);
  assert!(c.policy.syscalls.refuse_exec);
}

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
      // file-map-executable didn't hold to the --allow-ffi paths in testing
      // (macOS 26.6): the model claims the read scope, as on Linux
      name: "macOS --allow-ffi=P: code loads from the read scope, documented",
      options: PermissionsOptions {
        allow_ffi: s(&["/project/lib/libfoo.dylib"]),
        allow_read: s(&["/project"]),
        ..Default::default()
      },
      caps: macos(),
      mode: Auto,
      check: |c| {
        let map = &c.policy.fs.map_exec;
        assert_eq!(map, &c.policy.fs.read);
        assert!(map.covers(Path::new("/project/lib/libfoo.dylib")));
        assert!(map.covers(Path::new("/project/other.dylib")));
        assert!(!map.covers(Path::new("/elsewhere/lib.dylib")));
        assert!(!macos().supports(Feature::MapExecutable));
        let g = gap(c, GapClass::Ffi);
        assert_eq!(g.resolution, GapResolution::Documented);
        assert!(g.detail.contains("read scope"), "{}", g.detail);
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
      name: "Linux before ABI 6 with FFI and no run: seccomp holds signals",
      options: PermissionsOptions {
        allow_ffi: all(),
        ..Default::default()
      },
      caps: linux(Some(4)),
      mode: Auto,
      check: |c| {
        assert!(!c.policy.scopes.signal);
        assert!(c.policy.syscalls.signal_self);
        assert!(!c.gaps.iter().any(|g| g.class == GapClass::Signal));
      },
    },
    Row {
      name: "Linux before ABI 6 with a run list: signals not narrowed",
      options: PermissionsOptions {
        allow_ffi: all(),
        allow_run: s(&["/bin/true"]),
        ..Default::default()
      },
      caps: linux(Some(4)),
      mode: Auto,
      check: |c| assert!(!c.policy.syscalls.signal_self),
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
        assert!(!c.policy.syscalls.signal_self);
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
      name: "macOS enforce: the re-executed image is scrubbed too",
      options: PermissionsOptions {
        allow_env: s(&["PORT"]),
        ..Default::default()
      },
      caps: macos(),
      mode: Enforce,
      check: |c| {
        let keep = c.policy.env_scrub.as_ref().unwrap();
        assert!(keep.contains(&"PORT".to_string()));
        assert!(!keep.contains(&"DENO_AUTH_TOKENS".to_string()));
        assert!(!c.gaps.iter().any(|g| g.class == GapClass::Env));
      },
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
        // only the fail-closed note: other processes' /proc/<pid>/environ
        assert!(!c.gaps.iter().any(|g| g.class == GapClass::Env
          && g.resolution != GapResolution::FailClosed));
      },
    },
  ]);
}

/// D5 (decision O12): an `enforce` that `auto` chose (R4, a compiled
/// program with no mode baked) leaves the environment as it is; only an
/// explicit or baked enforce / broker scrubs it.
#[test]
fn auto_chosen_enforce_keeps_the_environment() {
  let options = PermissionsOptions {
    allow_env: s(&["PORT"]),
    ..Default::default()
  };
  for caps in [linux(Some(6)), macos()] {
    let mut chosen = ctx(caps, Enforce);
    chosen.requested = Auto;
    let c = compile_in(&options, &chosen);
    assert_eq!(c.policy.env_scrub, None, "{:?}", caps.os);
    assert!(has_gap(&c, GapClass::Env, GapResolution::Documented));
    // an explicit enforce or broker scrubs it, as before
    for explicit in [Enforce, Broker] {
      let c = compile_in(&options, &ctx(caps, explicit));
      assert!(c.policy.env_scrub.is_some(), "{:?} {explicit:?}", caps.os);
    }
  }
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
      check: |c| {
        assert!(c.policy.syscalls.refuse_path_metadata);
        // inside the write scope too: a fail-closed gap says so
        assert!(has_gap(c, GapClass::Metadata, GapResolution::FailClosed));
        // and not the auto-mode residual (the metadata-read gap stays)
        assert!(!c.gaps.iter().any(|g| g.class == GapClass::Metadata
          && g.resolution == GapResolution::Documented
          && g.detail.contains("change the mode")));
      },
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
  // Linuxbrew's prefix is part of the children baseline, only with run
  assert!(!has(&none, "/home/linuxbrew/.linuxbrew"));
  let run = ImplicitSet::build(
    &PermissionsOptions {
      allow_run: s(&["python3"]),
      ..Default::default()
    },
    &c,
  );
  assert!(has(&run, "/home/linuxbrew/.linuxbrew"));
  let tty = ImplicitSet::build(
    &PermissionsOptions::default(),
    &Context {
      tty_parent: true,
      ttys: vec![PathBuf::from("/dev/pts/3")],
      ..c.clone()
    },
  );
  assert!(has(&tty, "/dev/tty"));
  // the standard streams' own terminal, never every terminal of the user
  assert!(has(&tty, "/dev/pts/3"));
  assert!(!has(&tty, "/dev/pts"));
  let policy = KernelPolicy::compile(
    &PermissionsOptions::default(),
    &tty,
    &Context {
      tty_parent: true,
      ttys: vec![PathBuf::from("/dev/pts/3")],
      ..c.clone()
    },
  );
  assert!(policy.policy.fs.write.covers(Path::new("/dev/pts/3")));
  assert!(!policy.policy.fs.write.covers(Path::new("/dev/pts/4")));
  assert!(!policy.policy.fs.read.covers(Path::new("/dev/pts/4")));
  // dynamic entries
  let full = ImplicitSet::build(
    &PermissionsOptions::default(),
    &Context {
      exe: Some(PathBuf::from("/nix/store/abc-deno/bin/deno")),
      deno_dir: Some(PathBuf::from("/home/me/.cache/deno")),
      config_files: vec![PathBuf::from("/project/deno.json")],
      node_modules: Some(PathBuf::from("/project/node_modules")),
      linked_node_modules: vec![PathBuf::from("/packages/ui/node_modules")],
      vendor_dir: Some(PathBuf::from("/project/vendor")),
      link_dirs: vec![PathBuf::from("/linked")],
      audit_file: Some(PathBuf::from("/project/audit.jsonl")),
      ..c.clone()
    },
  );
  assert!(
    full
      .entries
      .iter()
      .any(|e| e.path == Path::new("/project/audit.jsonl")
        && e.need == ImplicitNeed::AuditLog
        && e.access == ImplicitAccess::ReadWrite)
  );
  assert!(full.entries.iter().any(|e| e.path == Path::new("/linked")
    && e.need == ImplicitNeed::LinkedPackages
    && e.access == ImplicitAccess::Read));
  assert!(has(&full, "/nix/store"));
  // the vendor folder is read (the remote modules a confined run loads),
  // never written
  assert!(
    full
      .entries
      .iter()
      .any(|e| e.path == Path::new("/project/vendor")
        && e.need == ImplicitNeed::Vendor
        && e.access == ImplicitAccess::Read)
  );
  assert!(has(&full, "/home/me/.cache/deno/npm"));
  assert!(has(&full, "/project/deno.json"));
  assert!(has(&full, "/project/node_modules"));
  // a node_modules folder a linked package resolves to is read and mapped
  // as the project's own is
  assert!(
    full
      .entries
      .iter()
      .any(|e| e.path == Path::new("/packages/ui/node_modules")
        && e.need == ImplicitNeed::NodeModules
        && e.access == ImplicitAccess::ReadMap)
  );
  assert!(!has(&none, "/packages/ui/node_modules"));
  // a compiled program reads no DENO_DIR caches
  let compiled = ImplicitSet::build(
    &PermissionsOptions::default(),
    &Context {
      deno_dir: Some(PathBuf::from("/home/me/.cache/deno")),
      vendor_dir: Some(PathBuf::from("/project/vendor")),
      compiled: true,
      ..c
    },
  );
  assert!(!has(&compiled, "/home/me/.cache/deno/npm"));
  assert!(!has(&compiled, "/project/vendor"));
  // never a credential folder
  for set in [&none, &net, &full] {
    assert!(!set.entries.iter().any(|e| {
      let p = e.path.to_string_lossy();
      p.contains("/.ssh") || p.contains("/.aws") || p.contains(".npmrc")
    }));
  }
}

// Linux, Mode A: the addresses `--allow-net-listen` names are bound before
// the process confines itself, so the kernel policy has no bind side: on
// every Landlock ABI, a listen-only server has no IP socket of its own.
#[test]
fn listeners_are_bound_before_confinement() {
  use std::net::SocketAddr;
  let addrs = |list: &[&str]| -> Vec<SocketAddr> {
    list.iter().map(|a| a.parse().unwrap()).collect()
  };
  let listen = PermissionsOptions {
    allow_net_listen: s(&["0.0.0.0:8000", "127.0.0.1:8000", "localhost:9000"]),
    ..Default::default()
  };
  // ABI 1 and 2 have no TCP rules at all; it holds there too
  for abi in [1, 2, 4, 6] {
    let c = compile(&listen, linux(Some(abi)), Enforce);
    assert_eq!(c.policy.shape, Shape::ModeA, "abi {abi}");
    assert_eq!(c.policy.net, NetPolicy::Denied, "abi {abi}");
    assert_eq!(
      c.policy.prebound,
      addrs(&[
        "0.0.0.0:8000",
        "127.0.0.1:8000",
        "127.0.0.1:9000",
        "[::1]:9000"
      ]),
      "abi {abi}"
    );
    // the confined process never listens itself
    assert!(c.policy.syscalls.refuse_listen, "abi {abi}");
    assert_eq!(c.fatal_gaps().count(), 0, "abi {abi}: {:#?}", c.gaps);
    let note = gap(&c, GapClass::Net);
    assert_eq!(note.resolution, GapResolution::Documented);
    assert!(note.detail.contains("bound before the sandbox applies"));
  }

  // with a connect grant beside it: the connect ports, no bind side
  let both = PermissionsOptions {
    allow_net_connect: s(&["127.0.0.1:5432"]),
    ..listen.clone()
  };
  let c = compile(&both, linux(Some(6)), Enforce);
  assert_eq!(
    c.policy.net,
    NetPolicy::Ports {
      connect: PortScope::Ports { ports: vec![5432] },
      bind: PortScope::None,
    }
  );
  assert_eq!(c.policy.prebound.len(), 4);

  // --allow-net names servers to connect to as often as addresses to
  // listen on: its ports are never taken at startup
  let allow_net = PermissionsOptions {
    allow_net: s(&["127.0.0.1:5432"]),
    ..Default::default()
  };
  let c = compile(&allow_net, linux(Some(6)), Enforce);
  assert!(c.policy.prebound.is_empty());
  let mixed = PermissionsOptions {
    allow_net: s(&["127.0.0.1:5432"]),
    ..listen.clone()
  };
  assert!(
    compile(&mixed, linux(Some(6)), Enforce)
      .policy
      .prebound
      .is_empty()
  );

  // not what can't be bound ahead: a subnet, a bare host, an unscoped
  // listen grant; not with the inspector; not on macOS (bind ports hold)
  for entries in [&["10.0.0.0/8:8000"][..], &["127.0.0.1"][..], &[][..]] {
    let options = PermissionsOptions {
      allow_net_listen: s(entries),
      ..Default::default()
    };
    assert!(
      compile(&options, linux(Some(6)), Auto)
        .policy
        .prebound
        .is_empty(),
      "{entries:?}"
    );
  }
  let inspected = Context {
    inspector_port: Some(9229),
    ..ctx(linux(Some(6)), Enforce)
  };
  assert!(compile_in(&listen, &inspected).policy.prebound.is_empty());
  assert!(
    compile(&listen, macos(), Enforce)
      .policy
      .prebound
      .is_empty()
  );

  // a policy without listeners serializes as before (its hash is unchanged)
  let plain = compile(&allow_net, linux(Some(6)), Enforce);
  assert!(
    !serde_json::to_string(&plain.policy)
      .unwrap()
      .contains("prebound")
  );
  let json = serde_json::to_string(&c.policy).unwrap();
  let _: KernelPolicy = serde_json::from_str(&json).unwrap();
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
    deny_ceiling: false,
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
      exec_programs: vec![],
      map_exec: PathScope::Paths { paths: vec![] },
    },
    net: NetPolicy::Denied,
    prebound: Vec::new(),
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

// enforce takes the broker for what Mode A can't hold: a write deny inside
// a write allow on Linux, a host name anywhere. Nothing is left fatal.
#[test]
fn the_broker_holds_what_mode_a_cant() {
  let c = compile(
    &PermissionsOptions {
      allow_write: s(&["/out"]),
      deny_write: s(&["/out/keep"]),
      ..Default::default()
    },
    linux(Some(6)),
    KernelSandboxMode::Enforce,
  );
  assert_eq!(c.policy.shape, Shape::ModeB);
  assert_eq!(c.fatal_gaps().count(), 0, "{:#?}", c.gaps);
  // an explicit broker with nothing for it to hold starts too
  let c = compile(
    &PermissionsOptions {
      allow_read: s(&["/data"]),
      ..Default::default()
    },
    linux(Some(6)),
    KernelSandboxMode::Broker,
  );
  assert_eq!(c.policy.shape, Shape::ModeB);
  assert_eq!(c.fatal_gaps().count(), 0, "{:#?}", c.gaps);
  // a network grant the kernel holds alone doesn't refuse the broker
  let c = compile(
    &PermissionsOptions {
      allow_net: all(),
      allow_write: s(&["/out"]),
      deny_write: s(&["/out/keep"]),
      ..Default::default()
    },
    linux(Some(6)),
    KernelSandboxMode::Enforce,
  );
  assert_eq!(c.policy.shape, Shape::ModeB);
  assert_eq!(c.policy.net, NetPolicy::Open);
  assert_eq!(c.fatal_gaps().count(), 0, "{:#?}", c.gaps);
  // a module fetch after start fails closed (reported, not fatal), as
  // without the broker
  let c = compile(
    &PermissionsOptions {
      allow_import: s(&["example.com"]),
      ..Default::default()
    },
    linux(Some(6)),
    KernelSandboxMode::Broker,
  );
  assert_eq!(c.fatal_gaps().count(), 0, "{:#?}", c.gaps);
  assert!(
    c.gaps.iter().any(|g| g.class == GapClass::Import
      && g.resolution == GapResolution::FailClosed),
    "{:#?}",
    c.gaps
  );
  let c = compile(
    &PermissionsOptions {
      allow_write: s(&["/out"]),
      deny_write: s(&["/out/keep"]),
      ..Default::default()
    },
    linux(Some(6)),
    KernelSandboxMode::Enforce,
  );
  assert_eq!(
    deny(&c.policy.fs.deny_write, "/out/keep"),
    DenyEnforcement::Broker
  );
  // no network to hold: it stays denied, not the broker's
  assert_eq!(c.policy.net, NetPolicy::Denied);
  // a host name: the broker's network, on Linux and macOS alike
  for caps in [linux(Some(1)), linux(Some(6)), macos()] {
    let c = compile(
      &PermissionsOptions {
        allow_net: s(&["example.com:443"]),
        ..Default::default()
      },
      caps,
      KernelSandboxMode::Enforce,
    );
    assert_eq!(c.policy.shape, Shape::ModeB, "{caps:?}");
    assert!(matches!(c.policy.net, NetPolicy::Broker { .. }), "{caps:?}");
    assert_eq!(c.fatal_gaps().count(), 0, "{caps:?}: {:#?}", c.gaps);
  }
  // an explicit broker with the whole network: nothing to hold, so the
  // process keeps its own sockets; with a deny, the broker holds it
  let open = PermissionsOptions {
    allow_net: all(),
    ..Default::default()
  };
  assert_eq!(
    compile(&open, linux(Some(6)), Broker).policy.net,
    NetPolicy::Open
  );
  let denied = PermissionsOptions {
    deny_net: s(&["169.254.169.254"]),
    ..open
  };
  assert!(matches!(
    compile(&denied, linux(Some(6)), Broker).policy.net,
    NetPolicy::Broker { .. }
  ));
  // and an address list, exact under an explicit broker (Mode A holds it
  // by port)
  let ports = PermissionsOptions {
    allow_net: s(&["127.0.0.1:5432"]),
    ..Default::default()
  };
  assert!(matches!(
    compile(&ports, macos(), Broker).policy.net,
    NetPolicy::Broker { .. }
  ));
  assert!(matches!(
    compile(&ports, macos(), Enforce).policy.net,
    NetPolicy::Ports { .. }
  ));
}

// a compiled program with no supervisor of its own (a desktop app, whose
// process hosts a browser engine; Windows) applies its sandbox in place and
// runs no broker: what only the broker would hold stays a fatal gap (warned
// about at compile, refused at start), never a Mode B policy. One that
// forks its supervisor (`Context::compiled_supervisor`) compiles as the CLI
// does.
#[test]
fn a_compiled_program_has_no_broker() {
  let compiled = |options: &PermissionsOptions, caps: Capabilities| {
    let mut ctx = ctx(caps, KernelSandboxMode::Enforce);
    ctx.compiled = true;
    compile_in(options, &ctx)
  };
  let deny_inside = PermissionsOptions {
    allow_write: s(&["/out"]),
    deny_write: s(&["/out/keep"]),
    ..Default::default()
  };
  let c = compiled(&deny_inside, linux(Some(6)));
  assert_eq!(c.policy.shape, Shape::ModeA);
  assert_eq!(
    deny(&c.policy.fs.deny_write, "/out/keep"),
    DenyEnforcement::DenoOnly
  );
  let fatal: Vec<_> = c.fatal_gaps().collect();
  assert_eq!(fatal.len(), 1, "{:#?}", c.gaps);
  assert_eq!(fatal[0].class, GapClass::Write);
  assert!(fatal[0].rule.starts_with("--deny-write="), "{fatal:#?}");
  // macOS holds the same deny itself: nothing fatal
  let c = compiled(&deny_inside, macos());
  assert_eq!(c.policy.shape, Shape::ModeA);
  assert_eq!(c.fatal_gaps().count(), 0, "{:#?}", c.gaps);
  // a host name: the broker's network, which a compiled program lacks
  let host = PermissionsOptions {
    allow_net: s(&["example.com:443"]),
    ..Default::default()
  };
  for caps in [linux(Some(6)), macos()] {
    let c = compiled(&host, caps);
    assert_eq!(c.policy.shape, Shape::ModeA, "{caps:?}");
    let fatal: Vec<_> = c.fatal_gaps().collect();
    assert_eq!(fatal.len(), 1, "{caps:?}: {:#?}", c.gaps);
    assert_eq!(fatal[0].class, GapClass::Net, "{caps:?}");
    assert_eq!(fatal[0].rule, "--allow-net=example.com:443", "{caps:?}");
  }
  // an address list the kernel holds by port needs no broker
  let c = compiled(
    &PermissionsOptions {
      allow_net: s(&["127.0.0.1:4545"]),
      ..Default::default()
    },
    linux(Some(6)),
  );
  assert!(matches!(c.policy.net, NetPolicy::Ports { .. }));
  assert_eq!(c.fatal_gaps().count(), 0, "{:#?}", c.gaps);
  // with a supervisor of its own the same grants take the broker
  let supervised = |options: &PermissionsOptions, caps: Capabilities| {
    let mut ctx = ctx(caps, KernelSandboxMode::Enforce);
    ctx.compiled = true;
    ctx.compiled_supervisor = true;
    compile_in(options, &ctx)
  };
  let c = supervised(&deny_inside, linux(Some(6)));
  assert_eq!(c.policy.shape, Shape::ModeB);
  assert_eq!(c.fatal_gaps().count(), 0, "{:#?}", c.gaps);
  for caps in [linux(Some(6)), macos()] {
    let c = supervised(&host, caps);
    assert_eq!(c.policy.shape, Shape::ModeB, "{caps:?}");
    assert!(matches!(c.policy.net, NetPolicy::Broker { .. }), "{caps:?}");
    assert_eq!(c.fatal_gaps().count(), 0, "{caps:?}: {:#?}", c.gaps);
  }
}

// the files an --allow-sys name reads are in the read scope only when it
// is granted (Linux); other processes' entries are a fail-closed gap
#[test]
fn sys_names_grant_their_proc_files() {
  let read = |c: &CompiledPolicy| match &c.policy.fs.read {
    PathScope::Paths { paths } => {
      paths.iter().map(|r| r.path.clone()).collect::<Vec<_>>()
    }
    PathScope::All => panic!("read all"),
  };
  let none = auto(&PermissionsOptions::default(), linux(Some(6)));
  for file in [
    "/proc/meminfo",
    "/proc/loadavg",
    "/proc/uptime",
    "/proc/version",
  ] {
    assert!(!read(&none).contains(&PathBuf::from(file)), "{file}");
  }
  let some = auto(
    &PermissionsOptions {
      allow_sys: s(&["loadavg", "osRelease", "processes"]),
      ..Default::default()
    },
    linux(Some(6)),
  );
  assert!(read(&some).contains(&PathBuf::from("/proc/loadavg")));
  assert!(read(&some).contains(&PathBuf::from("/proc/version")));
  assert!(!read(&some).contains(&PathBuf::from("/proc/meminfo")));
  assert!(
    some.gaps.iter().any(|g| g.rule == "--allow-sys=processes"
      && g.resolution == GapResolution::FailClosed),
    "{:#?}",
    some.gaps
  );
  // the runtime's own /proc/self and cgroup reads are documented, not hidden
  assert!(
    some
      .gaps
      .iter()
      .any(|g| g.rule.starts_with("implicit /proc/self")
        && g.resolution == GapResolution::Documented),
    "{:#?}",
    some.gaps
  );
  let unscoped = auto(
    &PermissionsOptions {
      allow_sys: all(),
      ..Default::default()
    },
    linux(Some(6)),
  );
  assert!(read(&unscoped).contains(&PathBuf::from("/proc/meminfo")));
  // a bare --allow-sys doesn't include the named-only processes: no gap for
  // other processes' entries unless it is named
  assert!(
    !unscoped.gaps.iter().any(|g| g.rule.contains("processes")),
    "{:#?}",
    unscoped.gaps
  );
  // macOS reads sysctls, not files
  let mac = auto(
    &PermissionsOptions {
      allow_sys: all(),
      ..Default::default()
    },
    macos(),
  );
  assert!(!read(&mac).contains(&PathBuf::from("/proc/meminfo")));
}

/// `-A` grants the named-only sys names, next to a deny that doesn't name
/// them too (as Deno's checks do): not a "bare --allow-sys", and nothing
/// narrower than the grant to report or refuse.
#[test]
fn allow_all_with_denies_keeps_the_named_only_sys_names() {
  let bare = PermissionsOptions {
    allow_sys: all(),
    ..Default::default()
  };
  let deny_read = PermissionsOptions {
    deny_read: s(&["/home/me/.ssh"]),
    ..allow_all()
  };
  for name in SYS_NAMED_ONLY {
    assert!(!sys_granted(&bare, name), "{name}");
    assert!(sys_granted(&allow_all(), name), "{name}");
    assert!(sys_granted(&deny_read, name), "{name}");
  }
  assert!(sys_granted(&deny_read, "loadavg"));
  for mode in [Auto, Enforce] {
    let c = compile(&deny_read, linux(Some(6)), mode);
    assert!(c.policy.deny_only, "{mode:?}");
    assert!(
      !c.gaps.iter().any(|g| g.rule.contains("processes")),
      "{mode:?}: {:#?}",
      c.gaps
    );
  }
  // the ceiling's grant is -A with the denies: the same
  let options = PermissionsOptions {
    allow_read: s(&["/project"]),
    deny_read: s(&["/home/me/.ssh"]),
    prompt: true,
    ..Default::default()
  };
  let c = ceiling(&options, linux(Some(6)));
  assert!(!c.gaps.iter().any(|g| g.rule.contains("processes")));
  assert_eq!(c.fatal_gaps().count(), 0, "{:#?}", c.gaps);
}

/// A scratch folder (not canonicalized: on macOS the temp folder is itself
/// reached through a symlink) holding `real/` and `link -> real`.
fn aliased_tree(name: &str) -> PathBuf {
  let dir = std::env::temp_dir()
    .join(format!("deno_policy_alias_{name}_{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(dir.join("real/sub")).unwrap();
  std::fs::write(dir.join("real/.env"), "").unwrap();
  std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
  dir
}

fn p(dir: &Path, rel: &str) -> String {
  dir.join(rel).to_str().unwrap().to_string()
}

// a deny is compared with the allows on resolved paths too: one spelled
// through another path than its allow is still held (never classed as
// outside every allow, which would leave it to Deno's checks alone)
#[test]
fn denies_compare_resolved_paths() {
  let dir = aliased_tree("deny");
  for (allow, denied) in [("link", "real/.env"), ("real", "link/.env")] {
    let options = PermissionsOptions {
      allow_read: s(&[&p(&dir, allow)]),
      deny_read: s(&[&p(&dir, denied)]),
      ..Default::default()
    };
    let lin = compile(&options, linux(Some(4)), Enforce);
    assert_eq!(
      deny(&lin.policy.fs.deny_read, &p(&dir, denied)),
      DenyEnforcement::Expand,
      "{allow} / {denied}"
    );
    let mac = compile(&options, macos(), Enforce);
    assert_eq!(
      deny(&mac.policy.fs.deny_read, &p(&dir, denied)),
      DenyEnforcement::Exact,
      "{allow} / {denied}"
    );
    // a write deny reached through an alias: Landlock can't hold it inside
    // the allow, so enforce takes the broker for it
    let options = PermissionsOptions {
      allow_write: s(&[&p(&dir, allow)]),
      deny_write: s(&[&p(&dir, denied)]),
      ..Default::default()
    };
    let lin = compile(&options, linux(Some(4)), Enforce);
    assert_eq!(
      deny(&lin.policy.fs.deny_write, &p(&dir, denied)),
      DenyEnforcement::Broker
    );
    assert_eq!(lin.policy.shape, Shape::ModeB);
    assert_eq!(lin.fatal_gaps().count(), 0, "{:#?}", lin.gaps);
    let mac = compile(&options, macos(), Enforce);
    assert_eq!(
      deny(&mac.policy.fs.deny_write, &p(&dir, denied)),
      DenyEnforcement::Exact
    );
  }
  std::fs::remove_dir_all(&dir).unwrap();
}

// an allow inside a deny of its class is left out of the kernel's scope
// (the deny wins, as in Deno's checks), spelled either way
#[test]
fn allows_inside_a_deny_are_left_out() {
  let dir = aliased_tree("carve");
  for (allow, denied) in [
    ("real/sub", "real"),
    ("link/sub", "real"),
    ("real/sub", "link"),
    ("real", "real"),
  ] {
    for caps in [linux(Some(4)), macos()] {
      let c = compile(
        &PermissionsOptions {
          allow_read: s(&[&p(&dir, allow)]),
          deny_read: s(&[&p(&dir, denied)]),
          allow_write: s(&[&p(&dir, allow)]),
          deny_write: s(&[&p(&dir, denied)]),
          ..Default::default()
        },
        caps,
        Enforce,
      );
      assert_eq!(grants(&c.policy.fs.read), Vec::<PathBuf>::new());
      assert_eq!(grants(&c.policy.fs.write), Vec::<PathBuf>::new());
      assert_eq!(
        deny(&c.policy.fs.deny_read, &p(&dir, denied)),
        DenyEnforcement::Implied
      );
      assert_eq!(
        deny(&c.policy.fs.deny_write, &p(&dir, denied)),
        DenyEnforcement::Implied
      );
      for class in [GapClass::Read, GapClass::Write] {
        assert!(
          c.gaps.iter().any(|g| g.class == class
            && g.resolution == GapResolution::FailClosed
            && g.detail.contains("left out")),
          "{allow} / {denied}: {:#?}",
          c.gaps
        );
      }
      assert_eq!(c.fatal_gaps().count(), 0, "{:#?}", c.gaps);
    }
  }
  // an allow beside the deny stays
  let c = compile(
    &PermissionsOptions {
      allow_read: s(&[&p(&dir, "real/sub"), &p(&dir, "other")]),
      deny_read: s(&[&p(&dir, "real")]),
      ..Default::default()
    },
    linux(Some(4)),
    Enforce,
  );
  assert_eq!(grants(&c.policy.fs.read), vec![dir.join("other")]);
  std::fs::remove_dir_all(&dir).unwrap();
}

// a path whose resolution stops at a folder that can't be searched can't be
// placed: a fail-open gap, so enforce refuses
#[test]
fn unresolvable_paths_fail_closed() {
  use std::os::unix::fs::PermissionsExt;
  let dir = aliased_tree("undecided");
  let locked = dir.join("locked");
  std::fs::create_dir_all(locked.join("in")).unwrap();
  std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000))
    .unwrap();
  // root can search any folder: nothing to measure then
  if std::fs::read_dir(&locked).is_err() {
    let c = compile(
      &PermissionsOptions {
        allow_read: s(&[&p(&dir, "real")]),
        deny_read: s(&[&p(&dir, "locked/in/x")]),
        ..Default::default()
      },
      linux(Some(4)),
      Enforce,
    );
    assert_eq!(
      deny(&c.policy.fs.deny_read, &p(&dir, "locked/in/x")),
      DenyEnforcement::DenoOnly
    );
    assert!(
      c.fatal_gaps().any(|g| g.detail.contains("can't be told")),
      "{:#?}",
      c.gaps
    );
  }
  std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))
    .unwrap();
  std::fs::remove_dir_all(&dir).unwrap();
}

// a machine where a seccomp filter can't be installed (an outer profile
// refuses it): enforce refuses on Linux rather than failing after the
// re-exec
#[test]
fn no_seccomp_refuses_enforce() {
  let caps = Capabilities {
    seccomp: false,
    ..linux(Some(6))
  };
  let c = compile(&PermissionsOptions::default(), caps, Enforce);
  assert!(
    c.fatal_gaps()
      .any(|g| g.class == GapClass::Enforcer && g.rule.contains("seccomp")),
    "{:#?}",
    c.gaps
  );
  let ok = compile(&PermissionsOptions::default(), linux(Some(6)), Enforce);
  assert!(!ok.gaps.iter().any(|g| g.rule.contains("seccomp")));
}

/// Item 451: the runtime's own write needs (the cache overlay, the coverage
/// folder) and the certificate files the environment names are implicit; the
/// overlay only inside this DENO_DIR, the certificates only with network.
#[test]
fn runtime_writes_and_tls_files_are_implicit() {
  for caps in [linux(Some(6)), macos()] {
    let deno_dir = PathBuf::from("/home/me/.cache/deno");
    let overlay = deno_dir.join("sandboxed").join("0123456789abcdef");
    let c = Context {
      deno_dir: Some(deno_dir.clone()),
      cache_overlay: Some(overlay.clone()),
      coverage_dir: Some(PathBuf::from("/project/cov")),
      profile_dirs: vec![PathBuf::from("/project/prof")],
      tls_files: vec![PathBuf::from("/etc/my-ca.pem")],
      origin_data: vec![
        deno_dir.join("location_data").join("abc"),
        PathBuf::from("/elsewhere/location_data/abc"),
      ],
      ..ctx(caps, Enforce)
    };
    let entry = |set: &ImplicitSet, path: &Path| {
      set.entries.iter().find(|e| e.path == path).cloned()
    };
    let none = ImplicitSet::build(&PermissionsOptions::default(), &c);
    let o = entry(&none, &overlay).expect("the overlay");
    assert_eq!(
      (o.need, o.access),
      (ImplicitNeed::CacheOverlay, ImplicitAccess::ReadWrite)
    );
    let cov = entry(&none, Path::new("/project/cov")).expect("coverage");
    assert_eq!(
      (cov.need, cov.access),
      (ImplicitNeed::Coverage, ImplicitAccess::ReadWrite)
    );
    assert!(entry(&none, Path::new("/etc/my-ca.pem")).is_none());
    // this program's origin data, only inside DENO_DIR's location_data
    let origin = deno_dir.join("location_data").join("abc");
    assert_eq!(
      entry(&none, &origin).map(|e| (e.need, e.access)),
      Some((ImplicitNeed::OriginData, ImplicitAccess::ReadWrite))
    );
    assert!(entry(&none, Path::new("/elsewhere/location_data/abc")).is_none());
    let net = ImplicitSet::build(
      &PermissionsOptions {
        allow_net: s(&["127.0.0.1:443"]),
        ..Default::default()
      },
      &c,
    );
    assert!(entry(&net, Path::new("/etc/my-ca.pem")).is_some());
    // the compiled policy writes the overlay and the coverage folder, and
    // nothing else of DENO_DIR
    let policy =
      KernelPolicy::compile(&PermissionsOptions::default(), &none, &c);
    let write = &policy.policy.fs.write;
    assert!(write.covers(&overlay.join("v8_code_cache_v2_denext")));
    assert!(write.covers(Path::new("/project/cov/x.json")));
    assert!(write.covers(Path::new("/project/prof/CPU.1.cpuprofile")));
    assert!(!write.covers(&deno_dir.join("v8_code_cache_v2_denext")));
    assert!(!write.covers(&deno_dir.join("gen")));
    assert!(write.covers(&origin.join("local_storage")));
    assert!(!write.covers(&deno_dir.join("location_data").join("other")));
    assert!(
      !write.covers(&deno_dir.join("sandboxed").join("fedcba9876543210"))
    );
    // an overlay outside DENO_DIR is never added
    let outside = ImplicitSet::build(
      &PermissionsOptions::default(),
      &Context {
        cache_overlay: Some(PathBuf::from("/tmp/sandboxed/0123456789abcdef")),
        ..c.clone()
      },
    );
    assert!(
      !outside
        .entries
        .iter()
        .any(|e| e.need == ImplicitNeed::CacheOverlay)
    );
  }
}

/// Item 451: plain `-A` (compiled only under the harness's forced enforce)
/// holds nothing narrower than the grant: no Unix socket gap, so `enforce`
/// starts in Mode A on every Landlock ABI; `-A` with denies keeps its gap.
#[test]
fn allow_all_has_no_socket_gap() {
  let all = Some(Vec::new());
  let options = PermissionsOptions {
    allow_read: all.clone(),
    allow_write: all.clone(),
    allow_net: all.clone(),
    allow_env: all.clone(),
    allow_run: all.clone(),
    allow_ffi: all.clone(),
    allow_sys: all,
    ..Default::default()
  };
  for abi in [1, 4, 6] {
    let c = ctx(linux(Some(abi)).built(), Enforce);
    let set = ImplicitSet::build(&options, &c);
    let p = KernelPolicy::compile(&options, &set, &c);
    assert_eq!(p.policy.shape, Shape::ModeA, "ABI {abi}");
    assert_eq!(p.policy.unix, UnixPolicy::Open);
    assert_eq!(p.policy.net, NetPolicy::Open);
    assert_eq!(p.fatal_gaps().count(), 0, "ABI {abi}: {:?}", p.gaps);
  }
  let with_deny = PermissionsOptions {
    deny_read: s(&["/secret"]),
    ..options
  };
  let c = ctx(linux(Some(4)).built(), Enforce);
  let set = ImplicitSet::build(&with_deny, &c);
  let p = KernelPolicy::compile(&with_deny, &set, &c);
  assert!(p.gaps.iter().any(|g| g.class == GapClass::Unix));
}

/// denext.30 R4 triage: unscoped read, write and net (short of `-A`) allow
/// every socket in Deno too, so they have no Unix socket gap either and
/// `enforce` (what `auto` chooses) starts in Mode A: the fatal gap made
/// `auto` start a broker that held nothing and slowed every child of the
/// run. A write deny keeps the gap in Mode A (Deno refuses a socket under
/// the deny; the kernel's "every socket" doesn't), and the broker closes
/// it: `enforce` starts Mode B, which decides each Unix socket by Deno's
/// rules, so no Unix gap is left there.
#[test]
fn unscoped_files_and_net_have_no_socket_gap() {
  let all = Some(Vec::new());
  let options = PermissionsOptions {
    allow_read: all.clone(),
    allow_write: all.clone(),
    allow_net: all.clone(),
    allow_env: all.clone(),
    allow_run: all,
    ..Default::default()
  };
  // (before ABI 6 abstract sockets are a gap of their own)
  for abi in [6, 7] {
    let c = ctx(linux(Some(abi)).built(), Enforce);
    let set = ImplicitSet::build(&options, &c);
    let p = KernelPolicy::compile(&options, &set, &c);
    assert_eq!(p.policy.shape, Shape::ModeA, "ABI {abi}");
    assert_eq!(p.policy.unix, UnixPolicy::Open);
    assert_eq!(p.fatal_gaps().count(), 0, "ABI {abi}: {:?}", p.gaps);
    assert!(!p.gaps.iter().any(|g| g.class == GapClass::Unix));
  }
  let with_deny = PermissionsOptions {
    deny_write: s(&["/etc"]),
    ..options
  };
  let c = ctx(linux(Some(6)).built(), Enforce);
  let set = ImplicitSet::build(&with_deny, &c);
  let mode_a = Compiler::new(&with_deny, &set, &c, Shape::ModeA).run();
  assert!(
    mode_a.gaps.iter().any(|g| g.class == GapClass::Unix
      && g.is_fatal_under_enforce()
      && g.exact_with == Some(ExactWith::Broker)),
    "{:?}",
    mode_a.gaps
  );
  let p = KernelPolicy::compile(&with_deny, &set, &c);
  assert_eq!(p.policy.shape, Shape::ModeB);
  assert_eq!(p.policy.unix, UnixPolicy::Open);
  assert!(
    !p.gaps.iter().any(|g| g.class == GapClass::Unix),
    "{:?}",
    p.gaps
  );
}

/// denext.30 R4 triage: the `Deno.cron` scheduler socket the operator names
/// (`DENO_UNSTABLE_CRON_SOCK=unix:<path>`) is the runtime's own connection,
/// which Deno's checks don't gate: a run with no network grant still
/// reaches it (it was refused, and `Deno.cron` never registered), held as
/// a named socket (by path on macOS and under the broker; on Linux Mode A
/// only by opening AF_UNIX, a broker-closable gap).
#[test]
fn cron_socket_is_the_runtimes_own() {
  let sock = PathBuf::from("/run/cron.sock");
  let options = PermissionsOptions::default();
  let with_sock = |caps, mode| {
    let mut c = ctx(caps, mode);
    c.runtime_unix_sockets = vec![sock.clone()];
    compile_in(&options, &c)
  };
  // without the variable: no socket at all
  assert_eq!(
    compile(&options, linux(Some(6)).built(), Enforce)
      .policy
      .unix,
    UnixPolicy::Denied
  );
  let mac = with_sock(macos().built(), Enforce);
  assert_eq!(
    mac.policy.unix,
    UnixPolicy::Paths {
      paths: vec![sock.clone()]
    }
  );
  // Linux: by path where the broker holds it, else by opening AF_UNIX
  // with a gap the broker closes (never refused)
  let on_linux = with_sock(linux(Some(6)).built(), Enforce);
  match &on_linux.policy.unix {
    UnixPolicy::Paths { paths } => assert_eq!(paths, &vec![sock.clone()]),
    UnixPolicy::Open => {
      let g = gap(&on_linux, GapClass::Unix);
      assert_eq!(g.resolution, GapResolution::FailOpen);
      assert_eq!(g.exact_with, Some(ExactWith::Broker));
    }
    other => panic!("the cron socket is refused: {other:?}"),
  }
  if BROKER_NET_BUILT {
    let linux_b = with_sock(linux(Some(6)).built(), Broker);
    assert_eq!(
      linux_b.policy.unix,
      UnixPolicy::Paths {
        paths: vec![sock.clone()]
      }
    );
  }
}

/// denext.30 R4 triage: with no IP socket possible, a Unix socket the
/// grant listens on may listen (listen(2) was refused outright, so
/// `--allow-net-listen=unix:<path>` failed with EPERM under the broker
/// `auto` starts); a connect-only Unix grant still refuses it, and so does
/// any grant that leaves IP sockets possible.
#[test]
fn unix_listen_grant_may_listen() {
  for (options, refused) in [
    (
      PermissionsOptions {
        allow_net_listen: s(&["unix:/run/b.sock"]),
        ..Default::default()
      },
      false,
    ),
    (
      PermissionsOptions {
        allow_net: s(&["unix:/run/b.sock"]),
        ..Default::default()
      },
      false,
    ),
    (
      PermissionsOptions {
        allow_net_connect: s(&["unix:/run/a.sock"]),
        ..Default::default()
      },
      true,
    ),
    (
      PermissionsOptions {
        allow_net_connect: s(&["example.com:443"]),
        allow_net_listen: s(&["unix:/run/b.sock"]),
        ..Default::default()
      },
      true,
    ),
  ] {
    let c = compile(&options, linux(Some(6)).built(), Broker);
    assert_eq!(c.policy.syscalls.refuse_listen, refused, "{options:?}");
  }
}

/// A deny ceiling (D1 / D2, WP-A of denext.30) compiled for `options` on
/// `caps`, as `check_run` does: the ceiling's grant, the context flag, and
/// the mode `auto` chose (enforce).
fn ceiling(options: &PermissionsOptions, caps: Capabilities) -> CompiledPolicy {
  let mut ctx = ctx(caps, KernelSandboxMode::Enforce);
  ctx.deny_ceiling = true;
  compile_in(&deny_ceiling_options(options), &ctx)
}

/// The ceiling's grant: every allow unscoped, every deny kept.
#[test]
fn deny_ceiling_options_keep_only_the_denies() {
  let options = PermissionsOptions {
    allow_read: s(&["/project"]),
    allow_net_connect: s(&["example.com:443"]),
    deny_read: s(&["/home/me/.ssh"]),
    ignore_env: s(&["TOKEN"]),
    prompt: true,
    ..Default::default()
  };
  let c = deny_ceiling_options(&options);
  assert!(options_allow_all(&c));
  assert!(options_have_deny(&c));
  assert_eq!(c.deny_read, options.deny_read);
  assert_eq!(c.ignore_env, options.ignore_env);
  assert_eq!(c.allow_net_connect, None);
  assert!(c.prompt);
}

/// Linux: a deny ceiling holds nothing without the overmount (Landlock
/// needs no-new-privileges, an expansion freezes folders a prompt may
/// write): each deny is a documented Deno-only rule, nothing is refused,
/// no seccomp list, no scrub, Mode A. Whatever the mode says.
#[test]
fn deny_ceiling_linux_is_deno_only_and_touches_nothing() {
  let options = PermissionsOptions {
    allow_read: s(&["/project"]),
    deny_read: s(&["/home/me/.ssh"]),
    deny_write: s(&["/home/me/.ssh"]),
    deny_net: s(&["169.254.169.254"]),
    prompt: true,
    ..Default::default()
  };
  for caps in [linux(Some(6)), linux_userns(6), linux(None)] {
    let c = ceiling(&options, caps);
    let p = &c.policy;
    assert!(p.deny_only && p.deny_ceiling, "{caps:?}");
    assert_eq!(p.shape, Shape::ModeA);
    assert!(p.fs.read.is_all() && p.fs.write.is_all());
    assert_eq!(p.fs.exec, ExecScope::Unrestricted);
    assert_eq!(p.net, NetPolicy::Open);
    assert_eq!(p.unix, UnixPolicy::Open);
    assert_eq!(p.env_scrub, None);
    assert_eq!(
      p.syscalls,
      SyscallPolicy {
        attack_surface: false,
        refuse_exec: false,
        refuse_listen: false,
        clone3_enosys: false,
        refuse_io_uring: false,
        refuse_path_metadata: false,
        refuse_path_truncate: false,
        vsock: true,
        signal_self: false,
      },
      "{caps:?}"
    );
    assert_eq!(
      deny(&p.fs.deny_read, "/home/me/.ssh"),
      DenyEnforcement::DenoOnly
    );
    assert_eq!(
      deny(&p.fs.deny_write, "/home/me/.ssh"),
      DenyEnforcement::DenoOnly
    );
    assert!(!p.deny_ceiling_held(), "{caps:?}");
    assert_eq!(c.fatal_gaps().count(), 0, "{:#?}", c.gaps);
    // reported: the read deny says what would hold it
    assert!(c.gaps.iter().any(|g| g.class == GapClass::Read
      && g.rule == "--deny-read=/home/me/.ssh"
      && g.exact_with == Some(ExactWith::UserNamespaces)));
  }
}

/// The same denies under an explicit enforce (no ceiling) keep enforce's
/// rules: the read deny is expanded, the seccomp list applies.
#[test]
fn deny_only_under_explicit_enforce_is_not_a_ceiling() {
  let options = PermissionsOptions {
    deny_read: s(&["/home/me/.ssh"]),
    ..allow_all()
  };
  let c = compile(&options, linux(Some(6)), KernelSandboxMode::Enforce);
  assert!(c.policy.deny_only && !c.policy.deny_ceiling);
  assert!(c.policy.syscalls.refuse_io_uring);
  // an explicit enforce in a ceiling's context still needs the flag: a
  // context with it and a grant that isn't the ceiling's isn't one
  let mut ctx = ctx(linux(Some(6)), KernelSandboxMode::Enforce);
  ctx.deny_ceiling = true;
  let partial = PermissionsOptions {
    allow_read: s(&["/project"]),
    deny_read: s(&["/project/secret"]),
    ..Default::default()
  };
  assert!(!compile_in(&partial, &ctx).policy.deny_ceiling);
}

/// macOS: Seatbelt holds a deny ceiling exactly (an allow-default profile
/// with the denies), and nothing is refused.
#[test]
fn deny_ceiling_macos_is_exact() {
  let options = PermissionsOptions {
    allow_read: s(&["/Users/me/project"]),
    deny_read: s(&["/Users/me/.ssh"]),
    deny_write: s(&["/Users/me/.zshrc"]),
    deny_net: s(&["169.254.169.254"]),
    prompt: true,
    ..Default::default()
  };
  let c = ceiling(&options, macos());
  let p = &c.policy;
  assert!(p.deny_ceiling);
  assert_eq!(p.shape, Shape::ModeA);
  assert_eq!(
    deny(&p.fs.deny_read, "/Users/me/.ssh"),
    DenyEnforcement::Exact
  );
  assert_eq!(
    deny(&p.fs.deny_write, "/Users/me/.zshrc"),
    DenyEnforcement::Exact
  );
  assert!(p.deny_ceiling_held());
  assert_eq!(c.fatal_gaps().count(), 0, "{:#?}", c.gaps);
  // a host deny is Deno's alone (documented), never the broker
  assert!(has_gap(&c, GapClass::Net, GapResolution::Documented));
  assert_eq!(p.net, NetPolicy::Open);
  assert!(!p.syscalls.attack_surface);
}

/// A ceiling with denies the kernel never holds (env, sys, run) applies
/// nothing on any OS.
#[test]
fn deny_ceiling_with_only_api_denies_holds_nothing() {
  let options = PermissionsOptions {
    deny_env: s(&["TOKEN"]),
    deny_run: s(&["sudo"]),
    deny_sys: s(&["hostname"]),
    prompt: true,
    ..Default::default()
  };
  for caps in [macos(), linux(Some(6))] {
    let c = ceiling(&options, caps);
    assert!(c.policy.deny_ceiling);
    assert!(!c.policy.deny_ceiling_held(), "{caps:?}");
    assert_eq!(c.fatal_gaps().count(), 0, "{:#?}", c.gaps);
  }
}

/// `deno test --junit-path` writes its report through the implicit set.
#[test]
fn test_report_files_are_implicit() {
  let mut ctx = ctx(linux(Some(6)), KernelSandboxMode::Enforce);
  ctx.report_files = vec![PathBuf::from("/project/report.xml")];
  let implicit = ImplicitSet::build(&PermissionsOptions::default(), &ctx);
  assert!(
    implicit
      .entries
      .iter()
      .any(|e| e.need == ImplicitNeed::TestReport
        && e.access == ImplicitAccess::ReadWrite
        && e.path == Path::new("/project/report.xml"))
  );
}
