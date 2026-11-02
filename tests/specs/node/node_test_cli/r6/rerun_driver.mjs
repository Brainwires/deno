import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
// `--test-rerun-failures`: three attempts with one state file, with and
// without `--test`.
for (const args of [[], ["--test"]]) {
  const dir = mkdtempSync(join(tmpdir(), "rerun-"));
  const state = join(dir, "state.json");
  for (let attempt = 0; attempt < 3; attempt++) {
    const { status, stdout } = spawnSync(process.execPath, [
      ...args,
      "--test-reporter=spec",
      "--test-rerun-failures",
      state,
      "rerun.test.js",
    ], { encoding: "utf8" });
    const replayed = stdout.split("\n").filter((line) =>
      line.includes("passed on attempt")
    ).map((line) => line.replace(/\(\d+(\.\d+)?ms\) /, "").trim());
    const counts = stdout.match(/ℹ pass \d+|ℹ fail \d+/g);
    console.log(
      args.join(" ") || "(no --test)",
      attempt,
      status,
      counts,
      replayed,
    );
  }
  const runs = JSON.parse(readFileSync(state, "utf8"));
  console.log(runs.map((run) => Object.keys(run).sort()));
  rmSync(dir, { recursive: true });
}
