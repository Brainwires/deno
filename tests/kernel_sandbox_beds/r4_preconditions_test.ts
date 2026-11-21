// Copyright 2018-2026 the Deno authors. MIT license.
// denext (DENEXT.md item 472): the pure parts of the R4 precondition check.
//
//   deno test tests/kernel_sandbox_beds/r4_preconditions_test.ts

import { assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import {
  bedChecks,
  type Check,
  evidenceProblems,
  greenOnCandidate,
  median,
  shippedStage,
  unsettledEntries,
  withEvidence,
} from "./r4_preconditions.ts";
import type { BedRun } from "./soak.ts";

Deno.test("an expected failure is settled once its reason is", () => {
  const lists = {
    enforce: {
      specs: {
        "a": { reason: "documented (item 425): a dynamic import" },
        "b": { reason: "to triage (P5 second pass): unknown" },
        "c": { reason: "to fix (item 425 re-exec): prints twice" },
        "d": { reason: "" },
      },
    },
    project: { node_compat: { "e": { reason: "to triage" } } },
    "stage-r3": { specs: { "f": { reason: "to fix" } } },
  };
  assertEquals(unsettledEntries(lists, ["enforce", "project", "stage-r4"]), [
    "enforce > specs > b",
    "enforce > specs > c",
    "enforce > specs > d",
    "project > node_compat > e",
  ]);
  assertEquals(unsettledEntries({}, ["enforce"]), []);
});

Deno.test("the shipped stage is read from the constant", () => {
  assertEquals(
    shippedStage("pub const AUTO_STAGE: AutoStage = AutoStage::R3;"),
    "r3",
  );
  assertEquals(
    shippedStage("pub const AUTO_STAGE: AutoStage = AutoStage::Preview;"),
    "preview",
  );
  // a release before the stages
  assertEquals(shippedStage("pub const AUTO_APPLIES: bool = false;"), null);
});

Deno.test("recorded evidence turns a manual check into a pass, nothing else", () => {
  const checks: Check[] = [
    { id: "1", title: "matrix", status: "manual", detail: "record it" },
    { id: "2a", title: "lists", status: "fail", detail: "3 unsettled" },
    { id: "7", title: "advisory", status: "manual", detail: "record it" },
  ];
  const out = withEvidence(checks, {
    "1": "run 123, 2026-12-01",
    "2a": "x",
    "7": " ",
  });
  assertEquals(out.map((c) => c.status), ["pass", "fail", "manual"]);
  assertEquals(out[0].detail, "recorded: run 123, 2026-12-01");
});

Deno.test("an entry object records evidence, and a pending check takes it", () => {
  const checks: Check[] = [
    { id: "8", title: "rcs", status: "pending", detail: "to come" },
    { id: "3", title: "startup", status: "manual", detail: "measure" },
  ];
  const out = withEvidence(checks, {
    "8": { evidence: "5.0.0-rc.1, 5.0.0-rc.2", recorded: "2027-01-10" },
    "3": { evidence: " ", recorded: "2027-01-10" },
  });
  assertEquals(out.map((c) => c.status), ["pass", "manual"]);
  assertEquals(out[0].detail, "recorded: 5.0.0-rc.1, 5.0.0-rc.2 (2027-01-10)");
});

Deno.test("the evidence file's problems", () => {
  const ids = ["1", "3", "8"];
  assertEquals(
    evidenceProblems({ "$schema": "./x.json", "//": "c", "1": "run 1" }, ids),
    [],
  );
  assertEquals(
    evidenceProblems({
      "9": "x",
      "1": "",
      "3": { evidence: "run 2" },
      "8": { evidence: "rc", recorded: "2027-01-10", url: "x" },
    }, ids),
    [
      '"9" is no precondition\'s id',
      '"1" is empty',
      '"3" has no ISO 8601 recorded date',
      '"8" has unknown keys: url',
    ],
  );
});

const FULL = "suites=specs,node_compat,npm_matrix,denext macos=true";
let nextId = 1;
function bedRun(
  bed: string,
  hours: number,
  sha: string,
  conclusion = "success",
): BedRun {
  return {
    id: nextId++,
    title: bed.startsWith("stage-")
      ? `kernel beds force=none stage=${bed.slice(6)} ${FULL}`
      : `kernel beds force=${bed} stage=none ${FULL}`,
    sha,
    status: "completed",
    conclusion,
    startedAt: new Date(Date.UTC(2026, 10, 22) + hours * 3_600_000)
      .toISOString(),
  };
}

Deno.test("a forced bed counts when its newest full run on the candidate is green", () => {
  const runs = [
    bedRun("enforce", 0, "cand", "failure"),
    bedRun("enforce", 1, "cand"),
    bedRun("project", 0, "cand"),
    bedRun("project", 2, "cand", "failure"),
    bedRun("project", 3, "old"),
  ];
  assertEquals(greenOnCandidate(runs, "enforce", "cand")?.id, runs[1].id);
  assertEquals(greenOnCandidate(runs, "project", "cand"), null);
});

Deno.test("precondition 8 is pending once both stage beds soaked, failed before", () => {
  const isAncestor = (sha: string) => sha === "cand" || sha === "old";
  const soaked = (bed: string, sha: (i: number) => string) =>
    Array.from({ length: 7 }, (_, i) => bedRun(bed, i * 5, sha(i)));
  const r4 = soaked("stage-r4", (i) => (i === 6 ? "cand" : "old"));
  const r3 = soaked("stage-r3", () => "old");
  const byId = (checks: Check[]) =>
    Object.fromEntries(checks.map((c) => [c.id, c]));

  let checks = byId(bedChecks([...r4, ...r3], "cand", isAncestor, "main"));
  assertEquals(checks["2b-stage-r4"].status, "pass");
  assertEquals(checks["8"].status, "pending");
  assertStringIncludes(checks["8"].detail, "5.0.0 final");
  assertEquals(checks["2c-enforce"].status, "fail");

  // r3 short of seven counted runs
  checks = byId(bedChecks([...r4, ...r3.slice(1)], "cand", isAncestor, "main"));
  assertEquals(checks["8"].status, "fail");
  assertStringIncludes(checks["8"].detail, "stage-r3: 6 of 7");
  assertStringIncludes(checks["8"].detail, "-f stage=r3");

  // a failed stage-r4 run after the soak resets it
  checks = byId(
    bedChecks(
      [...r4, bedRun("stage-r4", 40, "cand", "failure"), ...r3],
      "cand",
      isAncestor,
      "main",
    ),
  );
  assertEquals(checks["2b-stage-r4"].status, "fail");
  assertEquals(checks["8"].status, "fail");
});

Deno.test("median", () => {
  assertEquals(median([3, 1, 2]), 2);
  assertEquals(median([4, 1, 3, 2]), 2.5);
});
