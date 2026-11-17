// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// macOS: a deno under someone else's Seatbelt profile (an agent's sandbox,
// here a permissive sandbox-exec one) with no carrier is not this runtime's
// sandbox: it runs with its own grant, never failing closed (DENEXT.md item
// 450).
const out = new Deno.Command("/usr/bin/sandbox-exec", {
  args: [
    "-p",
    "(version 1)(allow default)",
    Deno.execPath(),
    "run",
    "--quiet",
    "-A",
    "inside/nested_read.ts",
    "foreign, ",
  ],
  stderr: "piped",
}).outputSync();
Deno.stdout.writeSync(out.stdout);
console.log(
  `foreign warning: ${
    new TextDecoder().decode(out.stderr).includes("grants unknown")
  }`,
);
