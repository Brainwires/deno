// Copyright 2018-2026 the Deno authors. MIT license.
// A module the origin app ships (compile.include) and forks with
// `node:child_process`, the way a framework dev server forks its workers:
// the app's own executable runs it headless and it answers over IPC.

import process from "node:process";

process.send({
  ok: true,
  pid: process.pid,
  // The in-process memory serve address names a listener in the parent.
  serveAddress: process.env.DENO_SERVE_ADDRESS ?? null,
  appOrigin: process.env.DENO_DESKTOP_APP_ORIGIN ?? null,
}, () => process.disconnect());
