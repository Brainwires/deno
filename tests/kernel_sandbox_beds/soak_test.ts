// Copyright 2018-2026 the Deno authors. MIT license.
// denext (DENEXT.md item 472): the soak counter of the kernel sandbox beds.
//
//   deno test tests/kernel_sandbox_beds/soak_test.ts

import { assert, assertEquals } from "jsr:@std/assert@1";
import {
  type BedRun,
  dispatchCommand,
  isFullRun,
  runKind,
  soak,
  SOAK_RUNS,
} from "./soak.ts";

const FULL = "suites=specs,node_compat,npm_matrix,denext macos=true";
const title = (bed: string, rest = FULL) =>
  bed.startsWith("stage-")
    ? `kernel beds force=none stage=${bed.slice(6)} ${rest}`
    : `kernel beds force=${bed} stage=none ${rest}`;

let nextId = 1;
/** A run `hours` after the epoch of the test, on `sha`. */
function run(
  hours: number,
  sha: string,
  conclusion: string | null = "success",
  bed = "stage-r4",
  rest = FULL,
): BedRun {
  return {
    id: nextId++,
    title: title(bed, rest),
    sha,
    status: conclusion === null ? "in_progress" : "completed",
    conclusion,
    startedAt: new Date(Date.UTC(2026, 10, 20) + hours * 3_600_000)
      .toISOString(),
  };
}

const ancestors = (shas: string[]) => (sha: string) => shas.includes(sha);

Deno.test("a run's title says which bed it ran", () => {
  assertEquals(runKind(title("enforce")), {
    bed: "enforce",
    suites: ["denext", "node_compat", "npm_matrix", "specs"],
    macos: true,
  });
  assertEquals(runKind(title("stage-r4"))?.bed, "stage-r4");
  // a forced mode at a later stage is neither bed
  assertEquals(runKind(`kernel beds force=enforce stage=r4 ${FULL}`), null);
  assertEquals(runKind(`kernel beds force=none stage=none ${FULL}`), null);
  // a run from before the title carried the inputs
  assertEquals(runKind("denext kernel sandbox beds"), null);
  const partial = runKind(title("enforce", "suites=specs macos=true"))!;
  assert(!isFullRun(partial, "enforce"));
  const noMac = runKind(title("enforce", FULL.replace("true", "false")))!;
  assert(!isFullRun(noMac, "enforce"));
  assert(isFullRun(runKind(title("enforce"))!, "enforce"));
  assert(!isFullRun(runKind(title("enforce"))!, "project"));
});

Deno.test("seven green full runs, spaced, the newest on the candidate", () => {
  const runs = Array.from(
    { length: SOAK_RUNS },
    (_, i) => run(i * 4, i < 5 ? "a" : "c"),
  );
  const result = soak(runs, "stage-r4", "c", ancestors(["a", "c"]));
  assertEquals(result.counted.length, 7);
  assert(result.complete, result.notes.join("; "));
  // six is not seven
  const six = soak(runs.slice(1), "stage-r4", "c", ancestors(["a", "c"]));
  assertEquals([six.complete, six.missing], [false, 1]);
});

Deno.test("runs dispatched together count once", () => {
  // seven dispatches in one hour, then one four hours later
  const runs = [
    ...Array.from({ length: 7 }, (_, i) => run(i / 10, "c")),
    run(4, "c"),
  ];
  const result = soak(runs, "stage-r4", "c", ancestors(["c"]));
  assertEquals(result.counted.length, 2);
  assert(!result.complete);
  assert(result.notes.some((n) => n.includes("6 green run(s) started less")));
});

Deno.test("a red or cancelled full run resets the count", () => {
  for (const conclusion of ["failure", "cancelled", "timed_out"]) {
    const runs = [
      ...Array.from({ length: 6 }, (_, i) => run(i * 4, "a")),
      run(24, "a", conclusion),
      run(28, "c"),
      run(32, "c"),
    ];
    const result = soak(runs, "stage-r4", "c", ancestors(["a", "c"]));
    assertEquals(result.counted.length, 2, conclusion);
    assert(result.notes[0].includes(`ended ${conclusion}`));
  }
  // a partial run that failed doesn't: it isn't the bed
  const runs = [
    ...Array.from({ length: 6 }, (_, i) => run(i * 4, "c")),
    run(24, "c", "failure", "stage-r4", "suites=specs macos=false"),
    run(28, "c"),
  ];
  assert(soak(runs, "stage-r4", "c", ancestors(["c"])).complete);
});

Deno.test("only the candidate's own history counts, and it must end on it", () => {
  // runs on a commit that isn't an ancestor (another branch) don't count
  const other = Array.from({ length: 7 }, (_, i) => run(i * 4, "x"));
  assertEquals(
    soak(other, "stage-r4", "c", ancestors(["a", "c"])).counted.length,
    0,
  );
  // seven green on ancestors, none on the candidate itself
  const old = Array.from({ length: 7 }, (_, i) => run(i * 4, "a"));
  const result = soak(old, "stage-r4", "c", ancestors(["a", "c"]));
  assertEquals([result.missing, result.complete], [0, false]);
  assert(result.notes.some((n) => n.includes("on the candidate")));
  // another bed's runs don't count for this one
  const forced = Array.from(
    { length: 7 },
    (_, i) => run(i * 4, "c", "success", "enforce"),
  );
  assertEquals(
    soak(forced, "stage-r4", "c", ancestors(["c"])).counted.length,
    0,
  );
  assert(soak(forced, "enforce", "c", ancestors(["c"])).complete);
});

Deno.test("a run still running neither counts nor resets", () => {
  const runs = [
    ...Array.from({ length: 7 }, (_, i) => run(i * 4, "c")),
    run(28, "c", null),
  ];
  const result = soak(runs, "stage-r4", "c", ancestors(["c"]));
  assert(result.complete);
  assert(result.notes.some((n) => n.includes("still running")));
});

Deno.test("the dispatch line names the bed's inputs", () => {
  assertEquals(
    dispatchCommand("stage-r4", "train/denext31"),
    "gh workflow run denext_kernel_beds.yml --ref train/denext31 -f force=none -f stage=r4 -f suites=specs,node_compat,npm_matrix,denext -f macos=true",
  );
  assert(
    dispatchCommand("project", "x").includes("-f force=project -f stage=none"),
  );
});
