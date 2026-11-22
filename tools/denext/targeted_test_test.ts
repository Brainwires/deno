// Copyright 2018-2026 the Deno authors. MIT license.

// denext: the targeted-run dispatcher's argument checks and commands
// (tools/README.md, "denext/targeted_test.ts"). Run with:
//   deno test tools/denext/targeted_test_test.ts

import assert from "node:assert/strict";
import {
  checkItem,
  commands,
  defaultBranch,
  type Options,
  parseArgs,
} from "./targeted_test.ts";

Deno.test("parseArgs: items, legs and defaults", () => {
  const o = parseArgs([
    "--os",
    "linux,windows-arm",
    "specs:kernel_sandbox::policy::config",
    "unit:pty_test libs:deno_permissions:kernel_broker::net",
  ]) as Options;
  assert.equal(typeof o, "object");
  assert.deepEqual(o.items, [
    "specs:kernel_sandbox::policy::config",
    "unit:pty_test",
    "libs:deno_permissions:kernel_broker::net",
  ]);
  assert.equal(o.os, "linux,windows-arm");
  assert.equal(o.commit, "HEAD");
  assert.equal(o.remote, "origin");
});

Deno.test("parseArgs: refusals", () => {
  for (
    const [args, why] of [
      [[], "at least one"],
      [["--os", "linux, windows", "unit"], "unknown leg"],
      [["--os", "ubuntu-22.04", "unit"], "unknown leg"],
      [["lint"], "unknown suite"],
      [["unit:"], "empty filter"],
      [["libs"], "libs:<crate>"],
      [["specs:$(id)"], "bad filter"],
      [["specs:-x"], "bad filter"],
      [["--branch", "train/denext28", "unit"], "pretest/"],
      [["--bogus", "unit"], "unknown option"],
      [["--os"], "needs a value"],
    ] as [string[], string][]
  ) {
    const r = parseArgs(args);
    assert.equal(typeof r, "string", args.join(" "));
    assert.match(r as string, new RegExp(why.replace(/[$()<>]/g, "\\$&")));
  }
});

Deno.test("checkItem accepts every documented form", () => {
  for (
    const item of [
      "specs",
      "specs:kernel_sandbox::policy::config",
      "unit:pty_test",
      "unit_node:dgram_test",
      "integration:run::foo",
      "node_compat:parallel/test-dgram-bind.js",
      "libs:deno_core",
      "libs:deno_permissions:kernel_broker::net",
    ]
  ) {
    assert.equal(checkItem(item), "", item);
  }
});

Deno.test("commands: push a pretest branch, dispatch on it", () => {
  const o = parseArgs([
    "--os",
    "windows",
    "unit:pty_test",
    "specs:a",
  ]) as Options;
  const sha = "0d2b070f31d3348d02209fba3dacca6ae3c96ea0";
  o.branch = defaultBranch(sha, new Date("2026-11-22T17:05:09.123Z"));
  assert.equal(o.branch, "pretest/targeted-0d2b070f31d3-20261122t170509");
  assert.deepEqual(commands(o, sha), [
    ["git", "push", "origin", `${sha}:refs/heads/${o.branch}`],
    [
      "gh",
      "workflow",
      "run",
      "denext_tests.yml",
      "-R",
      "Brainwires/deno",
      "--ref",
      o.branch,
      "-f",
      "only=unit:pty_test specs:a",
      "-f",
      "os=windows",
    ],
  ]);
});
