// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// denext (DENEXT.md item 448): compares one suite's failures under the forced
// kernel sandbox (DENO_INTERNAL_KERNEL_SANDBOX_FORCE) with the reviewed list
// in expected_failures.jsonc. A failure that isn't listed for this OS fails
// the bed; a listed entry that passed is reported as stale (fix the list).
//
//   deno run -A tests/kernel_sandbox_beds/compare.ts --suite node_compat \
//     --cargo-log node_compat.log [--force enforce|project] [--os linux]
//     [--json out.json]
//   ... --suite npm_matrix --npm-jsonl npm-matrix.jsonl
//   ... --suite denext_integration --deno-test-log integration.log
//
// Inputs: a `cargo test` log of a file_test_runner suite (specs,
// node_compat), the npm matrix's --json results, or a `deno test` log.

import { parse as parseJsonc } from "jsr:@std/jsonc@1";
import { parseArgs } from "jsr:@std/cli@1/parse-args";
import { fromFileUrl } from "jsr:@std/path@1/from-file-url";

interface Entry {
  os?: string[];
  reason: string;
}
type Expected = Record<string, Record<string, Record<string, Entry>>>;

const args = parseArgs(Deno.args, {
  string: [
    "suite",
    "force",
    "cargo-log",
    "npm-jsonl",
    "deno-test-log",
    "os",
    "json",
  ],
});
const suite = args.suite;
if (!suite) throw new Error("--suite is required");
const os = args.os ?? Deno.build.os;
// the forced mode the suite ran under (each has its own list)
const force = args.force ?? "enforce";

// deno-lint-ignore no-control-regex
const ansi = /\x1b\[[0-9;]*m/g;

/** file_test_runner's `test <name> ... fail` lines. */
export function cargoFailures(text: string): Set<string> {
  const out = new Set<string>();
  for (const raw of text.split("\n")) {
    const line = raw.replace(ansi, "").trim();
    const m = /^test (\S+) \.\.\. (fail|FAILED)\b/.exec(line);
    if (m) out.add(m[1]);
  }
  return out;
}

/** The npm matrix's cases that did worse than their case.json expects. */
export function npmFailures(text: string): Set<string> {
  const out = new Set<string>();
  for (const line of text.split("\n")) {
    if (!line.trim()) continue;
    const r = JSON.parse(line);
    const rank: Record<string, number> = {
      "fail": 0,
      "pass-with-allow-scripts": 1,
      "pass": 2,
    };
    if (rank[r.status] < rank[r.expect]) out.add(r.name);
  }
  return out;
}

/** `deno test`'s failure summary: `<name> => <file>:<line>:<col>`. */
export function denoTestFailures(text: string): Set<string> {
  const out = new Set<string>();
  let inFailures = false;
  for (const raw of text.split("\n")) {
    const line = raw.replace(ansi, "").trim();
    if (line === "FAILURES") {
      inFailures = true;
      continue;
    }
    if (inFailures) {
      const m = /^(.+?) => (\S+?):\d+:\d+$/.exec(line);
      if (m) out.add(`${m[2].replace(/^\.\//, "")} > ${m[1]}`);
      else if (/^(ok|FAILED) \|/.test(line)) inFailures = false;
    }
  }
  return out;
}

function matches(pattern: string, name: string): boolean {
  return pattern.endsWith("*")
    ? name.startsWith(pattern.slice(0, -1))
    : pattern === name;
}

if (import.meta.main) {
  let failures: Set<string>;
  if (args["cargo-log"]) {
    failures = cargoFailures(Deno.readTextFileSync(args["cargo-log"]));
  } else if (args["npm-jsonl"]) {
    failures = npmFailures(Deno.readTextFileSync(args["npm-jsonl"]));
  } else if (args["deno-test-log"]) {
    failures = denoTestFailures(Deno.readTextFileSync(args["deno-test-log"]));
  } else {
    throw new Error("one of --cargo-log, --npm-jsonl, --deno-test-log");
  }
  const expectedPath = fromFileUrl(
    new URL("./expected_failures.jsonc", import.meta.url),
  );
  const all = parseJsonc(
    Deno.readTextFileSync(expectedPath),
  ) as unknown as Expected;
  const listed = Object.entries(all[force]?.[suite] ?? {}).filter(([, e]) =>
    !e.os || e.os.includes(os)
  );
  const unexpected = [...failures].filter((f) =>
    !listed.some(([p]) => matches(p, f))
  ).sort();
  const expected = [...failures].filter((f) =>
    listed.some(([p]) => matches(p, f))
  ).sort();
  const stale = listed.filter(([p]) => !p.endsWith("*") && !failures.has(p))
    .map(([p]) => p).sort();
  console.log(
    `${suite} (${os}, ${force}): ${failures.size} failed: ${expected.length} expected, ${unexpected.length} unexpected; ${stale.length} listed entries passed`,
  );
  for (const f of expected) {
    const reason = listed.find(([p]) => matches(p, f))![1].reason;
    console.log(`  expected  ${f}: ${reason}`);
  }
  for (const f of unexpected) console.log(`  UNEXPECTED ${f}`);
  for (const f of stale) console.log(`  stale     ${f} (passed; remove it)`);
  if (args.json) {
    Deno.writeTextFileSync(
      args.json,
      JSON.stringify(
        { suite, os, force, expected, unexpected, stale },
        null,
        2,
      ),
    );
  }
  if (unexpected.length > 0) Deno.exit(1);
}
