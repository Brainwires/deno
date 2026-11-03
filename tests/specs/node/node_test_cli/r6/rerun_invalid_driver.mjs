import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
// `--test-rerun-failures` with a state file that isn't JSON, or isn't an
// array of attempts: the "not a valid rerun file" warning, the tests still
// run, and the file is left as it was.
for (const args of [[], ["--test"]]) {
  for (const content of ["{not json", "{}", "5", "[1]"]) {
    const dir = mkdtempSync(join(tmpdir(), "rerun-invalid-"));
    const state = join(dir, "state.json");
    writeFileSync(state, content);
    const { status, stdout, stderr } = spawnSync(process.execPath, [
      ...args,
      "--test-reporter=spec",
      "--test-rerun-failures",
      state,
      "rerun.test.js",
    ], { encoding: "utf8" });
    console.log(
      args.join(" ") || "(no --test)",
      JSON.stringify(content),
      status,
      stdout.includes("✔ passes"),
      (stdout + stderr).includes("is not a valid rerun file"),
      readFileSync(state, "utf8") === content,
    );
    rmSync(dir, { recursive: true });
  }
}
