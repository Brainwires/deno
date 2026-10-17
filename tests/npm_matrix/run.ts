#!/usr/bin/env -S deno run --allow-all --config=tests/config/deno.json
// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console

// The npm smoke matrix: real npm packages, each in its own package.json
// project with a real node_modules, run with the deno under test and no
// `node` on PATH. Each case is `cases/<name>/` (package.json, the program,
// and case.json: the command to run and the expected status).
//
// A case passes when its command exits 0 and prints SMOKE_OK. One that fails
// is retried after `deno install --allow-scripts` and then reports
// "pass-with-allow-scripts" if that run passes. The run fails (exit 1) when a
// case does worse than its `expect`; a case that does better is reported as
// an improvement, so its `expect` can be raised.
//
// Usage:
//   deno run -A --config=tests/config/deno.json tests/npm_matrix/run.ts \
//     --deno target/debug/deno \
//     [--work-dir DIR] [--jobs 3] [--only a,b] [--skip a,b] [--json out.jsonl]
//     [--install-args "--node-modules-linker=isolated"]
//
// Cases that need a database or object store list it in `services`; start
// them with `tests/npm_matrix/services.sh up` first (or pass --skip).

import { parseArgs } from "@std/cli/parse-args";
import { copy } from "@std/fs/copy";
import { join, resolve } from "@std/path";

type Status = "pass" | "pass-with-allow-scripts" | "fail";

interface Case {
  category: string;
  run: string;
  node: string;
  expect: Status;
  gap?: number;
  services?: string[];
}

interface StepResult {
  code: number | "timeout";
  ok: boolean;
  secs: number;
  tail: string;
}

interface CaseResult {
  name: string;
  category: string;
  expect: Status;
  status: Status;
  install: StepResult;
  run: StepResult;
  installScripts?: StepResult;
  run2?: StepResult;
}

const RANK: Record<Status, number> = {
  "fail": 0,
  "pass-with-allow-scripts": 1,
  "pass": 2,
};

const SERVICE_PORTS: Record<string, number> = {
  postgres: 55432,
  redis: 56379,
  mongo: 57017,
  minio: 59000,
};

const args = parseArgs(Deno.args, {
  string: ["deno", "work-dir", "jobs", "only", "skip", "json", "install-args"],
  default: { jobs: "3" },
});
if (!args.deno) {
  console.error("--deno <path to the deno under test> is required");
  Deno.exit(2);
}
const denoPath = resolve(args.deno);
const casesDir = new URL("./cases/", import.meta.url).pathname;
const workDir = resolve(
  args["work-dir"] ?? await Deno.makeTempDir({ prefix: "npm_matrix_" }),
);
const only = args.only ? new Set(args.only.split(",")) : undefined;
const skip = new Set(args.skip ? args.skip.split(",") : []);
// extra arguments for every `deno install` (e.g. a linker to compare)
const installArgs = args["install-args"] ? " " + args["install-args"] : "";

// PATH holds only `deno` plus the system directories: no `node`, `npm` or
// `npx`, so the run is the one a user without Node.js gets.
const binDir = join(workDir, "_bin");
await Deno.mkdir(binDir, { recursive: true });
try {
  await Deno.symlink(denoPath, join(binDir, "deno"));
} catch (err) {
  if (!(err instanceof Deno.errors.AlreadyExists)) throw err;
}
const systemPath = ["/usr/bin", "/bin"];
for (const dir of systemPath) {
  for (const tool of ["node", "npm", "npx"]) {
    try {
      await Deno.stat(join(dir, tool));
      console.error(`warning: ${dir}/${tool} exists; cases may use it`);
    } catch {
      // not there: good
    }
  }
}
const env: Record<string, string> = {
  PATH: [binDir, ...systemPath].join(":"),
  HOME: Deno.env.get("HOME") ?? workDir,
  NO_COLOR: "1",
  CI: Deno.env.get("CI") ?? "",
};
for (const name of ["DENO_DIR", "TMPDIR", "LANG", "XDG_CACHE_HOME"]) {
  const value = Deno.env.get(name);
  if (value !== undefined) env[name] = value;
}

async function sh(
  cmd: string,
  cwd: string,
  timeoutSecs: number,
): Promise<StepResult> {
  const start = performance.now();
  const child = new Deno.Command("bash", {
    args: ["-c", cmd],
    cwd,
    env: { ...env, PUPPETEER_CACHE_DIR: join(cwd, ".pcache") },
    clearEnv: true,
    stdin: "null",
    stdout: "piped",
    stderr: "piped",
    // its own process group, so a timeout takes down every child too
    detached: true,
  }).spawn();
  let timedOut = false;
  const timer = setTimeout(() => {
    timedOut = true;
    try {
      Deno.kill(-child.pid, "SIGKILL");
    } catch {
      // already gone
    }
  }, timeoutSecs * 1000);
  const out = await child.output();
  clearTimeout(timer);
  try {
    Deno.kill(-child.pid, "SIGKILL");
  } catch {
    // the group is already gone
  }
  const text = new TextDecoder().decode(out.stdout) +
    new TextDecoder().decode(out.stderr);
  const code = timedOut ? "timeout" : out.code;
  return {
    code,
    ok: code === 0 && text.includes("SMOKE_OK"),
    secs: Math.round((performance.now() - start) / 100) / 10,
    tail: text.slice(-2500),
  };
}

async function waitForPort(port: number): Promise<boolean> {
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    try {
      const conn = await Deno.connect({ hostname: "127.0.0.1", port });
      conn.close();
      return true;
    } catch {
      await new Promise((r) => setTimeout(r, 500));
    }
  }
  return false;
}

async function runCase(name: string, c: Case): Promise<CaseResult> {
  const dir = join(workDir, name);
  await Deno.remove(dir, { recursive: true }).catch(() => {});
  await copy(join(casesDir, name), dir);
  await Deno.remove(join(dir, "case.json"));
  const result: CaseResult = {
    name,
    category: c.category,
    expect: c.expect,
    status: "fail",
    install: await sh("deno install" + installArgs, dir, 900),
    run: { code: -1, ok: false, secs: 0, tail: "" },
  };
  result.run = await sh(c.run, dir, 600);
  if (result.run.ok) {
    result.status = "pass";
  } else {
    result.installScripts = await sh(
      "deno install --allow-scripts" + installArgs,
      dir,
      900,
    );
    result.run2 = await sh(c.run, dir, 600);
    if (result.run2.ok) result.status = "pass-with-allow-scripts";
  }
  return result;
}

const names: string[] = [];
for await (const entry of Deno.readDir(casesDir)) {
  if (!entry.isDirectory) continue;
  if (only && !only.has(entry.name)) continue;
  if (skip.has(entry.name)) continue;
  names.push(entry.name);
}
names.sort();
const cases = new Map<string, Case>();
for (const name of names) {
  cases.set(
    name,
    JSON.parse(await Deno.readTextFile(join(casesDir, name, "case.json"))),
  );
}

const needed = new Set([...cases.values()].flatMap((c) => c.services ?? []));
for (const service of needed) {
  if (!await waitForPort(SERVICE_PORTS[service])) {
    console.error(
      `service ${service} is not listening on ${SERVICE_PORTS[service]}; ` +
        "run tests/npm_matrix/services.sh up, or --skip its cases",
    );
    Deno.exit(2);
  }
}

console.log(`deno: ${denoPath}`);
console.log(`work dir: ${workDir}`);
console.log(`${names.length} cases, ${args.jobs} at a time`);
const results: CaseResult[] = [];
const queue = [...names];
const jsonOut = args.json
  ? await Deno.open(args.json, {
    create: true,
    append: true,
  })
  : undefined;
await Promise.all(
  Array.from({ length: Number(args.jobs) }, async () => {
    for (let name = queue.shift(); name; name = queue.shift()) {
      const r = await runCase(name, cases.get(name)!);
      results.push(r);
      console.log(`${r.status.padEnd(24)} ${name} (expect ${r.expect})`);
      if (jsonOut) {
        await jsonOut.write(
          new TextEncoder().encode(JSON.stringify(r) + "\n"),
        );
      }
    }
  }),
);
jsonOut?.close();

results.sort((a, b) => a.name.localeCompare(b.name));
const regressions = results.filter((r) => RANK[r.status] < RANK[r.expect]);
const improvements = results.filter((r) => RANK[r.status] > RANK[r.expect]);
const count = (s: Status) => results.filter((r) => r.status === s).length;

const lines = [
  `## npm matrix: ${count("pass")} pass, ${
    count("pass-with-allow-scripts")
  } pass with --allow-scripts, ${count("fail")} fail (of ${results.length})`,
  "",
  "| case | category | status | expected |",
  "| --- | --- | --- | --- |",
  ...results.map((r) =>
    `| ${r.name} | ${r.category} | ${
      RANK[r.status] < RANK[r.expect]
        ? `**${r.status}** (regression)`
        : RANK[r.status] > RANK[r.expect]
        ? `${r.status} (improved)`
        : r.status
    } | ${r.expect} |`
  ),
];
const summary = lines.join("\n") + "\n";
console.log("\n" + summary);
const stepSummary = Deno.env.get("GITHUB_STEP_SUMMARY");
if (stepSummary) {
  await Deno.writeTextFile(stepSummary, summary, { append: true });
}

for (const r of regressions) {
  const step = r.run2 ?? r.run;
  console.log(`\n---- ${r.name}: ${r.status}, expected ${r.expect}`);
  console.log(`install (${r.install.code}):\n${r.install.tail.slice(-800)}`);
  console.log(`run (${step.code}):\n${step.tail}`);
}
if (improvements.length) {
  console.log(
    `\nimproved (raise "expect" in case.json): ${
      improvements.map((r) => r.name).join(", ")
    }`,
  );
}
Deno.exit(regressions.length ? 1 : 0);
