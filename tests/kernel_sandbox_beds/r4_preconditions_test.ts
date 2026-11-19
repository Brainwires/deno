// Copyright 2018-2026 the Deno authors. MIT license.
// denext (DENEXT.md item 472): the pure parts of the R4 precondition check.
//
//   deno test tests/kernel_sandbox_beds/r4_preconditions_test.ts

import { assertEquals } from "jsr:@std/assert@1";
import {
  type Check,
  median,
  shippedStage,
  unsettledEntries,
  withEvidence,
} from "./r4_preconditions.ts";

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

Deno.test("median", () => {
  assertEquals(median([3, 1, 2]), 2);
  assertEquals(median([4, 1, 3, 2]), 2.5);
});
