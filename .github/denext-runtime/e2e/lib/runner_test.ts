// Copyright 2018-2026 the Deno authors. MIT license.
// The e2e runner's own logic, without a runtime or a display:
//
//   deno test -A --no-config --no-lock .github/denext-runtime/e2e/lib/

const dir = Deno.makeTempDirSync({ prefix: "denext-e2e-runner-test-" });
// runner.ts reads the results directory when it loads.
Deno.env.set("DENEXT_E2E_DIR", dir);
const {
  kill,
  killByPath,
  launch,
  pidsUnder,
  processTable,
  ProcessTree,
  results,
  seenResults,
  waitResult,
} = await import("./runner.ts");

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

// ---------------------------------------------------------------------------
// Killing a launch's process tree by pid and start time.

const posix = Deno.build.os !== "windows";
const env = {
  target: "",
  backend: "cef" as const,
  runtimeDir: "",
  workDir: dir,
  nonce: "t",
};

async function readPids(file: string, n: number): Promise<number[]> {
  for (let i = 0; i < 100; i++) {
    const pids = (await Deno.readTextFile(file).catch(() => "")).split(/\s+/)
      .filter(Boolean).map(Number);
    if (pids.length >= n) return pids;
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`${file}: fewer than ${n} pids`);
}

/** Wait (up to 5 s) for every pid to be gone, then say which are not. */
async function survivors(pids: number[]): Promise<number[]> {
  let left = pids;
  for (let i = 0; i < 50 && left.length; i++) {
    const t = await processTable();
    left = pids.filter((p) => t.has(p));
    if (left.length) await new Promise((r) => setTimeout(r, 100));
  }
  return left;
}

// A Linux CEF helper re-execs itself through /proc/self/exe, so its command
// line need not name the artifact (pkill -f missed them), and its parent may
// exit and leave it reparented. Both are killed: they were seen while the
// launch ran. (waitExit's timer outlives kill(): no op sanitizer.)
Deno.test({
  name: "kill() kills renamed and orphaned descendants by pid",
  ignore: !posix,
  sanitizeOps: false,
  sanitizeResources: false,
}, async () => {
  const file = `${dir}/tree-pids`;
  const script = [
    'f="$1"',
    'sleep 300 & echo $! >>"$f"',
    // argv without any path to the executable.
    '(exec -a renamed-helper sleep 300) & echo $! >>"$f"',
    // A grandchild whose parent exits after a second: reparented.
    '(sleep 300 & echo $! >>"$f"; sh -c \'echo $PPID\' >"$f.mid"; sleep 1) &',
    "wait",
  ].join("\n");
  const l = await launch(env, "bash", ["-c", script, "tree", file]);
  const pids = await readPids(file, 3);
  // Let the middle subshell exit and orphan its child.
  await new Promise((r) => setTimeout(r, 2_500));
  const [mid] = await readPids(`${file}.mid`, 1);
  const t = await processTable();
  assert(!t.has(mid), "the grandchild's parent exited", t.get(mid));
  assert(t.has(pids[2]), "the grandchild runs on", pids[2]);
  for (const p of pids) {
    assert(l.tree.seen.has(p), `pid ${p} tracked`, [...l.tree.seen]);
  }
  await kill(l);
  // kill() returns once they have ended, not just been signalled: the next
  // launch must not find a profile lock still held.
  const after = await processTable();
  const running = pids.filter((p) =>
    after.has(p) && !after.get(p)!.state.startsWith("Z")
  );
  assert(running.length === 0, "ended when kill() returns", running);
  await l.exited;
  const left = await survivors([l.pid, ...pids]);
  assert(left.length === 0, "every process is gone", left);
});

// Processes the OS started (not our descendants), found by the executable
// they run, whatever their argv says.
Deno.test({
  name: "killByPath() kills a process by its executable, not its argv",
  ignore: !posix,
  sanitizeOps: false,
  sanitizeResources: false,
}, async () => {
  const artifact = `${dir}/artifact dir`;
  await Deno.mkdir(`${artifact}/bin`, { recursive: true });
  const helper = `${artifact}/bin/helper`;
  const sleepBin = new TextDecoder().decode(
    (await new Deno.Command("sh", { args: ["-c", "command -v sleep"] })
      .output()).stdout,
  ).trim();
  await Deno.copyFile(sleepBin, helper);
  await Deno.chmod(helper, 0o755);
  const file = `${dir}/path-pids`;
  // On Linux the argv names no path (as a /proc/self/exe re-exec); macOS's
  // ps reports argv[0] as the executable, so it keeps its path there.
  const argv0 = Deno.build.os === "linux" ? "-a zygote " : "";
  // Detached: the shell exits and the helper is reparented.
  await new Deno.Command("bash", {
    args: [
      "-c",
      `(exec ${argv0}"$1" 300 </dev/null >/dev/null 2>&1 & echo $! >"$2")`,
      "spawn",
      helper,
      file,
    ],
  }).output();
  const [pid] = await readPids(file, 1);
  const bystander = new Deno.Command("sleep", { args: ["300"] }).spawn();
  try {
    await killByPath(artifact);
    const now = await processTable();
    assert(
      !now.has(pid) || now.get(pid)!.state.startsWith("Z"),
      "ended when killByPath() returns",
      now.get(pid),
    );
    const left = await survivors([pid]);
    assert(left.length === 0, "the helper is gone", left);
    const t = await processTable();
    assert(t.has(bystander.pid), "a process outside the dir is not killed");
  } finally {
    bystander.kill("SIGKILL");
    await bystander.status;
  }
});

Deno.test("a tracked pid reused by another process is not alive", () => {
  const tree = new ProcessTree(100);
  tree.absorb(
    new Map([
      [100, { ppid: 1, start: "a", state: "S" }],
      [101, { ppid: 100, start: "b", state: "S" }],
      [102, { ppid: 101, start: "c", state: "S" }],
      [200, { ppid: 1, start: "d", state: "S" }],
    ]),
  );
  assert(
    [...tree.seen.keys()].sort().join() === "100,101,102",
    "the tree",
    [...tree.seen],
  );
  // 101 exited and its pid went to an unrelated process; 102 was reparented.
  const later = new Map([
    [100, { ppid: 1, start: "a", state: "S" }],
    [101, { ppid: 1, start: "z", state: "S" }],
    [102, { ppid: 1, start: "c", state: "S" }],
    [103, { ppid: 101, start: "y", state: "S" }],
  ]);
  tree.absorb(later);
  const alive = tree.alive(later).sort().join();
  assert(alive === "100,102", "only the same processes", alive);
  assert(!tree.seen.has(103), "the impostor's child is not adopted");
});

addEventListener("unload", () => {
  try {
    Deno.removeSync(dir, { recursive: true });
  } catch { /* gone */ }
});

Deno.test("Windows lists no processes and spawns no ps", async () => {
  // A plain Windows machine has no `ps` (the spawn threw NotFound and failed
  // every area), and Git for Windows' takes no -A / -o: the table is empty
  // there by design, and launches end through taskkill /T and killByPath.
  const t = await processTable("windows");
  assert(t.size === 0, "processTable on Windows", [...t.keys()]);
  const under = await pidsUnder(Deno.cwd(), "windows");
  assert(under.length === 0, "pidsUnder on Windows", under);
});
