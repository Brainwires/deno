// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// P6: a grandchild that orphans itself. The shell starts a subshell in the
// background and exits; the subshell waits for this process to exit too,
// then reads a file outside the grant and records the result.
const script = [
  "(",
  'while kill -0 "$DENO_PID" 2>/dev/null; do sleep 0.1; done;',
  "cat outside.txt >/dev/null 2>&1;",
  'echo "read outside: $?" > inside/orphan_result.txt;',
  "sleep 30",
  ") </dev/null >/dev/null 2>&1 &",
  "echo $!",
].join(" ");
const out = new Deno.Command("sh", {
  args: ["-c", script],
  env: { DENO_PID: String(Deno.pid) },
}).outputSync();
console.log(`grandchild ${new TextDecoder().decode(out.stdout).trim()}`);
