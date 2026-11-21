// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// The confined worker of a brokered run whose supervisor is PID 1.
console.log(`the supervisor is PID 1: ${Deno.ppid === 1}`);
console.log(`the program is not: ${Deno.pid !== 1}`);
Deno.addSignalListener("SIGTERM", () => {
  console.log("the program got SIGTERM");
  Deno.exit(7);
});
console.log("ready");
setInterval(() => {}, 1000);
