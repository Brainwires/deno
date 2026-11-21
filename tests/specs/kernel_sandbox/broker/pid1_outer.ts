// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// A brokered run as PID 1 of a PID namespace, as in a container: `unshare`
// forks, and the `deno` it executes is the namespace's init. After the
// supervisor's exec that PID 1 is the supervisor. An init gets only the
// signals it has handlers for, and nothing else reaps for it.
const child = new Deno.Command("unshare", {
  args: [
    "--user",
    "--map-root-user",
    "--pid",
    "--fork",
    "--mount-proc",
    Deno.execPath(),
    "run",
    "--quiet",
    "--no-prompt",
    "--kernel-sandbox=broker",
    "--allow-read=.",
    "pid1_inner.ts",
  ],
  stdout: "piped",
  stderr: "inherit",
}).spawn();
const reader = child.stdout.getReader();
const decoder = new TextDecoder();
let text = "";
while (!text.includes("ready\n")) {
  const { value, done } = await reader.read();
  if (done) break;
  text += decoder.decode(value);
}
// the namespace's init, by its PID out here: the one child of `unshare`
const children = (await Deno.readTextFile(
  `/proc/${child.pid}/task/${child.pid}/children`,
)).trim().split(/\s+/).filter((pid) => pid !== "");
console.log(`unshare has one child: ${children.length === 1}`);
Deno.kill(Number(children[0]), "SIGTERM");
for (;;) {
  const { value, done } = await reader.read();
  if (done) break;
  text += decoder.decode(value);
}
const status = await child.status;
console.log(text.replace("ready\n", "").trim());
console.log(`exit code: ${status.code}`);
