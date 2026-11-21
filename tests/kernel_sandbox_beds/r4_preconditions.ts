// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// denext (DENEXT.md item 472): what must hold before `--kernel-sandbox=auto`
// moves to stage R4 (enforce wherever the permission set is fixed, a bare run
// with no terminal included). One check per precondition; each is `pass`,
// `fail`, `manual` (a person records the evidence in r4_evidence.json, which
// turns it into `pass`) or `pending` (evidence that can only come after the
// flip ships, which gates 5.0.0 final and not the flip: precondition 8's
// release candidates).
//
//   deno run -A tests/kernel_sandbox_beds/r4_preconditions.ts \
//     [--candidate <sha>] [--deno target/release/deno] [--denext ../denext] \
//     [--repo Brainwires/deno] [--no-soak] [--json]
//
// Exits 0 when every precondition passes or is pending. `--deno` measures
// the startup cost with that binary, Mode A and Mode B (without it the
// checks are `manual`); `--denext` is a checkout of Brainwires/denext, for
// the documentation checks; `--no-soak` skips the GitHub requests (the soak
// checks then read `manual`). The runs are read with `gh api`, or with no
// token from GitHub's public API where `gh` is missing or not signed in.

import { parseArgs } from "jsr:@std/cli@1/parse-args";
import { parse as parseJsonc } from "jsr:@std/jsonc@1";
import { fromFileUrl } from "jsr:@std/path@1/from-file-url";
import {
  ancestry,
  type BedRun,
  dispatchCommand,
  isFullRun,
  listBedRuns,
  runKind,
  soak,
  SOAK_RUNS,
} from "./soak.ts";

export type Status = "pass" | "fail" | "manual" | "pending";

export interface Check {
  id: string;
  title: string;
  status: Status;
  detail: string;
}

/** The most `deno run` may cost more under enforce (Mode A) than off. */
export const STARTUP_BUDGET_MS = 30;
/**
 * The most `deno run` may cost more with the supervising broker (Mode B: a
 * supervisor and two execs) than off.
 */
export const BROKER_STARTUP_BUDGET_MS = 60;

const root = fromFileUrl(new URL("../../", import.meta.url));
const read = (path: string): string | null => {
  try {
    return Deno.readTextFileSync(root + path);
  } catch {
    return null;
  }
};

async function output(
  cmd: string,
  args: string[],
  cwd = root,
): Promise<{ ok: boolean; text: string }> {
  try {
    const out = await new Deno.Command(cmd, { args, cwd, stderr: "null" })
      .output();
    return { ok: out.success, text: new TextDecoder().decode(out.stdout) };
  } catch {
    return { ok: false, text: "" };
  }
}

/**
 * The entries of the expected-failures lists that are not settled: a reason
 * that starts with "to triage" or "to fix" is a failure nobody decided on.
 */
export function unsettledEntries(
  // deno-lint-ignore no-explicit-any
  lists: any,
  beds: string[],
): string[] {
  const out: string[] = [];
  for (const bed of beds) {
    for (const [suite, entries] of Object.entries(lists?.[bed] ?? {})) {
      for (const [name, entry] of Object.entries(entries as object)) {
        const reason = String((entry as { reason?: string }).reason ?? "");
        if (/^to (triage|fix)\b/.test(reason) || reason.trim() === "") {
          out.push(`${bed} > ${suite} > ${name}`);
        }
      }
    }
  }
  return out;
}

/** The stage a source of cli/lib/kernel_sandbox.rs ships at. */
export function shippedStage(source: string): string | null {
  const m = /pub const AUTO_STAGE: AutoStage = AutoStage::(\w+);/.exec(source);
  return m ? m[1].toLowerCase() : null;
}

/** The median of `values`. */
export function median(values: number[]): number {
  const sorted = [...values].sort((a, b) => a - b);
  const mid = sorted.length >> 1;
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

async function timeRuns(deno: string, args: string[], dir: string) {
  const times: number[] = [];
  for (let i = 0; i < 21; i++) {
    const start = performance.now();
    const out = await new Deno.Command(deno, {
      args,
      cwd: dir,
      stdin: "null",
      stdout: "null",
      stderr: "null",
    }).output();
    if (!out.success) return null;
    // the first run warms the caches
    if (i > 0) times.push(performance.now() - start);
  }
  return median(times);
}

/**
 * One entry of r4_evidence.json (r4_evidence.schema.json): where the
 * evidence is, as a string, or with when it was recorded and by whom.
 */
export type Evidence = string | {
  evidence: string;
  /** ISO 8601 date. */
  recorded: string;
  by?: string;
};

/** The text of an entry, or null when it says nothing. */
export function evidenceText(entry: unknown): string | null {
  if (typeof entry === "string") return entry.trim() ? entry.trim() : null;
  if (entry && typeof entry === "object") {
    const e = entry as Record<string, unknown>;
    if (typeof e.evidence !== "string" || !e.evidence.trim()) return null;
    const by = typeof e.by === "string" && e.by ? `, ${e.by}` : "";
    return `${e.evidence.trim()} (${e.recorded}${by})`;
  }
  return null;
}

/**
 * Applies the recorded evidence: a `manual` or `pending` check with an
 * entry passes. A failed check stays failed whatever is recorded.
 */
export function withEvidence(
  checks: Check[],
  evidence: Record<string, unknown>,
): Check[] {
  return checks.map((c) => {
    const text = evidenceText(evidence[c.id]);
    return (c.status === "manual" || c.status === "pending") && text
      ? { ...c, status: "pass", detail: `recorded: ${text}` }
      : c;
  });
}

/**
 * What is wrong with r4_evidence.json against its schema and the checks
 * this script makes (`ids`): an entry for no check, an empty one, an object
 * without `evidence` or a `recorded` date.
 */
export function evidenceProblems(
  evidence: Record<string, unknown>,
  ids: string[],
): string[] {
  const out: string[] = [];
  for (const [key, entry] of Object.entries(evidence)) {
    if (key === "$schema" || key === "//") continue;
    if (!ids.includes(key)) {
      out.push(`"${key}" is no precondition's id`);
    } else if (typeof entry === "string") {
      if (!entry.trim()) out.push(`"${key}" is empty`);
    } else if (entry && typeof entry === "object" && !Array.isArray(entry)) {
      const e = entry as Record<string, unknown>;
      if (typeof e.evidence !== "string" || !e.evidence.trim()) {
        out.push(`"${key}" has no evidence`);
      }
      if (
        typeof e.recorded !== "string" ||
        !/^\d{4}-\d{2}-\d{2}/.test(e.recorded) ||
        Number.isNaN(Date.parse(e.recorded))
      ) {
        out.push(`"${key}" has no ISO 8601 recorded date`);
      }
      const extra = Object.keys(e).filter((k) =>
        !["evidence", "recorded", "by"].includes(k)
      );
      if (extra.length) out.push(`"${key}" has unknown keys: ${extra}`);
    } else {
      out.push(`"${key}" is neither a string nor an entry object`);
    }
  }
  return out;
}

/**
 * Whether the newest completed full run of `bed` on the candidate itself
 * is green (the forced beds: green once on the candidate, not soaked).
 */
export function greenOnCandidate(
  runs: BedRun[],
  bed: string,
  candidate: string,
): BedRun | null {
  const newest = runs
    .filter((r) => {
      const kind = runKind(r.title);
      return kind !== null && isFullRun(kind, bed) && r.sha === candidate &&
        r.status === "completed";
    })
    .sort((a, b) => Date.parse(b.startedAt) - Date.parse(a.startedAt))[0];
  return newest?.conclusion === "success" ? newest : null;
}

/** The titles of the checks the beds' runs decide, by id. */
export const BED_CHECKS: Record<string, string> = {
  "2b-stage-r4": "soak: stage-r4",
  "2c-enforce": "forced bed green on the candidate: enforce",
  "2c-project": "forced bed green on the candidate: project",
  "8": "both stage beds soaked; two 5.0 RCs on this runtime unreverted",
};

/**
 * The checks the beds' runs decide: `2b-stage-r4` (the soak: SOAK_RUNS green
 * full runs, four hours apart, on the candidate or its ancestors, the
 * newest on the candidate, a failure resets), `2c-enforce` / `2c-project`
 * (the forced beds, green once on the candidate) and `8`, O1's replacement
 * for "R3 shipped in the two releases before": both stage beds soaked
 * (stage-r3 needs its SOAK_RUNS counted runs on the candidate's ancestors;
 * once R4 ships, the raise-only variable runs an r3 bed at R4, so its newest
 * run needn't be on the candidate), then two 5.0 release candidates on this
 * runtime without a stage revert, which can only come after the flip ships:
 * `pending` (it gates 5.0.0 final), never `fail`.
 */
export function bedChecks(
  runs: BedRun[],
  candidate: string,
  isAncestor: (sha: string) => boolean,
  ref: string,
): Check[] {
  const checks: Check[] = [];
  const r4 = soak(runs, "stage-r4", candidate, isAncestor);
  checks.push({
    id: "2b-stage-r4",
    title: BED_CHECKS["2b-stage-r4"],
    status: r4.complete ? "pass" : "fail",
    detail: r4.complete
      ? `${r4.counted.length} green full runs in a row, ${
        r4.counted.map((r) => r.url ?? r.id).join(", ")
      }`
      : [...r4.notes, `next: ${dispatchCommand("stage-r4", ref)}`].join("; "),
  });
  for (const bed of ["enforce", "project"]) {
    const green = greenOnCandidate(runs, bed, candidate);
    checks.push({
      id: `2c-${bed}`,
      title: BED_CHECKS[`2c-${bed}`],
      status: green ? "pass" : "fail",
      detail: green
        ? `run ${green.url ?? green.id}`
        : `the newest full ${bed} run on ${
          candidate.slice(0, 10)
        } isn't green (or there is none): ${dispatchCommand(bed, ref)}`,
    });
  }
  const r3 = soak(runs, "stage-r3", candidate, isAncestor);
  const r3Soaked = r3.missing === 0;
  const missing = [
    r3Soaked ? null : `stage-r3: ${r3.counted.length} of ${SOAK_RUNS} counted`,
    r4.complete ? null : `stage-r4: not complete (2b-stage-r4)`,
  ].filter(Boolean);
  checks.push({
    id: "8",
    title: BED_CHECKS["8"],
    status: missing.length ? "fail" : "pending",
    detail: missing.length
      ? `${missing.join("; ")}${
        r3Soaked ? "" : `; next: ${dispatchCommand("stage-r3", ref)}`
      }`
      : 'both stage beds soaked; two 5.0 release candidates shipped on this runtime without a stage revert are still to come: record them as "8" in r4_evidence.json (gates 5.0.0 final, not the flip)',
  });
  return checks;
}

if (import.meta.main) {
  const args = parseArgs(Deno.args, {
    string: ["candidate", "deno", "denext", "repo", "ref"],
    boolean: ["json", "soak"],
    negatable: ["soak"],
    default: { soak: true },
  });
  const candidate = args.candidate ??
    (await output("git", ["rev-parse", "HEAD"])).text.trim();
  const checks: Check[] = [];
  const add = (id: string, title: string, status: Status, detail: string) =>
    checks.push({ id, title, status, detail });
  const denextDoc = (page: string): string | null => {
    if (!args.denext) return null;
    try {
      return Deno.readTextFileSync(
        `${args.denext}/site/app/docs/${page}/content.md`,
      );
    } catch {
      return null;
    }
  };

  // 1. the guarantee matrix is published, and the escape corpus matches it
  {
    const corpus = read(
      "tests/specs/kernel_sandbox/escape_corpus/__test__.jsonc",
    );
    const matrix = denextDoc("kernel-sandbox-guarantees");
    if (!corpus) {
      add(
        "1",
        "guarantee matrix and escape corpus",
        "fail",
        "the escape corpus spec is missing",
      );
    } else if (args.denext && !matrix) {
      add(
        "1",
        "guarantee matrix and escape corpus",
        "fail",
        "the docs have no guarantee matrix page",
      );
    } else {
      add(
        "1",
        "guarantee matrix and escape corpus",
        "manual",
        "record the run of kernel_sandbox::escape_corpus on every matrix kernel (5.15, 6.1, 6.8, 6.12, the newest; x86_64 and aarch64) and every macOS version of the runners, each matching the published matrix",
      );
    }
  }

  // 2. the lists settled; the stage-r4 bed soaked; the forced beds green on
  // the candidate (precondition 8, below, is decided from the same runs)
  {
    const lists = parseJsonc(
      read("tests/kernel_sandbox_beds/expected_failures.jsonc") ?? "{}",
    );
    const beds = ["enforce", "project", "stage-r4"];
    const unsettled = unsettledEntries(lists, beds);
    add(
      "2a",
      "expected failures settled",
      unsettled.length ? "fail" : "pass",
      unsettled.length
        ? `${unsettled.length} entr${
          unsettled.length === 1 ? "y is" : "ies are"
        } still "to triage" / "to fix" (each must be fixed, documented, or an implicit-set entry with a test): ${
          unsettled.slice(0, 5).join("; ")
        }${unsettled.length > 5 ? "; …" : ""}`
        : "every listed failure has a settled reason",
    );
  }
  if (!args.soak) {
    for (const [id, title] of Object.entries(BED_CHECKS)) {
      add(id, title, "manual", "run without --no-soak (reads the beds' runs)");
    }
  } else {
    try {
      const runs = await listBedRuns(args.repo ?? "Brainwires/deno");
      checks.push(
        ...bedChecks(
          runs,
          candidate,
          await ancestry(runs, candidate, root),
          args.ref ?? "train/denext30",
        ),
      );
    } catch (err) {
      const why = `the beds' runs couldn't be read: ${
        err instanceof Error ? err.message : err
      }`;
      for (const [id, title] of Object.entries(BED_CHECKS)) {
        add(id, title, "fail", why);
      }
    }
  }

  // 3. the startup cost, inside the budgets: Mode A (in place) and Mode B
  // (the supervising broker, which `auto` starts for a host-scoped network
  // grant)
  if (args.deno) {
    const dir = Deno.makeTempDirSync({ prefix: "ks-startup-" });
    Deno.writeTextFileSync(`${dir}/main.js`, "");
    const base = ["run", "--no-prompt", "--allow-read=.", "--no-lock"];
    const time = (extra: string[]) =>
      timeRuns(args.deno!, [...base, ...extra, "main.js"], dir);
    const off = await time(["--kernel-sandbox=off"]);
    const modeA = await time(["--kernel-sandbox=enforce"]);
    // a host-scoped grant only the broker holds exactly; nothing connects
    const net = "--allow-net=example.com:443";
    const offNet = await time(["--kernel-sandbox=off", net]);
    const modeB = await time(["--kernel-sandbox=broker", net]);
    Deno.removeSync(dir, { recursive: true });
    const measured = (
      id: string,
      title: string,
      base: number | null,
      on: number | null,
      budget: number,
      what: string,
      record: string,
    ) => {
      if (base === null || on === null) {
        add(id, title, "fail", `the measured ${what} runs failed to start`);
        return;
      }
      const added = on - base;
      add(
        id,
        title,
        added <= budget ? "pass" : "fail",
        `deno run adds ${
          added.toFixed(1)
        } ms under ${what} on this machine (median of 20; off ${
          base.toFixed(1)
        } ms, ${what} ${on.toFixed(1)} ms; budget ${budget} ms). ${record}`,
      );
    };
    measured(
      "3",
      "startup cost, Mode A",
      off,
      modeA,
      STARTUP_BUDGET_MS,
      "enforce",
      "Record the matrix hosts' numbers, and that a compiled program and a desktop app add no exec",
    );
    measured(
      "3b",
      "startup cost, Mode B",
      offNet,
      modeB,
      BROKER_STARTUP_BUDGET_MS,
      "the broker",
      "Record the matrix hosts' numbers (if the budget fails, auto keeps Mode B for names only: plan O7)",
    );
  } else {
    add(
      "3",
      "startup cost, Mode A",
      "manual",
      `measure with --deno <binary> on each matrix host: at most ${STARTUP_BUDGET_MS} ms added to deno run, no added exec for a compiled program or a desktop app`,
    );
    add(
      "3b",
      "startup cost, Mode B",
      "manual",
      `measure with --deno <binary> on each matrix host: at most ${BROKER_STARTUP_BUDGET_MS} ms added to deno run with the supervising broker (a host-scoped network grant)`,
    );
  }

  // 4. the first-failure message, the report, doctor and the break-glass
  {
    const state = read("runtime/permissions/kernel_sandbox_state.rs") ?? "";
    const mode = read("cli/lib/kernel_sandbox.rs") ?? "";
    const flags = read("libs/cli_parser/src/flags.rs") ?? "";
    const missing = [
      state.includes("refused by the kernel sandbox")
        ? null
        : "the refusal note",
      state.includes("a child process") ? null : "the child-failure hint",
      mode.includes('"DENO_KERNEL_SANDBOX"') ? null : "DENO_KERNEL_SANDBOX",
      flags.includes("kernel_sandbox_report")
        ? null
        : "--kernel-sandbox-report",
    ].filter(Boolean);
    const docs = args.denext
      ? [
        ["kernel-sandbox-troubleshooting", "refused by the kernel sandbox"],
        ["kernel-sandbox-modes", "DENO_KERNEL_SANDBOX"],
        ["kernel-sandbox-modes", "--kernel-sandbox-report"],
        ["kernel-sandbox-migrating", "denext kernel-sandbox check"],
        ["kernel-sandbox-doctor", "denext doctor"],
      ].filter(([page, text]) => !(denextDoc(page) ?? "").includes(text))
        .map(([page, text]) => `docs/${page} doesn't mention "${text}"`)
      : [];
    if (missing.length || docs.length) {
      add(
        "4",
        "failure message, report, doctor, break-glass",
        "fail",
        [...missing.map((m) => `${m} is missing`), ...docs].join("; "),
      );
    } else {
      add(
        "4",
        "failure message, report, doctor, break-glass",
        args.denext ? "pass" : "manual",
        args.denext
          ? "all four exist in the runtime and are documented"
          : "all four exist in the runtime; pass --denext <checkout> to check the documentation",
      );
    }
  }

  // 5. unfixed runs resolve to off, with tests
  {
    const mode = read("cli/lib/kernel_sandbox.rs") ?? "";
    const rows = [
      '"--watch"',
      '"denext dev"',
      '"a permission broker"',
      '"a prompt can happen"',
    ];
    const missing = rows.filter((r) => !mode.includes(r));
    add(
      "5",
      "--watch, HMR, denext dev and a permission broker stay off",
      mode.includes("fn auto_stage_matrix") && !missing.length
        ? "pass"
        : "fail",
      missing.length
        ? `auto_stage_matrix has no row for ${missing.join(", ")}`
        : "auto_stage_matrix asserts off at every stage for each (cargo test -p deno_lib auto_stage_matrix)",
    );
  }

  // 6. no gap is silent
  {
    const tests = read("cli/sandbox/policy/tests.rs") ?? "";
    const warn = read("cli/sandbox/warn.rs") ?? "";
    const ok = tests.includes("fatal_gaps") &&
      warn.includes("fn fatal_gaps_are_the_fail_open_ones");
    add(
      "6",
      "no policy gap is silent",
      ok ? "manual" : "fail",
      ok
        ? "record the review: every PolicyGap the compiler can produce prints in --kernel-sandbox-report, a fail-open one refuses enforce, and warn mode reports it as a startup refusal"
        : "the gap tests are missing",
    );
  }

  // 7. SECURITY.md states the boundary; one dry-run advisory went through
  {
    // the policy lives in denext's repository (the runtime has none of its
    // own); without a checkout it can't be read
    let security: string | null = null;
    if (args.denext) {
      try {
        security = Deno.readTextFileSync(`${args.denext}/SECURITY.md`)
          .toLowerCase();
      } catch {
        security = "";
      }
    }
    if (security === null) {
      add(
        "7",
        "SECURITY.md and a dry-run advisory",
        "manual",
        "pass --denext <checkout> to check that SECURITY.md states the boundary, and record the dry-run advisory that went through the private advisory fork",
      );
    } else if (!security.includes("kernel sandbox")) {
      add(
        "7",
        "SECURITY.md and a dry-run advisory",
        "fail",
        "denext's SECURITY.md doesn't state the kernel sandbox's boundary (an escape from a kernel-held cell of the guarantee matrix is a vulnerability; a documented gap is not)",
      );
    } else {
      add(
        "7",
        "SECURITY.md and a dry-run advisory",
        "manual",
        "record the dry-run advisory that went through the private advisory fork",
      );
    }
  }

  // 8. replaced by O1 (R3 never shipped as a default): both stage beds
  // soaked, then two 5.0 release candidates on this runtime without a stage
  // revert; decided with precondition 2's runs (bedChecks)

  const evidence = JSON.parse(
    read("tests/kernel_sandbox_beds/r4_evidence.json") ?? "{}",
  );
  const problems = evidenceProblems(evidence, checks.map((c) => c.id));
  add(
    "evidence",
    "r4_evidence.json fits its schema",
    problems.length ? "fail" : "pass",
    problems.length
      ? problems.join("; ")
      : "every entry names a precondition and says where its evidence is",
  );
  const result = withEvidence(checks, evidence);
  const ready = result.every((c) =>
    c.status === "pass" || c.status === "pending"
  );
  if (args.json) {
    console.log(JSON.stringify({ candidate, ready, checks: result }, null, 2));
  } else {
    console.log(
      `R4 preconditions, candidate ${candidate.slice(0, 10)}: ${
        ready ? "READY" : "not ready"
      }`,
    );
    const mark = { pass: "✓", fail: "✗", manual: "?", pending: "…" };
    for (const c of result) {
      console.log(
        `  ${mark[c.status]} ${c.id.padEnd(11)} ${c.title}: ${c.detail}`,
      );
    }
    if (!ready) {
      console.log(
        "  ? = evidence a person records in tests/kernel_sandbox_beds/r4_evidence.json (id → where it is)",
      );
    }
    if (result.some((c) => c.status === "pending")) {
      console.log(
        "  … = pending: evidence that can only come after the flip ships; it gates 5.0.0 final, not the flip",
      );
    }
  }
  Deno.exit(ready ? 0 : 1);
}
