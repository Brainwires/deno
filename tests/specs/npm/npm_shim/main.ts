// Runs deno through a symlink named `npm` (the shim
// `deno x --install-node-shims` writes) and prints what each npm command did.
const bin = Deno.makeTempDirSync();
const npm = `${bin}/npm`;
Deno.symlinkSync(Deno.execPath(), npm);

async function run(args: string[], cwd = "project") {
  const out = await new Deno.Command(npm, {
    args,
    cwd,
    env: { NO_COLOR: "1" },
    stdout: "piped",
    stderr: "piped",
  }).output();
  const text = new TextDecoder().decode(out.stdout) +
    new TextDecoder().decode(out.stderr);
  // deno task echoes the task before running it
  const lines = text.split("\n").filter((l) => l && !l.startsWith("Task "));
  console.log(`$ npm ${args.join(" ")} [exit ${out.code}]`);
  for (const line of lines) console.log(`  ${line}`);
}

await run(["run", "hello", "--", "world"]);
await run(["test"]);
await run(["install", "@denotest/esm-basic"]);
const pkg = JSON.parse(Deno.readTextFileSync("project/package.json"));
console.log("dependency", pkg.dependencies["@denotest/esm-basic"]);
Deno.mkdirSync("fresh");
await run(["init", "-y"], "fresh");
console.log(
  "init",
  JSON.parse(Deno.readTextFileSync("fresh/package.json")).name,
);
await run(["publish"]);
