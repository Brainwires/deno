// Copyright 2018-2026 the Deno authors. MIT license.

// denext: the flake hunt's carry-forward selection (tools/README.md,
// "Carry-forward"). Run with:
//   deno test tools/denext/flake_hunt_test.ts

import assert from "node:assert/strict";
import {
  baseLabel,
  carryIneligible,
  carryPlan,
  type CarrySource,
  classifyFile,
  computeImpact,
  type CrateGraph,
  flakySelection,
  formattingOnly,
  type Impact,
  legacyRequestedSuites,
  legacyUnits,
  mergeUnitRecords,
  type Unit,
  unitAffected,
  type UnitRecord,
  unitRecords,
} from "./flake_hunt.ts";

// A small workspace: deno (cli) -> deno_runtime -> deno_net -> deno_core;
// deno_foo stands alone (nothing the harness runs is built from it);
// test_util under tests/ (the harness's own crate).
const graph: CrateGraph = {
  packages: [
    { name: "deno_core", dir: "libs/core", targets: ["deno_core"], deps: [] },
    {
      name: "deno_net",
      dir: "ext/net",
      targets: ["deno_net"],
      deps: ["deno_core"],
    },
    {
      name: "deno_runtime",
      dir: "runtime",
      targets: ["deno_runtime"],
      deps: ["deno_net", "deno_core"],
    },
    {
      name: "deno",
      dir: "cli",
      targets: ["deno", "integration_like"],
      deps: ["deno_runtime", "test_util"],
    },
    { name: "deno_foo", dir: "libs/foo", targets: ["deno_foo"], deps: [] },
    {
      name: "test_util",
      dir: "tests/util/server",
      targets: ["test_util"],
      deps: [],
    },
    { name: "specs_tests", dir: "tests/specs", targets: ["specs"], deps: [] },
  ],
};

const specDirOf = (f: string) => {
  const m = f.match(/^(tests\/specs\/[^/]+\/[^/]+)\//);
  return m ? m[1] : undefined;
};
const classify = (f: string) => classifyFile(f, graph, specDirOf);
const impactOf = (...files: string[]) =>
  computeImpact(files.map(classify), graph);

/** A unit as buildUnits makes it (only what the carry-forward reads). */
function unit(
  label: string,
  key: Unit["key"],
  reps = 10,
): Unit {
  const suite = label.split(" ")[0];
  return {
    suite,
    label,
    key,
    kind: suite === "libs" || suite === "libs_core" ? "libtest" : "harness",
    exes: [],
    env: {},
    reps,
    timeoutMin: 60,
  } as Unit;
}

function source(units: UnitRecord[], extra: Partial<CarrySource> = {}) {
  return {
    runId: "R1",
    sha: "a".repeat(40),
    legacy: false,
    mode: "fast",
    requestedSuites: ["libs", "unit"],
    qualifies: true,
    reps: 10,
    flakyReps: 30,
    profiles: ["normal", "cpu1", "cpu0.5"],
    constrainedReps: 3,
    units,
    ...extra,
  } satisfies CarrySource;
}

const opts = {
  flakyReps: 30,
  constrainedReps: 3,
  profiles: ["normal", "cpu1", "cpu0.5"],
};
const clean = (label: string, reps = 10): UnitRecord => ({
  label,
  suite: label.split(" ")[0],
  reps,
  constrainedReps: 3,
  failures: [],
});
const none: Impact = impactOf();

Deno.test("baseLabel drops a profile suffix only", () => {
  assert.equal(baseLabel("unit unit::pty_test [cpu1]"), "unit unit::pty_test");
  assert.equal(baseLabel("specs (1/4) [cpu0.5+hog]"), "specs (1/4)");
  assert.equal(baseLabel("libs deno_net"), "libs deno_net");
  assert.equal(baseLabel("libs x [weird]"), "libs x [weird]");
});

Deno.test("classifyFile: docs and the hunt's tooling affect nothing", () => {
  for (
    const f of [
      "DENEXT.md",
      "README.md",
      "doc/testing.md",
      ".github/SECURITY.md",
      ".github/workflows/ci.ts",
      "tools/README.md",
      "tools/denext/flake_hunt.ts",
      "tools/denext/flake_hunt_test.ts",
      ".dprint.json",
    ]
  ) assert.deepEqual(classify(f), { kind: "ignore" }, f);
});

Deno.test("classifyFile: shared inputs affect everything", () => {
  for (
    const f of [
      "Cargo.lock",
      "Cargo.toml",
      "rust-toolchain.toml",
      ".github/workflows/denext_tests_os.yml",
      ".github/denext-runtime/rusty_v8.sha256",
      "tests/testdata/run/001_hello.js",
      "tests/util/std/assert/mod.ts",
      "tests/registry/npm/x/registry.json",
      "tools/glibc_math_shim.c",
    ]
  ) assert.deepEqual(classify(f), { kind: "all" }, f);
});

Deno.test("classifyFile: markdown a crate reads is the crate's", () => {
  // build.rs reads a hash pinned in this README: not a doc
  assert.deepEqual(classify("cli/tools/appimage_runtime/README.md"), {
    kind: "crate",
    crate: "deno",
  });
});

Deno.test("classifyFile: test files narrow to their tests", () => {
  assert.deepEqual(classify("tests/unit/pty_test.ts"), {
    kind: "test",
    suite: "unit",
    name: "unit::pty_test",
  });
  assert.deepEqual(
    classify("tests/unit/__snapshots__/lint_plugin_test.ts.snap"),
    { kind: "test", suite: "unit", name: "unit::lint_plugin_test" },
  );
  assert.deepEqual(classify("tests/unit/test_util.ts"), {
    kind: "suite",
    suite: "unit",
  });
  assert.deepEqual(classify("tests/unit_node/_fs/_fs_dir_test.ts"), {
    kind: "test",
    suite: "unit_node",
    name: "unit_node::_fs::_fs_dir_test",
  });
  assert.deepEqual(classify("tests/unit_node/testdata/x.txt"), {
    kind: "suite",
    suite: "unit_node",
  });
  assert.deepEqual(classify("tests/specs/run/unref_stdin/main.ts"), {
    kind: "test",
    suite: "specs",
    name: "specs::run::unref_stdin",
  });
  assert.deepEqual(classify("tests/specs/mod.rs"), {
    kind: "suite",
    suite: "specs",
  });
  assert.deepEqual(classify("tests/integration/lsp_tests.rs"), {
    kind: "test",
    suite: "integration",
    name: "integration::lsp::",
  });
  assert.deepEqual(classify("tests/integration/mod.rs"), {
    kind: "suite",
    suite: "integration",
  });
  assert.deepEqual(classify("tests/node_compat/config.jsonc"), {
    kind: "suite",
    suite: "node_compat",
  });
});

Deno.test("classifyFile: crate files go to the innermost crate", () => {
  assert.deepEqual(classify("ext/net/happy_eyeballs.rs"), {
    kind: "crate",
    crate: "deno_net",
  });
  assert.deepEqual(classify("ext/net/01_net.js"), {
    kind: "crate",
    crate: "deno_net",
  });
  assert.deepEqual(classify("libs/foo/Cargo.toml"), {
    kind: "crate",
    crate: "deno_foo",
  });
  // under no crate: everything, to be safe
  assert.deepEqual(classify("x/strange.rs"), { kind: "all" });
});

Deno.test("computeImpact: a crate change reaches its dependents", () => {
  const im = impactOf("ext/net/lib.rs");
  assert.deepEqual(
    [...im.crates].sort(),
    ["deno", "deno_net", "deno_runtime"],
  );
  assert.equal(im.harness, true); // deno is built from it
  assert.equal(im.all, false);
});

Deno.test("computeImpact: a crate outside the harness binaries", () => {
  const im = impactOf("libs/foo/lib.rs");
  assert.deepEqual([...im.crates], ["deno_foo"]);
  assert.equal(im.harness, false);
});

Deno.test("computeImpact: docs and test files", () => {
  const im = impactOf("DENEXT.md", "tests/unit/pty_test.ts");
  assert.equal(im.all, false);
  assert.equal(im.harness, false);
  assert.equal(im.crates.size, 0);
  assert.deepEqual(im.tests, [{ suite: "unit", name: "unit::pty_test" }]);
});

Deno.test("unitAffected: lib test binaries by crate", () => {
  const im = impactOf("ext/net/lib.rs");
  const net = unit("libs deno_net", { crate: "deno_net" });
  const foo = unit("libs deno_foo", { crate: "deno_foo" });
  const core = unit("libs_core deno_core", { crate: "deno_core" });
  const cliTest = unit("libs integration_like", { crate: "integration_like" });
  const unknown = unit("libs mystery", { crate: "mystery" });
  assert.equal(unitAffected(net, im, graph), true);
  assert.equal(unitAffected(foo, im, graph), false);
  assert.equal(unitAffected(core, im, graph), false); // a dependency, not a dependent
  assert.equal(unitAffected(cliTest, im, graph), true); // deno's test target
  assert.equal(unitAffected(unknown, im, graph), true); // not provable: affected
  assert.equal(unitAffected(net, impactOf("Cargo.lock"), graph), true);
});

Deno.test("unitAffected: harness units", () => {
  const pty = unit("unit unit::pty_test", { file: "unit::pty_test" });
  const blob = unit("unit unit::blob_test", { file: "unit::blob_test" });
  const known = unit("unit ~ unit::pty_test", { filter: "unit::pty_test" });
  const webrtc = unit("unit ~ webrtc_test", { filter: "webrtc_test" });
  const shard = unit("specs (1/4)", { shard: 0 });
  const spec = unit("specs ~ specs::run::unref_stdin", {
    filter: "specs::run::unref_stdin",
  });
  const other = unit("specs ~ specs::run::other", {
    filter: "specs::run::other",
  });
  const im = impactOf("tests/unit/pty_test.ts");
  assert.equal(unitAffected(pty, im, graph), true);
  assert.equal(unitAffected(blob, im, graph), false);
  assert.equal(unitAffected(known, im, graph), true);
  assert.equal(unitAffected(webrtc, im, graph), false); // a perFile name match
  const sim = impactOf("tests/specs/run/unref_stdin/__test__.jsonc");
  assert.equal(unitAffected(spec, sim, graph), true);
  assert.equal(unitAffected(other, sim, graph), false);
  // a shard keeps its other tests' results; the touched test runs alone
  assert.equal(unitAffected(shard, sim, graph), false);
  // a crate the binaries are built from: every harness unit
  const rt = impactOf("runtime/worker.rs");
  for (const u of [pty, blob, known, webrtc, shard, spec]) {
    assert.equal(unitAffected(u, rt, graph), true, u.label);
  }
  // a crate outside them: no harness unit
  const foo = impactOf("libs/foo/lib.rs");
  for (const u of [pty, blob, known, webrtc, shard, spec]) {
    assert.equal(unitAffected(u, foo, graph), false, u.label);
  }
  // a suite's shared file: all of that suite, nothing else
  const util = impactOf("tests/unit/test_util.ts");
  assert.equal(unitAffected(blob, util, graph), true);
  assert.equal(unitAffected(shard, util, graph), false);
});

Deno.test("unitAffected: a non-path filter in a sharded suite is conservative", () => {
  const u = unit("integration ~ lsp_large", { filter: "lsp_large" });
  const im = impactOf("tests/integration/lsp_tests.rs");
  assert.equal(unitAffected(u, im, graph), true);
  const mod = unit("integration ~ ::run::", { filter: "::run::" });
  assert.equal(unitAffected(mod, im, graph), false);
});

Deno.test("carryPlan: clean and untouched units carry; the rest runs", () => {
  const desired = [
    unit("libs deno_foo", { crate: "deno_foo" }),
    unit("libs deno_net", { crate: "deno_net" }),
    unit("unit unit::blob_test", { file: "unit::blob_test" }),
    unit("unit unit::new_test", { file: "unit::new_test" }),
  ];
  const src = source([
    clean("libs deno_foo"),
    clean("libs deno_net"),
    clean("unit unit::blob_test"),
  ]);
  const plan = carryPlan(
    desired,
    src,
    () => impactOf("libs/foo/x.rs"),
    graph,
    opts,
  );
  assert.deepEqual(plan.run.map((u) => u.label), [
    "libs deno_foo", // touched
    "unit unit::new_test", // not in the earlier run
  ]);
  assert.deepEqual(plan.carried.map((c) => [c.label, c.source]), [
    ["libs deno_net", { runId: "R1", sha: "a".repeat(40) }],
    ["unit unit::blob_test", { runId: "R1", sha: "a".repeat(40) }],
  ]);
  assert.deepEqual(plan.extra, []);
});

Deno.test("carryPlan: a formatting-only or docs diff carries everything", () => {
  const desired = [
    unit("libs deno_net", { crate: "deno_net" }),
    unit("specs (1/4)", { shard: 0 }),
  ];
  const src = source([clean("libs deno_net"), clean("specs (1/4)")]);
  const plan = carryPlan(desired, src, () => none, graph, opts);
  assert.equal(plan.run.length, 0);
  assert.equal(plan.carried.length, 2);
});

Deno.test("carryPlan: flaky tests rerun alone at --flaky-reps", () => {
  const desired = [
    unit("node_compat (1/4)", { shard: 0 }),
    unit("libs deno_net", { crate: "deno_net" }),
  ];
  const src = source([
    {
      ...clean("node_compat (1/4)"),
      failures: [
        { test: "node_compat::parallel::test-math-random.js", kind: "retried" },
        { test: "node_compat::parallel::test-math-random.js", kind: "failed" },
      ],
    },
    {
      ...clean("libs deno_net"),
      failures: [{
        test: "deno_net::happy_eyeballs::tests::test_parallel_second_wins",
        kind: "failed",
      }],
    },
  ]);
  const plan = carryPlan(desired, src, () => none, graph, opts);
  // the shard's other tests and the crate's other tests are vetted
  assert.equal(plan.run.length, 0);
  assert.deepEqual(plan.carried.map((c) => c.pending), [
    ["node_compat::parallel::test-math-random.js"],
    ["deno_net::happy_eyeballs::tests::test_parallel_second_wins"],
  ]);
  assert.deepEqual(plan.extra, [
    {
      suite: "node_compat",
      filter: "node_compat::parallel::test-math-random.js",
      reps: 30,
      why: "flaky in R1",
    },
    {
      suite: "libs",
      step: "libs",
      crates: ["deno_net"],
      filter: "happy_eyeballs::tests::test_parallel_second_wins",
      reps: 30,
      why: "flaky in R1",
    },
  ]);
});

Deno.test("carryPlan: a touched unit with a flaky test runs, and so does the test", () => {
  const desired = [unit("libs deno_net", { crate: "deno_net" })];
  const src = source([{
    ...clean("libs deno_net"),
    failures: [{ test: "deno_net::a::b", kind: "retried" }],
  }]);
  const plan = carryPlan(
    desired,
    src,
    () => impactOf("ext/net/lib.rs"),
    graph,
    opts,
  );
  assert.deepEqual(plan.run.map((u) => u.label), ["libs deno_net"]);
  assert.equal(plan.carried.length, 0);
  assert.deepEqual(plan.extra.map((s) => [s.filter, s.reps]), [["a::b", 30]]);
});

Deno.test("carryPlan: a crash, a timeout or a dirty tree reruns the whole unit", () => {
  for (const kind of ["crashed", "timeout", "git-dirty"] as const) {
    const desired = [unit("integration (2/4)", { shard: 1 })];
    const src = source([{
      ...clean("integration (2/4)"),
      failures: [{ test: "integration (2/4) (whole run)", kind }],
    }]);
    const plan = carryPlan(desired, src, () => none, graph, opts);
    assert.deepEqual(plan.run.map((u) => u.label), ["integration (2/4)"]);
    assert.equal(plan.carried.length, 0);
  }
});

Deno.test("carryPlan: a carried result's diff starts at its own commit", () => {
  const old = "b".repeat(40);
  const desired = [
    unit("libs deno_net", { crate: "deno_net" }),
    unit("libs deno_foo", { crate: "deno_foo" }),
  ];
  // R1 carried deno_net from R0 (on commit b…): the diff from b… touches it,
  // the diff from R1's commit does not
  const src = source([
    { ...clean("libs deno_net"), source: { runId: "R0", sha: old } },
    { ...clean("libs deno_foo"), source: { runId: "R0", sha: old } },
  ]);
  const plan = carryPlan(
    desired,
    src,
    (sha) => sha === old ? impactOf("ext/net/lib.rs") : none,
    graph,
    opts,
  );
  assert.deepEqual(plan.run.map((u) => u.label), ["libs deno_net"]);
  assert.deepEqual(plan.carried.map((c) => [c.label, c.source]), [
    ["libs deno_foo", { runId: "R0", sha: old }],
  ]);
});

Deno.test("carryPlan: fewer repetitions than this run wants do not carry", () => {
  const desired = [unit("unit ~ webrtc_test", { filter: "webrtc_test" }, 30)];
  const src = source([clean("unit ~ webrtc_test", 10)]);
  const plan = carryPlan(desired, src, () => none, graph, opts);
  assert.deepEqual(plan.run.map((u) => u.label), ["unit ~ webrtc_test"]);
  assert.equal(plan.notes.length, 1);
});

Deno.test("carryPlan: an earlier run with weaker profiles carries nothing", () => {
  const desired = [unit("libs deno_foo", { crate: "deno_foo" })];
  for (
    const extra of [
      { profiles: ["normal"] },
      { constrainedReps: 1 },
    ]
  ) {
    const plan = carryPlan(
      desired,
      source([clean("libs deno_foo")], extra),
      () => none,
      graph,
      opts,
    );
    assert.equal(plan.run.length, 1);
    assert.equal(plan.carried.length, 0);
  }
});

Deno.test("carryPlan: a carried shard runs its touched tests alone", () => {
  const desired = [
    unit("specs (1/4)", { shard: 0 }),
    unit("specs (2/4)", { shard: 1 }),
    unit("integration (1/2)", { shard: 0 }),
  ];
  const src = source([
    clean("specs (1/4)"),
    clean("specs (2/4)"),
    clean("integration (1/2)"),
  ]);
  const plan = carryPlan(
    desired,
    src,
    () =>
      impactOf(
        "tests/specs/run/unref_stdin/main.ts",
        "tests/integration/lsp_tests.rs",
      ),
    graph,
    opts,
  );
  assert.equal(plan.run.length, 0);
  assert.equal(plan.carried.length, 3);
  assert.deepEqual(plan.extra.map((s) => [s.suite, s.filter, s.reps]), [
    ["specs", "specs::run::unref_stdin", 10],
    ["integration", "::lsp::", 10],
  ]);
});

Deno.test("flakySelection: lib tests by crate, harness tests by name", () => {
  assert.deepEqual(flakySelection("libs_core", "deno_core::a::b", 30, "R"), {
    suite: "libs_core",
    step: "libs_core",
    crates: ["deno_core"],
    filter: "a::b",
    reps: 30,
    why: "flaky in R",
  });
  assert.deepEqual(flakySelection("unit", "unit::pty_test", 30, "R"), {
    suite: "unit",
    filter: "unit::pty_test",
    reps: 30,
    why: "flaky in R",
  });
});

Deno.test("formattingOnly: whitespace, rustfmt and dprint reflows", () => {
  assert.equal(
    formattingOnly(
      "ext/net/lib.rs",
      "fn f(a: u32, b: u32) -> u32 { a + b }\n",
      "fn f(\n  a: u32,\n  b: u32,\n) -> u32 {\n  a + b\n}\n",
    ),
    true,
  );
  assert.equal(
    formattingOnly(
      "ext/web/01_x.js",
      "const x = foo(a, b);\n// a  comment\n",
      "const x = foo(\n  a,\n  b,\n);\n// a comment\n",
    ),
    true,
  );
  assert.equal(
    formattingOnly(
      "cli/x.rs",
      "let c = 'a'; fn g<'a>() {}",
      "let c = 'a';\nfn g<'a>() {}",
    ),
    true,
  );
});

Deno.test("formattingOnly: anything else is a real change", () => {
  // inside a string
  assert.equal(
    formattingOnly("cli/x.rs", 'let s = "a b";', 'let s = "a  b";'),
    false,
  );
  assert.equal(
    formattingOnly("cli/x.rs", 'let s = r#"a b"#;', 'let s = r#"a  b"#;'),
    false,
  );
  assert.equal(
    formattingOnly("ext/web/x.js", "const s = `a b`;", "const s = `a\nb`;"),
    false,
  );
  // a token split
  assert.equal(
    formattingOnly("cli/x.rs", "let ab = 1;", "let a b = 1;"),
    false,
  );
  // a real edit
  assert.equal(formattingOnly("cli/x.rs", "let a = 1;", "let a = 2;"), false);
  // a quote style change is not whitespace
  assert.equal(formattingOnly("ext/x.js", "f('a');", 'f("a");'), false);
  // tests, testdata and non-source files never qualify
  assert.equal(formattingOnly("tests/unit/x_test.ts", "a;", "a;\n"), false);
  assert.equal(formattingOnly("cli/testdata/x.ts", "a;", "a;\n"), false);
  assert.equal(formattingOnly("cli/x.md", "a", "a\n"), false);
  // an added or deleted file
  assert.equal(formattingOnly("cli/x.rs", undefined, "a"), false);
});

Deno.test("legacyUnits: a report.json from before the carry-forward", () => {
  const rep: Parameters<typeof legacyUnits>[0] = {
    flakes: [
      {
        test: "node_compat: node_compat::parallel::test-math-random.js",
        kinds: { retried: 2 },
        firstLog: "/h/flake-hunt/runs/X/logs/t0100-node_compat_1_4_-r1.log",
      },
      {
        test:
          "integration: integration (3/4) [cpu1] (harness exited 143 with no report) [cpu1]",
        kinds: { crashed: 1 },
        firstLog: "/h/flake-hunt/runs/X/logs/t0200-integration_3_4_-r1.log",
      },
      {
        test: "libs: deno_net (test binary exited 101; last test reported: a)",
        kinds: { crashed: 1 },
        firstLog: "/h/flake-hunt/runs/X/logs/t0300-libs_deno_net-r1.log",
      },
    ],
    runs: [
      { id: "t0100", label: "node_compat (1/4)", failures: 1 },
      { id: "t0101", label: "node_compat (1/4) [cpu1]", failures: 0 },
      { id: "t0102", label: "node_compat (2/4)", failures: 0 },
      { id: "t0200", label: "integration (3/4) [cpu1]", failures: 1 },
      { id: "t0300", label: "libs deno_net", failures: 1 },
      { id: "t0301", label: "libs deno_foo", failures: 0 },
      { id: "t0400", label: "unit ~ webrtc_test", failures: 0 },
      { id: "t0500", label: "specs (1/4)", failures: 1 }, // no flake names it
    ],
  };
  const units = new Map(
    legacyUnits(rep, "fast", 10, 30, 3).map((u) => [u.label, u]),
  );
  assert.deepEqual(units.get("node_compat (1/4)")!.failures, [
    { test: "node_compat::parallel::test-math-random.js", kind: "retried" },
  ]);
  assert.deepEqual(units.get("node_compat (2/4)")!.failures, []);
  assert.equal(units.get("integration (3/4)")!.failures[0].kind, "crashed");
  assert.equal(units.get("libs deno_net")!.failures[0].kind, "crashed");
  assert.deepEqual(units.get("libs deno_foo")!.failures, []);
  assert.equal(units.get("libs deno_foo")!.reps, 10);
  assert.equal(units.get("unit ~ webrtc_test")!.reps, 30); // known flaky
  assert.equal(units.get("specs (1/4)")!.failures[0].kind, "crashed");
});

Deno.test("legacyRequestedSuites: the --suite lines of the plan", () => {
  const log = [
    "[16:39:04]   unit_node x10  (ext/node/Cargo.toml)",
    "[16:39:04]   libs x10  (--suite)",
    "[16:39:04]   unit x10  (--suite)",
    "[16:39:04]   unit ~ webrtc_test x30  (known flaky: …)",
  ].join("\n");
  assert.deepEqual(legacyRequestedSuites(log), ["libs", "unit"]);
});

Deno.test("mergeUnitRecords: shares add up, carried units do not", () => {
  const src = { runId: "R0", sha: "b".repeat(40) };
  const merged = mergeUnitRecords([
    [
      {
        label: "libs x",
        suite: "libs",
        reps: 8,
        constrainedReps: 2,
        failures: [],
      },
      { ...clean("libs y"), source: src, pending: ["y::t"] },
    ],
    [
      {
        label: "libs x",
        suite: "libs",
        reps: 2,
        constrainedReps: 1,
        failures: [{ test: "x::t", kind: "failed" }],
      },
      {
        ...clean("libs y"),
        source: src,
        pending: ["y::t"],
        failures: [{ test: "y::t", kind: "retried" }],
      },
    ],
  ]);
  assert.deepEqual(merged[0], {
    label: "libs x",
    suite: "libs",
    reps: 10,
    constrainedReps: 3,
    failures: [{ test: "x::t", kind: "failed" }],
    pending: undefined,
  });
  assert.equal(merged[1].reps, 10);
  assert.deepEqual(merged[1].failures, [{ test: "y::t", kind: "retried" }]);
});

Deno.test("carryIneligible: finished, one commit, every share", () => {
  type St = Parameters<typeof carryIneligible>[0][number];
  const marker = {} as St["marker"];
  const st = (x: Partial<St>): St => ({
    state: "done",
    sha: "a",
    exit: 1,
    marker,
    format: 2,
    ...x,
  });
  assert.equal(carryIneligible([st({})]), undefined);
  assert.equal(
    carryIneligible([st({ share: [0, 4, 5] }), st({ share: [4, 5, 5] })]),
    undefined,
  );
  assert.match(carryIneligible([st({ share: [0, 4, 5] })])!, /share/);
  assert.match(carryIneligible([st({ state: "running" })])!, /finished/);
  assert.match(carryIneligible([st({ exit: 2 })])!, /infrastructure/);
  assert.match(carryIneligible([st({ marker: undefined })])!, /record/);
  assert.match(
    carryIneligible([
      st({ share: [0, 1, 2] }),
      st({ share: [1, 2, 2], sha: "b" }),
    ])!,
    /different commits/,
  );
  assert.match(carryIneligible([])!, /not found/);
  // from before the carry-forward: no shares recorded
  assert.equal(
    carryIneligible([st({ format: undefined }), st({ format: undefined })]),
    undefined,
  );
});

Deno.test("unitRecords: ran units and carried units' pending reruns", () => {
  const units = [unit("libs x", { crate: "x" }, 4)];
  const task = (u: string, suite: string) =>
    ({ unit: u, suite }) as Parameters<typeof unitRecords>[1][number]["task"];
  const fail = (test: string, kind: "failed" | "crashed") =>
    ({ test, kind }) as Parameters<
      typeof unitRecords
    >[1][number]["failures"][number];
  const results = [
    { task: task("libs x", "libs"), failures: [fail("x::t", "failed")] },
    { task: task("libs x", "libs"), failures: [fail("x::t", "failed")] },
    { task: task("libs y ~ t", "libs"), failures: [fail("y::t", "failed")] },
    {
      task: task("unit ~ unit::a_test", "unit"),
      failures: [fail("unit ~ unit::a_test (whole run)", "crashed")],
    },
  ];
  const src = { runId: "R0", sha: "b".repeat(40) };
  const recs = unitRecords(
    units,
    results,
    [
      { ...clean("libs y"), source: src, pending: ["y::t"] },
      {
        ...clean("unit unit::a_test"),
        source: src,
        pending: ["unit::a_test"],
      },
      { ...clean("libs z"), source: src },
    ],
    3,
    30,
  );
  assert.deepEqual(recs[0], {
    label: "libs x",
    suite: "libs",
    reps: 4,
    constrainedReps: 3,
    failures: [{ test: "x::t", kind: "failed" }],
  });
  assert.deepEqual(recs[1].failures, [{ test: "y::t", kind: "failed" }]);
  assert.equal(recs[2].failures[0].kind, "crashed");
  assert.deepEqual(recs[3].failures, []);
  assert.deepEqual(recs[3].source, src);
});
