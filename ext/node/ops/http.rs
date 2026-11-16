// Copyright 2018-2026 the Deno authors. MIT license.

use std::net::IpAddr;

use deno_core::OpState;
use deno_core::op2;
use deno_permissions::PermissionCheckError;
use deno_permissions::PermissionsContainer;

// When a node:http / node:https request is routed through a proxy, the socket
// is connected to the proxy endpoint, so the proxy is the only host the connect
// op permission-checks. Without this, `--allow-net=<proxy>` alone would let a
// request reach a target host that is outside `--allow-net` or explicitly in
// `--deny-net`. Enforce `--allow-net` for the request target here, mirroring
// the target check fetch() performs, before the connection to the proxy is
// established.
//
// This fails closed: a denied or unparseable target propagates its error,
// matching check_net_connect_url() in fetch(). Targets node:http rejects on its
// own (invalid header characters such as CR/LF, which would also fail the host
// parser) are filtered out before this op is called, so they surface as
// ERR_INVALID_CHAR rather than being masked here.
//
// The name check above cannot enforce a deny rule written as an IP address
// (`--deny-net=10.0.0.5`) against a target given as a name: the proxy resolves
// it, never this process. As fetch()'s proxy connector does, resolve the
// target here and compare every address with those rules, and fail closed
// when it does not resolve locally, since the proxy would resolve it on its
// own and connect wherever it points (unless the operator set
// `DENO_PROXY_RESOLVES_NAMES=1`, see `deno_permissions::proxy_resolves_names`).
// The lookup runs only when such a rule could apply to the port, and blocks
// the JS thread for the resolver's time: node:http checks the target
// synchronously, in the ClientRequest constructor, which has to throw a
// refusal there, so the check can't move off-thread without deferring the
// whole request. A slow resolver delays only proxied requests under an
// IP-address deny rule.
//
// Residual risk: the proxy repeats the lookup, so an answer that differs
// between the local resolver and the proxy's (split-horizon DNS, rebinding
// between the two lookups) is not caught. IP-address deny rules are a hard
// boundary only for direct connections.
#[op2(fast, stack_trace)]
pub fn op_node_http_check_proxy_net(
  state: &mut OpState,
  #[string] hostname: &str,
  port: u16,
  #[string] api_name: &str,
) -> Result<(), PermissionCheckError> {
  let permissions = state.borrow_mut::<PermissionsContainer>();
  permissions.check_net_connect(&(hostname, Some(port)), api_name)?;
  check_proxied_target_resolved(permissions, hostname, port, api_name)
}

fn check_proxied_target_resolved(
  permissions: &mut PermissionsContainer,
  hostname: &str,
  port: u16,
  api_name: &str,
) -> Result<(), PermissionCheckError> {
  if !permissions.has_net_connect_ip_deny_rules(port) {
    return Ok(());
  }
  let host = hostname
    .strip_prefix('[')
    .and_then(|h| h.strip_suffix(']'))
    .unwrap_or(hostname);
  let ips: Vec<IpAddr> = match host.parse::<IpAddr>() {
    Ok(ip) => vec![ip],
    // The resolver the connect ops use (`*.localhost` is the loopback).
    Err(_) => deno_net::resolve_addr::resolve_addr_sync(host, port)
      .map(|addrs| addrs.map(|addr| addr.ip()).collect())
      .unwrap_or_default(),
  };
  if ips.is_empty() {
    return permissions.check_net_connect_unresolved(host, port, api_name);
  }
  for ip in ips {
    permissions.check_net_connect_resolved(&ip, port, api_name)?;
  }
  Ok(())
}
