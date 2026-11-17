// Copyright 2018-2026 the Deno authors. MIT license.
// A repository with one untracked file, and a home folder with a git config.
Deno.mkdirSync("repo");
Deno.mkdirSync("home");
Deno.writeTextFileSync(
  "home/.gitconfig",
  "[user]\n\tname = t\n\temail = t@example.com\n",
);
const init = new Deno.Command("git", {
  args: ["init", "-q", "repo"],
  env: { HOME: `${Deno.cwd()}/home` },
}).outputSync();
if (!init.success) throw new Error(new TextDecoder().decode(init.stderr));
Deno.writeTextFileSync("repo/a.txt", "a\n");
