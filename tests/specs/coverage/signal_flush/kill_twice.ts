// Starts `deno <mode> --coverage <script>` (a script stuck in synchronous
// JavaScript, which the signal's flush waits for in vain), waits for it to
// print "ready", sends it `signal`, then the same signal again a second
// later, and reports how it ended and whether it ended well before the
// flush's 10 s timeout.
const [signal, mode, script] = Deno.args as [Deno.Signal, string, string];
const covDir = `${Deno.cwd()}/cov_twice_${mode}_${signal}`;

const args = mode === "test"
  ? ["test", "-A", `--coverage=${covDir}`, script]
  : ["run", "-A", script];
const child = new Deno.Command(Deno.execPath(), {
  args,
  env: mode === "test" ? {} : { DENO_COVERAGE_DIR: covDir },
  stdout: "piped",
  stderr: "null",
}).spawn();

const decoder = new TextDecoder();
let stdout = "";
const reader = child.stdout.getReader();
while (!stdout.includes("ready")) {
  const { value, done } = await reader.read();
  if (done) break;
  stdout += decoder.decode(value);
}

const start = performance.now();
child.kill(signal);
await new Promise((resolve) => setTimeout(resolve, 1000));
try {
  child.kill(signal);
} catch {
  // It already ended.
}
// Drain the rest of the output so the child never blocks on a full pipe.
while (true) {
  const { value, done } = await reader.read();
  if (done) break;
  stdout += decoder.decode(value);
}
const status = await child.status;
const elapsed = performance.now() - start;
console.log("signal:", status.signal);
console.log("code:", status.code);
console.log("ended before the flush timeout:", elapsed < 5000);

// Whatever was written is whole: no truncated coverage file.
try {
  for await (const entry of Deno.readDir(covDir)) {
    if (!entry.name.endsWith(".json")) continue;
    JSON.parse(await Deno.readTextFile(`${covDir}/${entry.name}`));
  }
} catch (err) {
  if (!(err instanceof Deno.errors.NotFound)) throw err;
}
