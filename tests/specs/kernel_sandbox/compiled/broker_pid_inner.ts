// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// Compiled with --kernel-sandbox=broker: this process is the confined
// worker, a child of the PID that was started (its supervisor).
const [what] = Deno.args;
if (what === "wait") {
  // listening before the launcher is told to send it
  Deno.addSignalListener("SIGTERM", () => {
    console.log("the program got SIGTERM");
    Deno.exit(7);
  });
  console.log(Deno.ppid);
  // until the signal comes
  setInterval(() => {}, 1000);
} else if (what === "exit") {
  console.log(Deno.ppid);
  Deno.exit(9);
} else {
  console.log(Deno.ppid);
  Deno.kill(Deno.pid, "SIGKILL");
}
