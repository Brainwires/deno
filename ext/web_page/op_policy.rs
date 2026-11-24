// Copyright 2018-2026 the Deno authors. MIT license.

//! The ops a page runtime registers and takes back.
//!
//! An extension registers all of its ops or none, and deno_core registers
//! its built-in table in every `JsRuntime`. Some of those ops have no place
//! in a page: the `Deno.webrtc` server extension (sockets on ports the
//! program chooses, RTP written and read by hand, the engine's log level),
//! the fake capture devices, and built-in ops that end the process, write to
//! its stdout or load code. Page JavaScript has no reference to any op, so
//! these were unreachable already; the extension's op middleware
//! ([`middleware`]) also takes their implementation away, so that reaching
//! one (a bug in an extension script, a leaked reference) throws `op is
//! disabled` instead of doing the work (`OpDecl::disable`; an op's fast-call
//! variant, which V8 uses from optimized code, becomes a function that does
//! nothing). The names stay in the op table.
//!
//! `99_page_main.js` calls every op of [`DISABLED_OPS`] once while it still
//! can, and fails the bootstrap if one does not throw. All of them are
//! synchronous ops: an async op's JavaScript stub would turn the throw into
//! a rejected promise, which that check does not look at.

use deno_core::OpDecl;

/// The ops whose implementation the middleware replaces with one that
/// throws.
pub(crate) const DISABLED_OPS: &[&str] = &[
  // deno_core. `op_panic` ends the process. `op_eval_context` and the two
  // `op_import_sync*` compile and run code under a name the caller chooses
  // (the REPL's and `require()`'s entry points). `op_resources` and
  // `op_is_terminal` describe the host's resource table. No script of the
  // extensions a page runtime is made of calls any of them.
  "op_panic",
  "op_eval_context",
  "op_import_sync",
  "op_import_sync_with_source",
  "op_resources",
  "op_is_terminal",
  // deno_webrtc: `Deno.webrtc.createNetwork()` (binds UDP and TCP sockets,
  // mDNS, 1:1 NAT addresses) and its `Network` object.
  "op_webrtc_network_create",
  "op_webrtc_network_addresses",
  "op_webrtc_network_tcp_addresses",
  "op_webrtc_network_close",
  // `Deno.webrtc.setLogLevel()`: process-wide, and the log goes to stderr.
  "op_webrtc_set_log_level",
  // `Deno.webrtc.EncodedMediaTrack` (frames and RTP packets the program
  // writes), `Deno.webrtc.readRtp()` and `readFrames()`.
  "op_webrtc_sender_write_rtp",
  "op_webrtc_sender_write_frame",
  "op_webrtc_receiver_read_rtp",
  "op_webrtc_receiver_read_frames",
  // The fake capture devices of `navigator.mediaDevices`, which a page
  // runtime does not have.
  "op_webrtc_fake_media_devices",
  "op_webrtc_sender_write_fake",
  "op_webrtc_sender_fake_frame",
  // What `deno_runtime`'s bootstrap calls to turn its lazy WebRTC globals
  // into data properties; a page's globals are data properties from the
  // start.
  "op_webrtc_data_globals",
  // Called by no script.
  "op_webrtc_sender_reserve_frame",
  "op_webrtc_pc_max_message_size",
];

/// The extension's op middleware: deno_core hands it every op of the
/// runtime, its own built-in ones included, before the op table is made.
pub(crate) fn middleware(op: OpDecl) -> OpDecl {
  match op.name {
    // The process's stdout is Thalora's MCP channel. `core.print` (an
    // internal error of `deno_web`'s streams is reported with it) goes to
    // the page's console instead.
    "op_print" => crate::host_ops::op_web_page_print(),
    name if DISABLED_OPS.contains(&name) => op.disable(),
    _ => op,
  }
}
