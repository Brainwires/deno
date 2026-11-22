// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// (through cat, which inherits this process's no-new-privileges and seccomp
// filters: reading /proc/self/status here would need
// --allow-sys=processInfo, which -A next to a deny doesn't grant)
const out = new Deno.Command("cat", { args: ["/proc/self/status"] })
  .outputSync();
console.log(new TextDecoder().decode(out.stdout));
