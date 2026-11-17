// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// P6: a child killed by a signal a refusal can end it with (SIGSYS: a
// seccomp kill; SIGABRT: macOS's dyld) gets the hint; SIGTERM doesn't.
const term = new Deno.Command("sh", { args: ["-c", "kill -TERM $$"] })
  .outputSync();
console.log(`signal: ${term.signal}`);
const sys = new Deno.Command("sh", {
  args: ["-c", "ulimit -c 0; kill -SYS $$"],
})
  .outputSync();
console.log(`signal: ${sys.signal}`);
