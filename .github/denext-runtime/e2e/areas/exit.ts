// Copyright 2018-2026 the Deno authors. MIT license.
// How an app ends: Deno.exit(code), process.exit(code) and desktop.quit()
// each end the process (no hang) with that exit code, `unload` runs before
// Deno.exit(), and what the page stored just before the end (localStorage,
// IndexedDB) survives into the next launch. An exit from the runtime's thread
// used to end the process under a running engine on every OS (the profile
// unwritten; on Windows it could also hang in DLL detach code); it now goes
// through laufey's exit_app (rt_desktop `desktop_exit`) on Windows, macOS and
// Linux, with both backends.
//
// With CEF the end is also quick and clean: the process is gone within
// QUIT_BUDGET_MS of the app asking for it (the host waits a bounded 5 s for
// its helper processes after CefShutdown, and on macOS it used to wait for
// Chromium's code-sign-clone-cleanup, which itself waits for the browser
// process to exit, so every end took over 5 s), the host reports no helper
// outliving its shutdown, and nothing the launch started outlives it but the
// helpers that end only after the browser process (AFTER_HOST), which are
// gone within moments of it.

import {
  type AreaReport,
  clearResults,
  type Env,
  kill,
  launch,
  type Launched,
  OS,
  packageApp,
  pidsUnder,
  processTable,
  results,
  seenResults,
  sh,
  sleep,
  tail,
  waitExit,
  waitResult,
  writeParams,
} from "../lib/runner.ts";

// Each launch ends one way; the next one reads what it stored.
const STEPS = [
  { how: "deno-exit", code: 3 },
  { how: "process-exit", code: 4 },
  { how: "quit", code: 0 },
  { how: "deno-exit", code: 0 },
  { how: "deno-exit", code: 5 },
];

// Then quit-and-relaunch at once, without the runner ending what the
// previous launch started (E2E_EXIT_QUICK_RELAUNCHES, default 10): the
// previous launch's helpers may still hold the profile's LevelDB LOCK files
// when the next one starts, which ran its localStorage in memory only and
// lost what the page stored (laufey now waits for both).
const QUICK = Number(Deno.env.get("E2E_EXIT_QUICK_RELAUNCHES") ?? 10);

// From the app asking to end to its process being gone (and its output
// closed: a helper still holding the host's stdout or stderr counts).
const QUIT_BUDGET_MS = Number(
  Deno.env.get("E2E_EXIT_QUIT_BUDGET_MS") ?? 3_000,
);
// The CEF host's line when a helper outlived its post-shutdown wait.
const HELPER_LEFT = /helper process\(es\) still running/;
// Chromium's helpers that end only after the browser process, which the host
// doesn't wait for (laufey's IsCefHelperToWaitFor): the zygotes and the crash
// handler (Linux), and the code-signing clone's cleanup (macOS), which polls
// once a second for its parent's exit.
const AFTER_HOST = /--type=(zygote|crashpad-handler|code-sign-clone-cleanup)\b/;

/** The processes of launch `l` still running once its process has ended:
 * those it was seen to start, and any running from the app's bundle (one
 * started too late for the runner's poll), with their command lines. */
async function survivors(l: Launched, artifact: string) {
  const t = await l.tree.track();
  const pids = new Set(l.tree.alive(t).filter((p) => p !== l.pid));
  for (const p of await pidsUnder(artifact)) pids.add(p);
  const out: { pid: number; start: string; command: string }[] = [];
  for (const pid of pids) {
    const p = t.get(pid);
    if (!p || p.state.startsWith("Z")) continue;
    const r = await sh("ps", ["-o", "command=", "-p", String(pid)]);
    if (r.out.trim()) {
      out.push({ pid, start: p.start, command: r.out.trim() });
    }
  }
  return out;
}

/** The processes of `procs` still running after up to `ms`. */
async function gone(procs: { pid: number; start: string }[], ms: number) {
  const until = Date.now() + ms;
  for (;;) {
    const t = await processTable();
    const left = procs.filter(({ pid, start }) =>
      t.get(pid)?.start === start && !t.get(pid)!.state.startsWith("Z")
    ).map(({ pid }) => pid);
    if (!left.length || Date.now() > until) return left;
    await sleep(100);
  }
}

export async function run(env: Env, rep: AreaReport) {
  const scheme = `dnxexit${env.nonce}`;
  const origin = `${scheme}://app`;
  const identifier = `dev.denext.e2e${env.nonce}.exit`;
  const app = await packageApp(env, {
    app: "exit",
    name: "E2EExit",
    identifier,
    appJson: { origin },
    launch: { appId: identifier, customSchemes: [scheme] },
  });
  await clearResults("exit");
  let previous: string | null = null;
  let n = 0;
  const steps = [
    ...STEPS.map((s) => ({ ...s, quick: false })),
    ...Array.from({ length: QUICK }, () => ({
      how: "quit",
      code: 0,
      quick: true,
    })),
  ];
  const pending: Awaited<ReturnType<typeof launch>>[] = [];
  for (const step of steps) {
    const label = `#${++n} ${step.how}(${step.code})`;
    await writeParams("exit", { how: step.how, code: step.code });
    const seen = await seenResults("exit");
    const token = `${env.nonce}-exit-${n}`;
    const l = await launch(env, app.exe, [], {
      env: { DENEXT_E2E_LAUNCH: token },
    });
    const r = await waitResult("exit", { seen, launch: token, ms: 90_000 });
    if (!r) {
      rep.check(
        `${label}: the app wrote a result`,
        false,
        await tail(l.logFile),
      );
      await kill(l, app.artifact);
      return;
    }
    const st = await waitExit(l, 30_000);
    const exitedAt = Date.now();
    const final = (await results("exit")).find((x) => x.pid === r.pid) ?? r;
    rep.merge(label, final, l.logFile);
    rep.check(
      `${label}: the process ended`,
      st !== null,
      st ?? (await tail(l.logFile)),
    );
    if (st === null) {
      // Ended by the runner instead; on Windows it may not be killable.
      await kill(l, app.artifact);
      return;
    }
    rep.check(`${label}: exit code ${step.code}`, st.code === step.code, st);
    if (env.backend === "cef") {
      const ending = Date.parse(final.data.ending ?? "");
      const quitMs = exitedAt - ending;
      rep.check(
        `${label}: the process was gone within ${QUIT_BUDGET_MS} ms`,
        quitMs <= QUIT_BUDGET_MS,
        { quitMs, ending: final.data.ending ?? null },
      );
      const hostLog = await Deno.readTextFile(l.logFile).catch(() => "");
      rep.check(
        `${label}: no helper outlived the shutdown`,
        !HELPER_LEFT.test(hostLog),
        hostLog.split("\n").filter((s) => HELPER_LEFT.test(s)),
      );
      if (OS === "windows") {
        rep.na(
          `${label}: nothing it started outlived it`,
          "the runner reads the process table with ps (macOS, Linux)",
        );
      } else {
        const left = await survivors(l, app.artifact);
        rep.check(
          `${label}: nothing it started outlived it`,
          left.every((p) => AFTER_HOST.test(p.command)),
          { left },
        );
        const late = await gone(left, 3_000);
        rep.check(
          `${label}: what it started ended within 3 s of it`,
          !late.length,
          { left, late },
        );
      }
    }
    if (step.how !== "quit") {
      rep.check(
        `${label}: unload ran before the end`,
        final.data.unloadRan === true,
        final.data,
      );
    }
    const page = final.data.page;
    if (previous !== null) {
      rep.check(
        `${label}: localStorage kept the previous launch's value`,
        page?.localPrevious === previous,
        { want: previous, got: page?.localPrevious },
      );
      rep.check(
        `${label}: IndexedDB kept the previous launch's value`,
        page?.idbPrevious === previous,
        { want: previous, got: page?.idbPrevious },
      );
    }
    previous = page?.value ?? null;
    if (step.quick) {
      // Relaunch at once; what this launch started is ended at the end.
      pending.push(l);
    } else {
      await kill(l, app.artifact);
    }
  }
  for (const l of pending) await kill(l, app.artifact);
}
