// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// denext (DENEXT.md item 472): what must hold before `--kernel-sandbox=auto`
// moves to stage R4 (enforce wherever the permission set is fixed, a bare run
// with no terminal included). One check per precondition; each is `pass`,
// `fail`, or `manual` (a person records the evidence in r4_evidence.json,
// which turns it into `pass`).
//
//   deno run -A tests/kernel_sandbox_beds/r4_preconditions.ts \
//     [--candidate <sha>] [--deno target/release/deno] [--denext ../denext] \
//     [--no-soak] [--json]
//
// Exits 0 only when every precondition passes. `--deno` measures the startup
// cost with that binary (without it the check is `manual`); `--denext` is a
// checkout of Brainwires/denext, for the documentation checks; `--no-soak`
// skips the GitHub request (the soak then reads `manual`).

import { parseArgs } from "jsr:@std/cli@1/parse-args";
import { parse as parseJsonc } from "jsr:@std/jsonc@1";
import { fromFileUrl } from "jsr:@std/path@1/from-file-url";

export type Status = "pass" | "fail" | "manual";

export interface Check {
  id: string;
  title: string;
  status: Status;
  detail: string;
}

/** The most `deno run` may cost more under enforce than without it. */
export const STARTUP_BUDGET_MS = 30;

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

/** Applies the recorded evidence: a `manual` check with an entry passes. */
export function withEvidence(
  checks: Check[],
  evidence: Record<string, unknown>,
): Check[] {
  return checks.map((c) =>
    c.status === "manual" && typeof evidence[c.id] === "string" &&
      (evidence[c.id] as string).trim()
      ? { ...c, status: "pass", detail: `recorded: ${evidence[c.id]}` }
      : c
  );
}

if (import.meta.main) {
  const args = parseArgs(Deno.args, {
    string: ["candidate", "deno", "denext"],
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

  // 2. every bed green seven times in a row; the lists settled
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
    for (const bed of beds) {
      if (!args.soak) {
        add(
          `2b-${bed}`,
          `soak: ${bed}`,
          "manual",
          "run soak.ts (skipped with --no-soak)",
        );
        continue;
      }
      const out = await output("deno", [
        "run",
        "-A",
        "tests/kernel_sandbox_beds/soak.ts",
        "--bed",
        bed,
        "--candidate",
        candidate,
        "--json",
      ]);
      let detail = "soak.ts failed (gh not signed in, or no network)";
      let status: Status = "fail";
      try {
        const soak = JSON.parse(out.text);
        status = soak.complete ? "pass" : "fail";
        detail = soak.complete
          ? `${soak.counted.length} green full runs in a row, the newest on the candidate`
          : soak.notes.join("; ");
      } catch {
        // keep the failure
      }
      add(`2b-${bed}`, `soak: ${bed}`, status, detail);
    }
  }

  // 3. the startup cost, inside the budget
  if (args.deno) {
    const dir = Deno.makeTempDirSync({ prefix: "ks-startup-" });
    Deno.writeTextFileSync(`${dir}/main.js`, "");
    const base = ["run", "--no-prompt", "--allow-read=.", "--no-lock"];
    const off = await timeRuns(args.deno, [
      ...base,
      "--kernel-sandbox=off",
      "main.js",
    ], dir);
    const on = await timeRuns(args.deno, [
      ...base,
      "--kernel-sandbox=enforce",
      "main.js",
    ], dir);
    Deno.removeSync(dir, { recursive: true });
    if (off === null || on === null) {
      add("3", "startup cost", "fail", "the measured runs failed to start");
    } else {
      const added = on - off;
      add(
        "3",
        "startup cost",
        added <= STARTUP_BUDGET_MS ? "pass" : "fail",
        `deno run adds ${
          added.toFixed(1)
        } ms under enforce on this machine (median of 20; off ${
          off.toFixed(1)
        } ms, enforce ${
          on.toFixed(1)
        } ms; budget ${STARTUP_BUDGET_MS} ms). Record the matrix hosts' numbers, and that a compiled program and a desktop app add no exec`,
      );
    }
  } else {
    add(
      "3",
      "startup cost",
      "manual",
      `measure with --deno <binary> on each matrix host: at most ${STARTUP_BUDGET_MS} ms added to deno run, no added exec for a compiled program or a desktop app`,
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

  // 8. R3 was the default in the two runtime releases before this one
  {
    const tags = (await output("git", [
      "tag",
      "--list",
      "denext-runtime-v*",
      "--sort=-creatordate",
      "--merged",
      candidate,
    ])).text.split("\n").map((t) => t.trim()).filter(Boolean).slice(0, 2);
    const stages: string[] = [];
    for (const tag of tags) {
      const src = await output("git", [
        "show",
        `${tag}:cli/lib/kernel_sandbox.rs`,
      ]);
      stages.push(`${tag}: ${(src.ok && shippedStage(src.text)) || "preview"}`);
    }
    const ok = tags.length === 2 && stages.every((s) => s.endsWith(": r3"));
    add(
      "8",
      "R3 shipped in the two releases before",
      ok ? "manual" : "fail",
      ok
        ? `${stages.join(", ")}; record that neither was reverted`
        : `the two newest runtime releases in the candidate's history must both ship stage R3 (${
          stages.join(", ") || "no release tags found"
        })`,
    );
  }

  const evidence = JSON.parse(
    read("tests/kernel_sandbox_beds/r4_evidence.json") ?? "{}",
  );
  const result = withEvidence(checks, evidence);
  const ready = result.every((c) => c.status === "pass");
  if (args.json) {
    console.log(JSON.stringify({ candidate, ready, checks: result }, null, 2));
  } else {
    console.log(
      `R4 preconditions, candidate ${candidate.slice(0, 10)}: ${
        ready ? "READY" : "not ready"
      }`,
    );
    const mark = { pass: "✓", fail: "✗", manual: "?" };
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
  }
  Deno.exit(ready ? 0 : 1);
}
