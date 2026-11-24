// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// P6 (D3): with --allow-run=sh, a shell may start; whether it may start
// another program (cat) depends on --kernel-sandbox-exec=strict.
const sh = (script: string) =>
  new Deno.Command("sh", {
    args: ["-c", script],
    stdout: "null",
    stderr: "null",
  }).outputSync().code;
console.log(`sh builtin: exit ${sh("echo hi")}`);
console.log(
  `sh starts cat: ${sh("cat inside/a.txt") === 0 ? "ok" : "refused"}`,
);
// an indirect start: the shell starts env (not on the list), which would
// start cat
console.log(
  `sh starts env cat: ${sh("env cat inside/a.txt") === 0 ? "ok" : "refused"}`,
);
