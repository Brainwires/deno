// denext (DENEXT.md item 451): the kernel sandbox's policy-keyed cache
// overlay. Run unconfined (`--kernel-sandbox=off`); it starts the runs it
// checks itself, each with a deadline, so a run that blocks is reported
// rather than hanging the suite.
const join = (...parts: string[]) => parts.filter((p) => p).join("/");

const deno = Deno.execPath();
const denoDir = Deno.env.get("DENO_DIR")!;
const overlayRoot = join(denoDir, "sandboxed");

async function run(
  label: string,
  args: string[],
  env: Record<string, string> = {},
) {
  const child = new Deno.Command(deno, {
    args: ["run", "--quiet", ...args, "typed.ts"],
    env,
    stdout: "piped",
    stderr: "piped",
    stdin: "null",
  }).spawn();
  const timer = setTimeout(() => child.kill("SIGKILL"), 20_000);
  const out = await child.output();
  clearTimeout(timer);
  const text = new TextDecoder().decode(out.stdout).trim();
  if (out.signal === "SIGKILL") {
    console.log(`${label}: blocked`);
  } else {
    console.log(`${label}: ${out.code === 0 ? "ok" : "failed"} ${text}`);
    if (out.code !== 0) {
      console.log(new TextDecoder().decode(out.stderr));
    }
  }
}

function walk(dir: string): string[] {
  const out: string[] = [];
  let entries: Deno.DirEntry[];
  try {
    entries = [...Deno.readDirSync(dir)];
  } catch {
    return out;
  }
  for (const entry of entries) {
    const path = join(dir, entry.name);
    if (entry.isDirectory) out.push(...walk(path));
    else out.push(path);
  }
  return out;
}

// 1. a confined run writes its caches into its policy's overlay
await run("confined", ["--kernel-sandbox=enforce"]);
const overlays = [...Deno.readDirSync(overlayRoot)].map((e) => e.name);
console.log("overlays:", overlays.length, /^[0-9a-f]{16}$/.test(overlays[0]));
const overlay = join(overlayRoot, overlays[0]);
const files = walk(overlay);
const emit = files.find((f) => f.includes("/gen/") && f.includes("typed.ts"));
console.log("emit in the overlay:", emit !== undefined);
console.log(
  "code cache in the overlay:",
  files.some((f) => f.includes("v8_code_cache_v2_denext")),
);
console.log(
  "nothing confined wrote outside it:",
  !walk(join(denoDir, "gen")).some((f) => f.includes("typed.ts")),
);
console.log(
  "overlay is the user's alone:",
  (Deno.statSync(overlay).mode! & 0o077) === 0,
);

// 2. the same policy again: the overlay is read (and warm)
await run("confined again", ["--kernel-sandbox=enforce"]);

// 3. the overlay's emit becomes a FIFO: a run that opens it to read blocks
Deno.removeSync(emit!);
const mkfifo = new Deno.Command("mkfifo", { args: [emit!] }).outputSync();
console.log("fifo:", mkfifo.success);

// an unconfined run never reads the overlay: it finishes
await run("unconfined", ["--kernel-sandbox=off"]);
// another policy has another overlay: it finishes too
await run("other policy", ["--kernel-sandbox=enforce", "--allow-env=X"]);
// the positive control: a confined run of the first policy reads the overlay
// first, so it blocks on the FIFO
await run("confined, first policy", ["--kernel-sandbox=enforce"]);
console.log(
  "overlays now:",
  [...Deno.readDirSync(overlayRoot)].length,
);

// 4. a DENO_DIR where no overlay can be made (`sandboxed` is a file): the
// confined run still runs, its caches in memory, rather than refusing
const blocked = Deno.makeTempDirSync();
Deno.writeTextFileSync(join(blocked, "sandboxed"), "");
await run("no overlay possible", ["--kernel-sandbox=enforce"], {
  DENO_DIR: blocked,
});
