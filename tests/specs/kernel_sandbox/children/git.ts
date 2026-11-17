// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// P6: `git status` works under the kernel sandbox with the documented
// grants: --allow-run=git, read on the repository and on the git config
// (HOME/.gitconfig); git writes nothing it needs for status.
const out = new Deno.Command("git", {
  args: ["status", "--porcelain"],
  cwd: "repo",
  env: { HOME: Deno.realPathSync("home") },
}).outputSync();
Deno.stdout.writeSync(out.stdout);
Deno.stdout.writeSync(out.stderr);
console.log(`git exit: ${out.code}`);
