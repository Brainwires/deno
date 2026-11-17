// Copyright 2018-2026 the Deno authors. MIT license.
// A compiled program (`denort`) started inside the sandbox honours the
// inherited grant (DENEXT.md item 450): its baked -A is intersected with the
// parent's, so a read outside it is NotCapable, not the kernel's bare
// EACCES. Through a shell that closes the carrier it refuses everything.
const prog = "./inside/prog";
let out = new Deno.Command(prog, { args: ["compiled, "] }).outputSync();
Deno.stdout.writeSync(out.stdout);
out = new Deno.Command("/bin/bash", {
  args: [
    "-c",
    'eval "exec ${DENO_INTERNAL_KERNEL_POLICY:-999}<&-"; ' +
    'unset DENO_INTERNAL_KERNEL_POLICY; exec "$@"',
    "sh",
    prog,
    "compiled closed, ",
  ],
  stderr: "null",
}).outputSync();
Deno.stdout.writeSync(out.stdout);
