// Copyright 2018-2026 the Deno authors. MIT license.
// A module the kernelsandbox app ships and forks (a sidecar): it says what
// holds it, and tries the same native write outside the grant as the app.

import process from "node:process";

const params = JSON.parse(process.env.E2E_KS_CHILD ?? "{}");
let native = "no path";
if (params.outside) {
  try {
    const lib = Deno.dlopen(
      Deno.build.os === "darwin" ? "libSystem.dylib" : "libc.so.6",
      {
        creat: { parameters: ["buffer", "u32"], result: "i32" },
        close: { parameters: ["i32"], result: "i32" },
      },
    );
    const fd = lib.symbols.creat(
      new TextEncoder().encode(`${params.outside}\0`),
      0o600,
    );
    if (fd >= 0) lib.symbols.close(fd);
    native = fd >= 0 ? "written" : "refused";
  } catch (e) {
    native = `error: ${e?.name}: ${e?.message}`;
  }
}
process.send(
  { ok: true, pid: process.pid, native },
  () => process.disconnect(),
);
