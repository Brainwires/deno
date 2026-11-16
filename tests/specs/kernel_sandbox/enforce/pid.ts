// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// Confined: prints its pid, then waits for SIGTERM.
Deno.addSignalListener("SIGTERM", () => {
  console.log("got SIGTERM");
  Deno.exit(0);
});
console.log(`pid ${Deno.pid}`);
setTimeout(() => {
  console.log("no signal");
  Deno.exit(1);
}, 30_000);
