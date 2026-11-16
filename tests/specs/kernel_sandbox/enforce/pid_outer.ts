// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// P4: the re-exec keeps the PID, and signals reach the confined process.
const child = new Deno.Command(Deno.execPath(), {
  args: ["run", "--quiet", "--kernel-sandbox=enforce", "pid.ts"],
  stdout: "piped",
}).spawn();
const reader = child.stdout.pipeThrough(new TextDecoderStream()).getReader();
let text = "";
while (!text.includes("\n")) {
  const { value, done } = await reader.read();
  if (done) break;
  text += value;
}
const reported = Number(/pid (\d+)/.exec(text)?.[1]);
console.log(`pid unchanged: ${reported === child.pid}`);
child.kill("SIGTERM");
while (true) {
  const { value, done } = await reader.read();
  if (done) break;
  text += value;
}
const status = await child.status;
console.log(`signal delivered: ${text.includes("got SIGTERM")}`);
console.log(`exit code: ${status.code}`);
