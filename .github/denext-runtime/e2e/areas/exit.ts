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
  results,
  seenResults,
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
