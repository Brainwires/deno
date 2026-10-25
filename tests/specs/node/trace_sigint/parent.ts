// Runs child.ts with the SIGINT trace turned on through `how` (the
// `util.setTraceSigInt` API, `NODE_OPTIONS=--trace-sigint`, or not at all),
// sends it SIGINT once it is `what` (busy in a loop, idle, or listening for
// SIGINT itself), and reports how it ended and what it printed.
const [how, what] = Deno.args;
const env: Record<string, string> = { CHILD_MODE: what };
if (how === "api") env.CHILD_TRACE_API = "1";
if (how === "node_options") env.NODE_OPTIONS = "--trace-sigint";
const child = new Deno.Command(Deno.execPath(), {
  args: ["run", "-A", new URL("./child.ts", import.meta.url).pathname],
  env,
  stdout: "piped",
  stderr: "piped",
}).spawn();

const decoder = new TextDecoder();
let stdout = "";
const reader = child.stdout.getReader();
while (!stdout.includes("ready")) {
  const { value, done } = await reader.read();
  if (done) break;
  stdout += decoder.decode(value);
}
child.kill("SIGINT");
while (true) {
  const { value, done } = await reader.read();
  if (done) break;
  stdout += decoder.decode(value);
}
const status = await child.status;
const stderr = decoder.decode(
  await new Response(child.stderr).arrayBuffer(),
);
console.log("signal:", status.signal);
console.log(
  "banner:",
  stderr.includes(
    "KEYBOARD_INTERRUPT: Script execution was interrupted by `SIGINT`",
  ),
);
console.log("stack:", /at innerWork \(.*child\.ts:\d+:\d+\)/.test(stderr));
if (stdout.includes("listener ran")) console.log("listener ran");
