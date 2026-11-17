// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// Inside the sandbox, a nested deno refuses the subcommands that write the
// project, the lockfile or the caches (DENEXT.md item 450), before they
// start, as a jailed lifecycle script does; `deno run` still runs.
const deno = Deno.execPath();
for (
  const args of [
    ["add", "jsr:@std/path"],
    ["install"],
    ["cache", "inside/nested_read.ts"],
    ["fmt", "inside"],
  ]
) {
  const out = new Deno.Command(deno, { args, stderr: "piped" }).outputSync();
  const refused = new TextDecoder().decode(out.stderr).includes(
    "not allowed inside a kernel sandbox",
  );
  console.log(`${args[0]}: code ${out.code}, refused: ${refused}`);
}
const run = new Deno.Command(deno, {
  args: ["run", "--quiet", "-A", "inside/nested_read.ts", "run, "],
}).outputSync();
Deno.stdout.writeSync(run.stdout);
