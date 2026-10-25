// Starts `deno <mode> --coverage <script>`, waits for it to print "ready",
// sends it `signal`, then reports how it ended and which modules have
// coverage written.
const [signal, mode, script] = Deno.args as [Deno.Signal, string, string];
const covDir = `${Deno.cwd()}/cov_${mode}_${signal}_${
  script.replace(".", "_")
}`;

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

child.kill(signal);
// Drain the rest of the output so the child never blocks on a full pipe.
while (true) {
  const { value, done } = await reader.read();
  if (done) break;
  stdout += decoder.decode(value);
}
const status = await child.status;
console.log("signal:", status.signal);
console.log("code:", status.code);
if (stdout.includes("listener ran")) console.log("listener ran");

const covered = new Set<string>();
try {
  for await (const entry of Deno.readDir(covDir)) {
    if (!entry.name.endsWith(".json")) continue;
    const profile = JSON.parse(
      await Deno.readTextFile(`${covDir}/${entry.name}`),
    );
    const name = profile.url.split("/").pop();
    if (name === script) covered.add(name);
  }
} catch (err) {
  if (!(err instanceof Deno.errors.NotFound)) throw err;
}
console.log(
  "coverage of",
  script + ":",
  covered.size === 1 ? "written" : "missing",
);
