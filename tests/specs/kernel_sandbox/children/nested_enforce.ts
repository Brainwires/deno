// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// P6: a nested --kernel-sandbox=enforce stacks its own layer (Linux).
const out = new Deno.Command(Deno.execPath(), {
  args: [
    "run",
    "--quiet",
    "--kernel-sandbox=enforce",
    "--allow-ffi",
    "--allow-read=inside/sub",
    "inside/stack.js",
  ],
}).outputSync();
Deno.stdout.writeSync(out.stdout);
Deno.stderr.writeSync(out.stderr);
console.log(`nested exit: ${out.code}`);
