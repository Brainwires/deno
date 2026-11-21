// Copyright 2018-2026 the Deno authors. MIT license.

//! The kernel-sandbox broker (denext, `--kernel-sandbox=broker`): the
//! unconfined supervisor process that performs, for the confined process it
//! started, the operations the kernel can't hold to Deno's rule exactly.
//!
//! - [`protocol`]: the frames (pure, fuzzed; the definition other broker
//!   operations build on).
//! - [`transport`]: frames over a Unix socket pair, descriptors as
//!   `SCM_RIGHTS`.
//! - [`server`]: the broker's side: a [`server::Handler`] answers each
//!   request; [`fs::FsBroker`] is the handler for the file operations, which
//!   resolves every path with the race-safe opens of
//!   [`crate::checked_open`] against its own copy of the policy.
//! - [`net`]: the network operations (the reserved kinds): their payloads,
//!   the broker's side of them over any `deno_net_route::NetRoute` (the
//!   CLI's network authority), and the confined side's route, which is the
//!   one every socket-opening crate asks through `deno_net_route`.
//! - [`client`]: the confined side: the process-wide [`client::BrokerClient`]
//!   the file-system operations fall back to.
//! - [`dial`]: how a `deno` / `denort` the confined program starts gets a
//!   channel of its own to the same broker.
//!
//! The broker is an authority, not an oracle: it holds a fixed policy, acts
//! itself, hands back only objects it checked, never asks the confined side
//! what is allowed and never widens. See [`protocol`] for the wire format.

#[cfg(unix)]
pub mod client;
#[cfg(unix)]
pub mod dial;
#[cfg(unix)]
pub mod fs;
pub mod net;
pub mod protocol;
#[cfg(unix)]
pub mod server;
#[cfg(unix)]
pub mod transport;

#[cfg(all(test, unix))]
mod tests;
