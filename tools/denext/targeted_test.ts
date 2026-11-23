#!/usr/bin/env -S deno run --allow-run=git,gh
// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console

// denext: dispatch a targeted `denext tests` run (DENEXT.md item 496): only
// the named tests, filtered, on the named OS legs, on a throwaway pretest/*
// branch at the commit. The release gate never counts a targeted run, but
// keeping it off the candidate's own branch keeps that branch's run list the
// release's. See tools/README.md ("denext/targeted_test.ts") and
// .github/denext-runtime/targeted_tests.py for the selector syntax.
//
// No imports: it runs on whatever Deno a host has.

const USAGE = `\
Usage: deno run --allow-run=git,gh tools/denext/targeted_test.ts [options] <item>...

Pushes <commit> to a throwaway branch pretest/targeted-<sha12>-<time> and
dispatches denext_tests.yml there with only=<items> and os=<legs>. Each item is
<suite>[:<filter>]:

  specs:kernel_sandbox::policy::config     unit:pty_test
  unit_node:dgram_test                     integration:<filter>
  node_compat:<file>                       libs:<crate>[:<filter>]

Options:
  --commit <ref>    the commit to test (default HEAD); it must be pushed
                    somewhere the remote has, or be pushable from here
  --os <legs>       comma-separated (default linux): linux, linux-arm, macos,
                    macos-x64, windows, windows-arm
  --remote <name>   the git remote of Brainwires/deno (default origin)
  --repo <o/n>      the GitHub repository (default Brainwires/deno)
  --branch <name>   the branch to push (default pretest/targeted-<sha12>-<time>);
                    must start with pretest/
  --dry-run         print the commands instead of running them
  -h, --help        this text

Delete the branch when done: git push <remote> --delete <branch>`;

const LEGS = [
  "linux",
  "linux-arm",
  "macos",
  "macos-x64",
  "windows",
  "windows-arm",
];
const SUITES = ["unit", "unit_node", "specs", "integration", "node_compat"];
const FILTER = /^[A-Za-z0-9_.:/@+=][A-Za-z0-9_.:/@+=-]*$/;

export interface Options {
  commit: string;
  os: string;
  remote: string;
  repo: string;
  branch: string;
  dryRun: boolean;
  items: string[];
}

/** The options, or an error message. Mirrors targeted_tests.py's checks so
 * a typo fails here, before a push (the workflow checks again). */
export function parseArgs(args: string[]): Options | string {
  const o: Options = {
    commit: "HEAD",
    os: "linux",
    remote: "origin",
    repo: "Brainwires/deno",
    branch: "",
    dryRun: false,
    items: [],
  };
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    const value = () => {
      const v = args[++i];
      if (v === undefined) throw new Error(`${a} needs a value`);
      return v;
    };
    try {
      if (a === "--commit") o.commit = value();
      else if (a === "--os") o.os = value();
      else if (a === "--remote") o.remote = value();
      else if (a === "--repo") o.repo = value();
      else if (a === "--branch") o.branch = value();
      else if (a === "--dry-run") o.dryRun = true;
      else if (a.startsWith("-")) return `unknown option ${a}`;
      else o.items.push(...a.split(/\s+/).filter(Boolean));
    } catch (e) {
      return (e as Error).message;
    }
  }
  if (o.items.length === 0) return "name at least one test item";
  for (const item of o.items) {
    const error = checkItem(item);
    if (error) return error;
  }
  const unknown = o.os.split(",").filter((n) => !LEGS.includes(n));
  if (unknown.length) {
    return `--os ${o.os}: unknown leg(s) ${unknown.join(", ")} (${
      LEGS.join(",")
    }, no spaces)`;
  }
  if (o.branch && !o.branch.startsWith("pretest/")) {
    return `--branch ${o.branch}: a targeted run goes on a pretest/* branch`;
  }
  return o;
}

/** An error message for an invalid `only` item, or "". */
export function checkItem(item: string): string {
  const [suite, ...rest] = item.split(":");
  if (suite === "libs") {
    if (!rest[0]) return `${item}: libs:<crate>[:<filter>]`;
    if (!/^[A-Za-z0-9_-]+$/.test(rest[0])) return `${item}: bad crate name`;
    const filter = rest.slice(1).join(":");
    if (filter && !FILTER.test(filter)) return `${item}: bad filter`;
    return "";
  }
  if (!SUITES.includes(suite)) {
    return `${item}: unknown suite ${suite} (${SUITES.join(", ")}, libs)`;
  }
  const filter = rest.join(":");
  if (rest.length && !filter) return `${item}: empty filter`;
  if (filter && !FILTER.test(filter)) return `${item}: bad filter`;
  return "";
}

/** The default branch name for a commit: pretest/targeted-<sha12>-<time>. */
export function defaultBranch(sha: string, now: Date): string {
  const time = now.toISOString().replace(/[-:]/g, "").replace(/\..*$/, "");
  return `pretest/targeted-${sha.slice(0, 12)}-${time.toLowerCase()}`;
}

/** The commands that push the branch and dispatch the run. */
export function commands(o: Options, sha: string): string[][] {
  return [
    ["git", "push", o.remote, `${sha}:refs/heads/${o.branch}`],
    [
      "gh",
      "workflow",
      "run",
      "denext_tests.yml",
      "-R",
      o.repo,
      "--ref",
      o.branch,
      "-f",
      `only=${o.items.join(" ")}`,
      "-f",
      `os=${o.os}`,
    ],
  ];
}

async function run(cmd: string[]): Promise<string> {
  const out = await new Deno.Command(cmd[0], {
    args: cmd.slice(1),
    stdout: "piped",
    stderr: "inherit",
  }).output();
  if (!out.success) {
    throw new Error(`${cmd.join(" ")} failed (exit ${out.code})`);
  }
  return new TextDecoder().decode(out.stdout).trim();
}

async function main(args: string[]): Promise<number> {
  if (args.includes("-h") || args.includes("--help")) {
    console.log(USAGE);
    return 0;
  }
  const o = parseArgs(args);
  if (typeof o === "string") {
    console.error(`error: ${o}\n\n${USAGE}`);
    return 2;
  }
  const sha = await run([
    "git",
    "rev-parse",
    "--verify",
    `${o.commit}^{commit}`,
  ]);
  if (!o.branch) o.branch = defaultBranch(sha, new Date());
  for (const cmd of commands(o, sha)) {
    console.log(`$ ${cmd.map((a) => /\s/.test(a) ? `'${a}'` : a).join(" ")}`);
    if (!o.dryRun) await run(cmd);
  }
  console.log(
    `\nTargeted run of ${sha.slice(0, 12)} dispatched on ${o.branch}` +
      ` (its title: "denext tests (targeted: ${
        o.items.join(" ")
      } on ${o.os})").` +
      `\nWhen done: git push ${o.remote} --delete ${o.branch}`,
  );
  return 0;
}

if (import.meta.main) {
  Deno.exit(await main(Deno.args));
}
