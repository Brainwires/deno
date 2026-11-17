// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// P6: a deno this confined process starts inherits its grant (the policy
// handed down), even with -A, even with a cleared environment.
const deno = Deno.execPath();
const run = (args: string[], options: Deno.CommandOptions = {}) =>
  new Deno.Command(deno, { args, ...options }).outputSync();

let out = run(["run", "--quiet", "-A", "inside/nested_read.ts"]);
Deno.stdout.writeSync(out.stdout);

out = run(["run", "--quiet", "-A", "inside/nested_read.ts", "cleared env, "], {
  clearEnv: true,
  env: { NO_COLOR: "1", DENO_NO_UPDATE_CHECK: "1" },
});
Deno.stdout.writeSync(out.stdout);

// the nested run's report says it inherited the parent's layer
out = run([
  "run",
  "--quiet",
  "-A",
  "--kernel-sandbox-report",
  "inside/nested_read.ts",
], {
  stdout: "null",
  stderr: "piped",
});
const report = new TextDecoder().decode(out.stderr);
console.log(`report: ${/inherited: 1 kernel sandbox layer/.test(report)}`);
