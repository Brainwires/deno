// Copyright 2018-2026 the Deno authors. MIT license.
// How an app ends: Deno.exit(code), process.exit(code) and desktop.quit()
// each end the process (no hang) with that exit code, `unload` runs before
// Deno.exit(), and what the page stored just before the end (localStorage,
// IndexedDB) survives into the next launch. An exit from the runtime's thread
// used to end the process under a running engine on every OS (the profile
// unwritten; on Windows it could also hang in DLL detach code); it now goes
// through laufey's exit_app (rt_desktop `desktop_exit`) on Windows, macOS and
// Linux, with both backends.

import {
  type AreaReport,
  clearResults,
  type Env,
  kill,
  launch,
  packageApp,
  resultKey,
  results,
  seenResults,
  sh,
  tail,
  waitExit,
  waitResult,
  writeParams,
} from "../lib/runner.ts";

// Each launch ends one way; the next one reads what it stored.
const BASE = [
  { how: "deno-exit", code: 3 },
  { how: "process-exit", code: 4 },
  { how: "quit", code: 0 },
  { how: "deno-exit", code: 0 },
  { how: "deno-exit", code: 5 },
];
// ci-exp: many rounds, to catch the rare launch whose page never reports.
const STEPS = Array.from({ length: 12 }, () => BASE).flat();

async function webview2Procs(identifier: string): Promise<string> {
  if (Deno.build.os !== "windows") return "";
  const ps =
    "Get-CimInstance Win32_Process -Filter \"Name='msedgewebview2.exe'\" | " +
    "Where-Object { $_.CommandLine -like '*" + identifier + "*' } | " +
    'ForEach-Object { "$($_.ProcessId) parent=$($_.ParentProcessId) $($_.CreationDate) " + ' +
    "($_.CommandLine -replace '^.*--type=([a-z-]+).*$', '$1') }";
  const r = await sh("powershell", ["-NoProfile", "-Command", ps]);
  return r.out.trim();
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
  for (const step of STEPS) {
    const label = `#${++n} ${step.how}(${step.code})`;
    await writeParams("exit", step);
    const seen = await seenResults("exit");
    const before = await webview2Procs(identifier);
    if (before) {
      console.log(`${label}: WebView2 processes alive at launch:\n${before}`);
    }
    const token = `${env.nonce}-exit-${n}`;
    const l = await launch(env, app.exe, [], {
      env: { DENEXT_E2E_LAUNCH: token },
    });
    const r = await waitResult("exit", { seen, launch: token, ms: 90_000 });
    if (!r) {
      const partial = (await results("exit")).filter((x) =>
        !seen.has(resultKey(x))
      );
      console.log(
        `${label}: no result; partial: ${
          JSON.stringify(
            partial.map((x) => ({
              pid: x.pid,
              launch: x.launch,
              trace: x.data.trace,
            })),
          )
        }`,
      );
      console.log(
        `${label}: WebView2 processes now:\n${await webview2Procs(identifier)}`,
      );
      rep.check(
        `${label}: the app wrote a result`,
        false,
        await tail(l.logFile),
      );
      await kill(l, app.artifact);
      return;
    }
    const st = await waitExit(l, 30_000);
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
    await kill(l, app.artifact);
  }
}
