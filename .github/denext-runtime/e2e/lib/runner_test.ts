// Copyright 2018-2026 the Deno authors. MIT license.
// The e2e runner's own logic, without a runtime or a display:
//
//   deno test -A --no-config --no-lock .github/denext-runtime/e2e/lib/

const dir = Deno.makeTempDirSync({ prefix: "denext-e2e-runner-test-" });
// runner.ts reads the results directory when it loads.
Deno.env.set("DENEXT_E2E_DIR", dir);
const { results, seenResults, waitResult } = await import("./runner.ts");

function assert(ok: boolean, what: string, detail?: unknown) {
  if (!ok) throw new Error(`${what}: ${JSON.stringify(detail)}`);
}

function writeResult(
  pid: number,
  startedAt: string,
  launch: string,
  done = true,
) {
  // As the apps write them (apps/_shared/e2e.ts Report): one file per pid.
  Deno.writeTextFileSync(
    `${dir}/pidreuse-${pid}.json`,
    JSON.stringify({
      area: "pidreuse",
      pid,
      startedAt,
      launch,
      params: {},
      checks: [],
      data: {},
      done,
    }),
  );
}

// Windows hands a pid to a new process seconds after the last one with it
// ended, and the new launch's result replaces the old one's file. The new
// launch's result is still found (the exit area, Brainwires/deno run
// 37720814135: launch #5 got launch #2's pid and was waited for 90 s).
Deno.test("a launch that reuses an earlier launch's pid is found", async () => {
  writeResult(4420, "2026-10-09T07:18:10.000Z", "n-exit-2");
  writeResult(9716, "2026-10-09T07:18:12.000Z", "n-exit-3");
  const seen = await seenResults("pidreuse");

  // Launch 4 starts and gets pid 4420 again.
  writeResult(4420, "2026-10-09T07:18:14.000Z", "n-exit-4");
  const r = await waitResult("pidreuse", {
    seen,
    launch: "n-exit-4",
    ms: 2_000,
  });
  assert(r?.launch === "n-exit-4", "launch 4's result", r);
  assert(r?.pid === 4420, "its pid", r);
});

Deno.test("results written before a launch are still skipped", async () => {
  writeResult(5840, "2026-10-09T07:19:00.000Z", "");
  const seen = await seenResults("pidreuse");
  const r = await waitResult("pidreuse", { seen, ms: 600 });
  assert(r === null, "no new result", r);
  assert(
    (await results("pidreuse")).some((x) => x.pid === 5840),
    "the old one is there",
  );
});

addEventListener("unload", () => {
  try {
    Deno.removeSync(dir, { recursive: true });
  } catch { /* gone */ }
});
