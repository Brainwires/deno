// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// denext (DENEXT.md item 957): the soak of the kernel sandbox beds, the gate
// in front of a rollout stage of `--kernel-sandbox=auto`.
//
// A stage's default may flip only after the beds
// (.github/workflows/denext_kernel_beds.yml) have run green SOAK_RUNS times in
// a row with that default forced on. The beds are dispatched by hand, never on
// a schedule, so "seven green nights" is counted in runs, with two rules that
// keep what the nights gave:
//
// - spread: a run counts only when it started SOAK_SPACING_HOURS or more after
//   the run counted before it, so seven dispatches in one minute are one run
//   (the soak spans a day at least, and catches what depends on the time and
//   on the runners' state);
// - the candidate: every counted run's commit is the candidate or one of its
//   ancestors, and the newest counted run is on the candidate itself, so the
//   soak proves the code that ships.
//
// A run counts when it is a full one (every suite, macOS included) of the
// asked-for bed that completed with every job green. A full run that failed or
// was cancelled resets the count: only runs newer than it count. Partial runs
// (one suite, no macOS) are ignored either way.
//
//   deno run -A tests/kernel_sandbox_beds/soak.ts --bed stage-r4 \
//     [--candidate <sha>] [--repo Brainwires/deno] [--json]
//
// Beds: `enforce`, `project` (the forced modes) and `stage-r3`, `stage-r4`
// (the defaults of a rollout stage, DENO_INTERNAL_KERNEL_SANDBOX_STAGE). Exits
// 0 when the soak is complete, 1 when it isn't. Reads the runs with `gh api`
// (one request) and the ancestry with `git merge-base`.

import { parseArgs } from "jsr:@std/cli@1/parse-args";

/** Green runs in a row a soak needs. */
export const SOAK_RUNS = 7;
/** Hours between two runs for the later one to count. */
export const SOAK_SPACING_HOURS = 4;
/** The suites of a full run. */
export const FULL_SUITES = ["specs", "node_compat", "npm_matrix", "denext"];
/** The beds a soak can be asked for. */
export const BEDS = ["enforce", "project", "stage-r3", "stage-r4"];

/** One run of the beds workflow, as the tracker reads it. */
export interface BedRun {
  id: number;
  /** The run's title (the workflow's `run-name`). */
  title: string;
  sha: string;
  /** `completed`, `in_progress`, `queued`. */
  status: string;
  /** `success`, `failure`, `cancelled`, … (null while it runs). */
  conclusion: string | null;
  /** ISO 8601. */
  startedAt: string;
  url?: string;
}

/** What a run's title says it ran. */
export interface BedRunKind {
  bed: string;
  suites: string[];
  macos: boolean;
}

/**
 * The workflow's `run-name`, parsed:
 * `kernel beds force=enforce stage=none suites=specs,node_compat macos=true`.
 * A run titled otherwise (one from before the title carried the inputs) has
 * no kind and is ignored.
 */
export function runKind(title: string): BedRunKind | null {
  const m =
    /^kernel beds force=(\S+) stage=(\S+) suites=(\S*) macos=(true|false)$/
      .exec(title.trim());
  if (!m) return null;
  const [, force, stage, suites, macos] = m;
  // a stage bed runs the stage's defaults with nothing forced; a forced bed
  // runs at the shipped stage
  let bed: string;
  if (stage !== "none" && force === "none") bed = `stage-${stage}`;
  else if (stage === "none" && force !== "none") bed = force;
  else return null;
  return {
    bed,
    suites: suites.split(",").map((s) => s.trim()).filter(Boolean).sort(),
    macos: macos === "true",
  };
}

/** Whether a run of `kind` is a full run of `bed`. */
export function isFullRun(kind: BedRunKind, bed: string): boolean {
  return kind.bed === bed && kind.macos &&
    FULL_SUITES.every((s) => kind.suites.includes(s));
}

/** Where a soak stands. */
export interface Soak {
  bed: string;
  candidate: string;
  /** The runs that count, oldest first. */
  counted: BedRun[];
  /** Runs still needed. */
  missing: number;
  complete: boolean;
  /** Why it isn't complete, or what to do next. */
  notes: string[];
}

/**
 * Counts the soak of `bed` for `candidate` from the workflow's runs.
 * `isAncestor(sha)` says whether `sha` is the candidate or one of its
 * ancestors.
 */
export function soak(
  runs: BedRun[],
  bed: string,
  candidate: string,
  isAncestor: (sha: string) => boolean,
): Soak {
  const notes: string[] = [];
  const full = runs
    .filter((r) => {
      const kind = runKind(r.title);
      return kind !== null && isFullRun(kind, bed);
    })
    .filter((r) => isAncestor(r.sha))
    .sort((a, b) => Date.parse(a.startedAt) - Date.parse(b.startedAt));
  // only what is newer than the last full run that completed without success
  const done = full.filter((r) => r.status === "completed");
  let from = 0;
  for (let i = 0; i < done.length; i++) {
    if (done[i].conclusion !== "success") from = i + 1;
  }
  if (from > 0) {
    const reset = done[from - 1];
    notes.push(
      `run ${reset.id} (${
        reset.sha.slice(0, 10)
      }) ended ${reset.conclusion}: the count starts after it`,
    );
  }
  const counted: BedRun[] = [];
  let skipped = 0;
  for (const run of done.slice(from)) {
    const last = counted[counted.length - 1];
    const apart = last
      ? (Date.parse(run.startedAt) - Date.parse(last.startedAt)) / 3_600_000
      : Infinity;
    if (apart >= SOAK_SPACING_HOURS) counted.push(run);
    else skipped++;
  }
  if (skipped > 0) {
    notes.push(
      `${skipped} green run(s) started less than ${SOAK_SPACING_HOURS} h after a counted one and count with it`,
    );
  }
  const running = full.filter((r) => r.status !== "completed").length;
  if (running > 0) notes.push(`${running} full run(s) still running`);
  const newest = done[done.length - 1];
  const onCandidate = newest !== undefined && newest.sha === candidate &&
    newest.conclusion === "success";
  const missing = Math.max(0, SOAK_RUNS - counted.length);
  if (missing > 0) {
    notes.push(
      `${counted.length} of ${SOAK_RUNS} counted: dispatch the full ${bed} bed ${missing} more time(s), ${SOAK_SPACING_HOURS} h apart`,
    );
  }
  if (!onCandidate) {
    notes.push(
      `the newest full run isn't a green one on the candidate ${
        candidate.slice(0, 10)
      }: dispatch the bed on it`,
    );
  }
  return {
    bed,
    candidate,
    counted,
    missing,
    complete: missing === 0 && onCandidate,
    notes,
  };
}

/** The `gh workflow run` line that dispatches a full run of `bed`. */
export function dispatchCommand(bed: string, ref: string): string {
  const inputs = bed.startsWith("stage-")
    ? `-f force=none -f stage=${bed.slice("stage-".length)}`
    : `-f force=${bed} -f stage=none`;
  return `gh workflow run denext_kernel_beds.yml --ref ${ref} ${inputs} -f suites=${
    FULL_SUITES.join(",")
  } -f macos=true`;
}

async function output(cmd: string, args: string[]): Promise<string> {
  const out = await new Deno.Command(cmd, { args, stderr: "inherit" }).output();
  if (!out.success) throw new Error(`${cmd} ${args.join(" ")} failed`);
  return new TextDecoder().decode(out.stdout);
}

if (import.meta.main) {
  const args = parseArgs(Deno.args, {
    string: ["bed", "candidate", "repo", "ref"],
    boolean: ["json"],
  });
  const bed = args.bed ?? "";
  if (!BEDS.includes(bed)) {
    throw new Error(`--bed is one of ${BEDS.join(", ")}`);
  }
  const repo = args.repo ?? "Brainwires/deno";
  const candidate = args.candidate ??
    (await output("git", ["rev-parse", "HEAD"])).trim();
  const listed = JSON.parse(
    await output("gh", [
      "api",
      `repos/${repo}/actions/workflows/denext_kernel_beds.yml/runs?event=workflow_dispatch&per_page=100`,
    ]),
  );
  // deno-lint-ignore no-explicit-any
  const runs: BedRun[] = listed.workflow_runs.map((r: any) => ({
    id: r.id,
    title: r.display_title ?? r.name ?? "",
    sha: r.head_sha,
    status: r.status,
    conclusion: r.conclusion,
    startedAt: r.run_started_at ?? r.created_at,
    url: r.html_url,
  }));
  const ancestry = new Map<string, boolean>();
  for (const sha of new Set(runs.map((r) => r.sha))) {
    const { success } = await new Deno.Command("git", {
      args: ["merge-base", "--is-ancestor", sha, candidate],
      stderr: "null",
    }).output();
    ancestry.set(sha, success);
  }
  const result = soak(
    runs,
    bed,
    candidate,
    (sha) => ancestry.get(sha) === true,
  );
  if (args.json) {
    console.log(JSON.stringify(result, null, 2));
  } else {
    console.log(
      `kernel beds soak, ${bed}, candidate ${candidate.slice(0, 10)}: ${
        result.complete ? "COMPLETE" : "not complete"
      } (${result.counted.length} of ${SOAK_RUNS})`,
    );
    for (const run of result.counted) {
      console.log(
        `  green  ${run.startedAt}  ${run.sha.slice(0, 10)}  ${
          run.url ?? run.id
        }`,
      );
    }
    for (const note of result.notes) console.log(`  ${note}`);
    if (!result.complete) {
      console.log(`  ${dispatchCommand(bed, args.ref ?? candidate)}`);
    }
  }
  Deno.exit(result.complete ? 0 : 1);
}
