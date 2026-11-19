// Copyright 2018-2026 the Deno authors. MIT license.

//! The broker's network authority over this run's grant (denext): Deno's
//! own permission checks as the [`NetDecider`] of
//! `deno_sandbox::net_authority`, so the process outside the sandbox and
//! the checks inside it read the grant with the same code.
//!
//! The decider is built from the permission options the run resolved, with
//! prompting off: every question has an answer, and nothing the confined
//! process does later (a prompt it answers, `Deno.permissions.revoke()`)
//! reaches it. It can only refuse what the options don't grant.

use std::borrow::Cow;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

use deno_core::error::AnyError;
use deno_core::parking_lot::Mutex;
use deno_runtime::deno_permissions::NetDescriptor;
use deno_runtime::deno_permissions::OpenAccessKind;
use deno_runtime::deno_permissions::PermissionDescriptorParser;
use deno_runtime::deno_permissions::Permissions;
use deno_runtime::deno_permissions::PermissionsContainer;
use deno_runtime::deno_permissions::PermissionsOptions;
use deno_runtime::deno_permissions::QueryDescriptor;
use deno_sandbox::net_authority::InProcessRoute;
use deno_sandbox::net_authority::NetAuthority;
use deno_sandbox::net_authority::NetDecider;

/// The name the broker's checks give as the calling API.
const API_NAME: &str = "the kernel sandbox's broker";

/// The internal switch that routes this process's sockets through an
/// authority in the process itself (value `in-process`): no boundary, so it
/// confines nothing; it is how the test suites exercise every routed socket
/// path, and the broker's decisions, without a supervisor. It can only
/// narrow: the authority holds the run's own grant.
pub const NET_ROUTE_TEST_ENV_VAR: &str = "DENO_INTERNAL_KERNEL_NET_ROUTE";

/// One side's list: `None` no grant, empty unscoped.
fn entries(lists: &[&Option<Vec<String>>]) -> Option<Vec<NetDescriptor>> {
  let mut out = Vec::new();
  let mut any = false;
  for list in lists.iter().copied().flatten() {
    any = true;
    if list.is_empty() {
      return Some(Vec::new());
    }
    out.extend(
      list
        .iter()
        .filter_map(|entry| NetDescriptor::parse_for_list(entry).ok()),
    );
  }
  any.then_some(out)
}

/// Deno's network rule for one run, as the broker asks it.
pub struct PermissionsDecider {
  permissions: Mutex<PermissionsContainer>,
  /// Every allow entry of either direction (`None`: no network grant;
  /// empty: an unscoped one).
  allow: Option<Vec<NetDescriptor>>,
  /// Every deny entry of either direction.
  deny: Vec<NetDescriptor>,
  open_datagrams: bool,
}

impl PermissionsDecider {
  pub fn new(
    parser: Arc<dyn PermissionDescriptorParser>,
    options: &PermissionsOptions,
  ) -> Result<Self, AnyError> {
    let options = PermissionsOptions {
      prompt: false,
      ..options.clone()
    };
    let permissions = Permissions::from_options(parser.as_ref(), &options)?;
    let denies = [
      &options.deny_net,
      &options.deny_net_connect,
      &options.deny_net_listen,
    ];
    let connect_unscoped = [&options.allow_net, &options.allow_net_connect]
      .into_iter()
      .flatten()
      .any(|list| list.is_empty());
    Ok(Self {
      permissions: Mutex::new(PermissionsContainer::new(parser, permissions)),
      allow: entries(&[
        &options.allow_net,
        &options.allow_net_connect,
        &options.allow_net_listen,
      ]),
      deny: entries(&denies).unwrap_or_default(),
      // any deny rule (an empty list denies everything) scopes the grant
      open_datagrams: connect_unscoped && denies.iter().all(|d| d.is_none()),
    })
  }
}

impl NetDecider for PermissionsDecider {
  fn connect(&self, host: &str, port: u16) -> bool {
    self
      .permissions
      .lock()
      .check_net_connect(&(host, Some(port)), API_NAME)
      .is_ok()
  }

  fn connect_resolved(&self, ip: IpAddr, port: u16) -> bool {
    self
      .permissions
      .lock()
      .check_net_connect_resolved(&ip, port, API_NAME)
      .is_ok()
  }

  fn listen(&self, host: &str, port: u16) -> bool {
    self
      .permissions
      .lock()
      .check_net_listen(&(host, Some(port)), API_NAME)
      .is_ok()
  }

  fn listen_resolved(&self, ip: IpAddr, port: u16) -> bool {
    self
      .permissions
      .lock()
      .check_net_listen_resolved(&ip, port, API_NAME)
      .is_ok()
  }

  fn names(&self, host: &str) -> bool {
    let Some(allow) = &self.allow else {
      return false;
    };
    let Ok(NetDescriptor(host, _)) = NetDescriptor::parse_for_query(host)
    else {
      return false;
    };
    // a deny entry with no port covers the host whole
    let whole = NetDescriptor(host.clone(), None);
    if self
      .deny
      .iter()
      .any(|deny| deny.1.is_none() && whole.matches_deny(deny))
    {
      return false;
    }
    // named for some port: asked with each entry's own
    allow.is_empty()
      || allow
        .iter()
        .any(|entry| NetDescriptor(host.clone(), entry.1).matches_allow(entry))
  }

  fn open_datagrams(&self) -> bool {
    self.open_datagrams
  }

  fn unix(&self, path: &Path) -> bool {
    let mut permissions = self.permissions.lock();
    // what `Deno.connect({ transport: "unix" })` checks: read and write on
    // the path, and the network grant for it
    let Ok(checked) = permissions.check_open(
      Cow::Borrowed(path),
      OpenAccessKind::ReadWriteNoFollow,
      Some(API_NAME),
    ) else {
      return false;
    };
    let resolved = checked.into_path();
    permissions
      .check_net_unix_socket_connect(&resolved, Some(API_NAME))
      .is_ok()
  }
}

/// The network authority of a run with these permission options.
pub fn authority(
  parser: Arc<dyn PermissionDescriptorParser>,
  options: &PermissionsOptions,
) -> Result<NetAuthority, AnyError> {
  Ok(NetAuthority::new(Arc::new(PermissionsDecider::new(
    parser, options,
  )?)))
}

/// Installs the in-process route when [`NET_ROUTE_TEST_ENV_VAR`] asks for
/// it (any other value is ignored with a warning).
pub fn install_test_route(
  parser: Arc<dyn PermissionDescriptorParser>,
  options: &PermissionsOptions,
) -> Result<(), AnyError> {
  #[allow(clippy::disallowed_methods, reason = "an internal test switch")]
  let Some(value) = std::env::var_os(NET_ROUTE_TEST_ENV_VAR) else {
    return Ok(());
  };
  if value != "in-process" {
    log::warn!(
      "{} ignoring {NET_ROUTE_TEST_ENV_VAR}: the only value is \"in-process\"",
      crate::colors::yellow("Warning"),
    );
    return Ok(());
  }
  if deno_net_route::route().is_none() {
    deno_net_route::install(Box::new(InProcessRoute(Arc::new(authority(
      parser, options,
    )?))));
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use deno_runtime::deno_permissions::RuntimePermissionDescriptorParser;

  use super::*;

  fn decider(options: PermissionsOptions) -> PermissionsDecider {
    let parser = Arc::new(RuntimePermissionDescriptorParser::new(
      sys_traits::impls::RealSys,
    ));
    PermissionsDecider::new(parser, &options).unwrap()
  }

  fn list(entries: &[&str]) -> Option<Vec<String>> {
    Some(entries.iter().map(|e| e.to_string()).collect())
  }

  #[test]
  fn the_grant_is_read_as_denos_checks_read_it() {
    let d = decider(PermissionsOptions {
      allow_net: list(&[
        "example.com:443",
        "*.cdn.test",
        "10.0.0.0/8:5432",
        "127.0.0.1:8000",
      ]),
      deny_net: list(&["bad.cdn.test", "10.9.9.9"]),
      ..Default::default()
    });
    // names, ports, wildcards, subnets
    assert!(d.connect("example.com", 443));
    assert!(!d.connect("example.com", 80));
    assert!(!d.connect("example.org", 443));
    assert!(d.connect("a.cdn.test", 1234));
    assert!(!d.connect("cdn.example", 1234));
    assert!(d.connect("10.1.2.3", 5432));
    assert!(!d.connect("10.1.2.3", 80));
    assert!(!d.connect("11.1.2.3", 5432));
    // denies, by name and by the address connected to
    assert!(!d.connect("bad.cdn.test", 443));
    assert!(!d.connect_resolved("10.9.9.9".parse().unwrap(), 5432));
    assert!(d.connect_resolved("10.1.2.3".parse().unwrap(), 5432));
    // --allow-net grants both directions
    assert!(d.listen("127.0.0.1", 8000));
    assert!(!d.listen("127.0.0.1", 8001));
    assert!(!d.listen("0.0.0.0", 8000));

    // what may be resolved: named for some port, not denied whole
    assert!(d.names("example.com"));
    assert!(d.names("x.cdn.test"));
    assert!(d.names("10.1.2.3"));
    assert!(d.names("127.0.0.1"));
    assert!(!d.names("example.org"));
    assert!(!d.names("bad.cdn.test"));
    assert!(!d.names("10.9.9.9"));
    assert!(!d.names("169.254.169.254"));
    assert!(!d.open_datagrams());
  }

  #[test]
  fn directions_and_unscoped_grants() {
    let d = decider(PermissionsOptions {
      allow_net_connect: list(&["db.test:5432"]),
      allow_net_listen: list(&["localhost:8000"]),
      ..Default::default()
    });
    assert!(d.connect("db.test", 5432));
    assert!(!d.listen("db.test", 5432));
    assert!(d.listen("localhost", 8000));
    assert!(!d.connect("localhost", 8000));
    assert!(d.names("db.test") && d.names("localhost"));

    let none = decider(PermissionsOptions::default());
    assert!(!none.connect("example.com", 443));
    assert!(!none.names("example.com"));
    assert!(!none.open_datagrams());

    let open = decider(PermissionsOptions {
      allow_net: Some(vec![]),
      ..Default::default()
    });
    assert!(open.connect("anything.test", 1));
    assert!(open.names("anything.test"));
    assert!(open.open_datagrams());

    // a deny scopes an unscoped grant: a socket that can send anywhere
    // doesn't hold it
    let denied = decider(PermissionsOptions {
      allow_net: Some(vec![]),
      deny_net: list(&["169.254.169.254"]),
      ..Default::default()
    });
    assert!(denied.connect("example.com", 443));
    assert!(!denied.connect("169.254.169.254", 80));
    assert!(!denied.names("169.254.169.254"));
    assert!(!denied.open_datagrams());

    // listening unscoped doesn't open datagrams: sending is connecting
    let listen_only = decider(PermissionsOptions {
      allow_net_listen: Some(vec![]),
      ..Default::default()
    });
    assert!(!listen_only.open_datagrams());
  }

  #[test]
  fn prompting_is_off_whatever_the_run_says() {
    let d = decider(PermissionsOptions {
      prompt: true,
      ..Default::default()
    });
    // answered at once, with no: a prompt would block the test
    assert!(!d.connect("example.com", 443));
    assert!(!d.unix(Path::new("/var/run/docker.sock")));
  }
}
