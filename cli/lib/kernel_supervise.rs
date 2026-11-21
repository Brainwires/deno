// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: what the CLI and `denort` share to run under the kernel
//! sandbox's supervising broker (Mode B).
//!
//! - The supervisor's side: [`serve_broker`] answers a plan's broker policy
//!   on a channel (the file operations, and the network when the broker
//!   holds it).
//! - The worker's side: [`install_channel`] makes a channel this process's
//!   broker, [`check_broker`] refuses a plan whose broker doesn't answer.
//! - Children: [`offer_dial`] leaves the processes a brokered worker starts
//!   a way to the same broker, [`adopt_inherited_dial`] takes it in a
//!   `deno` / `denort` started there, and [`drop_inherited_dial`] gives it
//!   up in one that confines itself.
//!
//! The CLI reaches the supervisor by re-executing itself
//! (`deno_sandbox::supervise::exec_supervisor`); a compiled program forks
//! in place ([`fork_supervised`]).

#![allow(
  clippy::disallowed_methods,
  reason = "the supervisor and its worker act on the real process and environment"
)]

use std::os::fd::OwnedFd;
use std::sync::OnceLock;

use deno_runtime::deno_permissions::PermissionsOptions;
use deno_runtime::deno_permissions::kernel_broker;
use deno_runtime::deno_permissions::kernel_broker::dial::DIAL_ENV_VAR;
use deno_runtime::deno_permissions::kernel_broker::dial::Dial;
use deno_sandbox::plan::BrokerNet;
use deno_sandbox::plan::BrokerPlan;
use deno_sandbox::plan::EnforcePlan;

use crate::kernel_policy::CompiledPolicy;
use crate::kernel_policy::NetPolicy;

/// The channels one supervisor serves at once: the worker's own (a control
/// channel and up to 16 working ones) and as many again for each `deno` /
/// `denort` it starts that takes the dial. One thread each, parked in a
/// read. Past it a new channel fails with `EMFILE` and the asker goes
/// without (its brokered operations are refused).
pub const MAX_SUPERVISOR_CHANNELS: usize = 1024;

/// Fills in the broker's copy of the network grant, as the flags wrote it,
/// when the policy's network is the broker's. Both images of a CLI run call
/// this with the same options, so their plans still compare.
pub fn fill_broker_net(
  plan: &mut EnforcePlan,
  compiled: &CompiledPolicy,
  options: &PermissionsOptions,
  inspector_port: Option<u16>,
) {
  if !matches!(compiled.policy.net, NetPolicy::Broker { .. }) {
    return;
  }
  let Some(broker) = &mut plan.broker else {
    return;
  };
  let mut allow_listen = options.allow_net_listen.clone();
  if let Some(port) = inspector_port {
    // the inspector's own listener, which no flag grants
    allow_listen
      .get_or_insert_with(Vec::new)
      .push(format!("127.0.0.1:{port}"));
  }
  broker.net = Some(BrokerNet {
    allow: options.allow_net.clone(),
    deny: options.deny_net.clone(),
    allow_connect: options.allow_net_connect.clone(),
    deny_connect: options.deny_net_connect.clone(),
    allow_listen,
    deny_listen: options.deny_net_listen.clone(),
  });
}

/// The supervisor's broker for `broker`, serving `sock` (the first
/// channel) on its own threads: the file operations against the run's read
/// and write rules, and the network operations through the authority over
/// its network lists when the broker holds the network. Returns once the
/// first channel is being served.
pub fn serve_broker(broker: &BrokerPlan, sock: OwnedFd) -> std::io::Result<()> {
  use kernel_broker::fs::FsBroker;
  use kernel_broker::fs::FsPolicy;
  use kernel_broker::server::Limits;
  use kernel_broker::server::Server;

  let policy = FsPolicy {
    read: broker.read.clone(),
    deny_read: broker.deny_read.clone(),
    write: broker.write.clone(),
    deny_write: broker.deny_write.clone(),
  };
  let net = match &broker.net {
    Some(net) => Some(
      crate::kernel_net_broker::route_for(net, &policy)
        .map_err(|err| std::io::Error::other(err.to_string()))?,
    ),
    None => None,
  };
  let server = Server::with_limits(
    crate::kernel_net_broker::BrokerHandler {
      fs: FsBroker::new(&policy)?,
      net,
    },
    Limits {
      max_channels: MAX_SUPERVISOR_CHANNELS,
    },
  );
  server.spawn(sock).map(|_| ())
}

/// Makes `sock` this process's broker: the file operations fall back to
/// it, and with `net` every socket of this process is asked of it from
/// here on (call before any module or user code runs). A process that
/// already has a broker keeps it (`false`).
pub fn install_channel(sock: OwnedFd, net: bool) -> bool {
  use std::os::fd::AsRawFd;
  // the channel is the runtime's: program code never adopts it as one of
  // the descriptors it inherited (through it a program with fewer
  // permissions than the broker's policy could ask for more than its own
  // checks allow)
  deno_runtime::deno_io::exclude_inherited_fd(sock.as_raw_fd());
  if kernel_broker::client::install(sock).is_err() {
    return false;
  }
  if net {
    deno_net_route::install(Box::new(kernel_broker::net::BrokerRoute::new()));
  }
  true
}

/// A plan with a broker runs only with its broker answering (the plan's
/// kernel rules alone would refuse what Deno's rules allow): this process
/// must have a channel, and the broker must serve what the plan leans on.
pub fn check_broker(plan: &EnforcePlan) -> Result<(), String> {
  use kernel_broker::protocol::features;
  let Some(broker_plan) = &plan.broker else {
    return Ok(());
  };
  let Some(broker) = kernel_broker::client::get() else {
    return Err("this process has no channel to its broker".to_string());
  };
  let net = broker_plan.net.is_some();
  let net_features = kernel_broker::net::FEATURES;
  match broker.hello() {
    Ok(served) if net && served & net_features != net_features => {
      Err("the broker doesn't serve network operations".to_string())
    }
    Ok(served) if served & features::FS != 0 => Ok(()),
    Ok(_) => Err("the broker doesn't serve file operations".to_string()),
    Err(err) => Err(format!("the broker doesn't answer: {err}")),
  }
}

/// A compiled program's supervisor, in place: forks; **returns in the
/// child**, the worker, with its end of the broker channel; the parent
/// serves `plan`'s broker and exits as the worker exits (it never
/// returns). Call first thing in `main`, while the process has one thread
/// and before it confines itself. `Err`: nothing was forked.
pub fn fork_supervised(plan: &EnforcePlan) -> Result<OwnedFd, String> {
  let Some(broker) = plan.broker.clone() else {
    return Err("the plan has no broker policy".to_string());
  };
  deno_sandbox::supervise::fork_worker(move |sock| serve_broker(&broker, sock))
    .map_err(|err| err.to_string())
}

// ---------------------------------------------------------------------------
// Children of a brokered run
// ---------------------------------------------------------------------------

/// The dial this process hands the processes it starts.
static OFFERED: OnceLock<Dial> = OnceLock::new();

fn pin_for_children(value: &str) {
  // SAFETY: called while the process has one thread (the top of `main`)
  unsafe { std::env::set_var(DIAL_ENV_VAR, value) };
  // a child started with an environment of its own still gets it
  deno_runtime::deno_process::repin_child_env_var(
    DIAL_ENV_VAR.into(),
    value.into(),
  );
}

/// The worker of a brokered run leaves the processes it starts a way to
/// its broker ([`Dial`]): a `deno` or `denort` among them takes it
/// ([`adopt_inherited_dial`]) and gets what this process's own operations
/// get; any other program never looks. The broker answers every channel
/// against the one policy it holds, so the dial hands a child nothing that
/// native code in this process couldn't ask for itself.
///
/// Call once this process has its broker, while it has one thread. A dial
/// that can't be made is left out: the children then meet the kernel's
/// refusals alone, as before.
pub fn offer_dial() {
  let Some(client) = kernel_broker::client::get() else {
    return;
  };
  if OFFERED.get().is_some() {
    return;
  }
  if let Ok(dial) = Dial::create(client) {
    for fd in dial.fds() {
      deno_runtime::deno_io::exclude_inherited_fd(fd);
    }
    pin_for_children(&dial.env_value());
    let _ = OFFERED.set(dial);
  }
}

/// Gives up a dial this process inherited: its descriptors are closed and
/// its variable removed, here and for the processes it starts. A process
/// that applies a kernel sandbox of its own calls this before it does: its
/// grant may be narrower than the one the inherited broker answers for, and
/// would not hold against that broker's answers. Call while the process
/// has one thread.
pub fn drop_inherited_dial() {
  let Some(value) = std::env::var_os(DIAL_ENV_VAR) else {
    return;
  };
  // SAFETY: called while the process has one thread (the top of `main`)
  unsafe { std::env::remove_var(DIAL_ENV_VAR) };
  if let Some(Ok(dial)) = value.to_str().map(Dial::from_env_value) {
    dial.close();
  }
}

/// A `deno` / `denort` started inside a brokered run takes the dial its
/// parent left: a channel of its own to the same broker, which then
/// answers this process as it answers its parent (the file operations, and
/// every socket when that broker holds the network). The dial stays open
/// for the processes this one starts.
///
/// `confined`: the kernel itself reports this process inside a kernel
/// sandbox it inherited (the caller measured it: an accepted carrier). The
/// variable alone is never enough; without the sandbox there is no broker
/// to need. Call first thing in `main`, before any module or user code
/// runs, and never in a process that applies a sandbox of its own
/// ([`drop_inherited_dial`]). Returns whether this process now has the
/// broker; when it doesn't, nothing changed (the kernel refuses what it
/// refused).
pub fn adopt_inherited_dial(confined: bool) -> bool {
  let Some(value) = std::env::var_os(DIAL_ENV_VAR) else {
    return false;
  };
  if !confined || kernel_broker::client::get().is_some() {
    return false;
  }
  let Some(Ok(dial)) = value.to_str().map(Dial::from_env_value) else {
    // not a dial (a program in between closed it): nothing to keep
    // SAFETY: called while the process has one thread (the top of `main`)
    unsafe { std::env::remove_var(DIAL_ENV_VAR) };
    return false;
  };
  for fd in dial.fds() {
    deno_runtime::deno_io::exclude_inherited_fd(fd);
  }
  let Ok(channel) = dial.connect() else {
    return false;
  };
  {
    use std::os::fd::AsRawFd;
    deno_runtime::deno_io::exclude_inherited_fd(channel.as_raw_fd());
  }
  if kernel_broker::client::install(channel).is_err() {
    return false;
  }
  // what the broker serves decides what is asked of it: the network only
  // when it holds the network (a run whose broker holds files alone makes
  // its own sockets). A broker that doesn't answer leaves a client whose
  // every request fails: closed, as without one.
  let served = kernel_broker::client::get()
    .and_then(|client| client.hello().ok())
    .unwrap_or(0);
  let net_features = kernel_broker::net::FEATURES;
  if served & net_features == net_features {
    deno_net_route::install(Box::new(kernel_broker::net::BrokerRoute::new()));
  }
  if let Some(value) = value.to_str() {
    pin_for_children(value);
  }
  true
}

/// The descriptor numbers [`DIAL_ENV_VAR`] names in this process's
/// environment, unchecked: they are the dial's, not descriptors the
/// program inherited for itself.
pub fn inherited_dial_fds() -> Vec<i32> {
  std::env::var(DIAL_ENV_VAR)
    .map(|value| Dial::named_fds(&value))
    .unwrap_or_default()
}
