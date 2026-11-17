// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// A deno started through a launcher that drops the policy's carrier
// (DENEXT.md item 450). `env -i` clears the variable but keeps the
// descriptor: the nested deno finds it again and keeps the parent's grant.
// A shell that closes the descriptor (as Python's close_fds does): the
// nested deno knows this runtime's sandbox holds it (its marker) but not the
// grant, so it refuses every permission, saying why.
const deno = Deno.execPath();
const decode = (b: Uint8Array) => new TextDecoder().decode(b);

let out = new Deno.Command("/usr/bin/env", {
  args: [
    "-i",
    "NO_COLOR=1",
    deno,
    "run",
    "--quiet",
    "-A",
    "inside/nested_read.ts",
    "env -i, ",
  ],
}).outputSync();
Deno.stdout.writeSync(out.stdout);

out = new Deno.Command("/bin/bash", {
  args: [
    "-c",
    'eval "exec ${DENO_INTERNAL_KERNEL_POLICY:-999}<&-"; ' +
    'unset DENO_INTERNAL_KERNEL_POLICY; exec "$@"',
    "sh",
    deno,
    "run",
    "--quiet",
    "-A",
    "inside/closed_read.ts",
    "closed, ",
  ],
  stderr: "piped",
}).outputSync();
Deno.stdout.writeSync(out.stdout);
console.log(
  `closed warning: ${
    decode(out.stderr).includes(
      "confined by an ancestor's kernel sandbox; grants unknown",
    )
  }`,
);
