// Copyright 2018-2026 the Deno authors. MIT license.
// The OS's quits reach the app (laufey API 52): on macOS a quit Apple Event,
// what the Dock's Quit sends, and one with a logout as its reason, as
// loginwindow sends. The Windows side of laufey is covered by its
// --quit-request native e2e, and the event path here is the same.

import {
  type AreaReport,
  clearResults,
  type Env,
  kill,
  launch,
  OS,
  packageApp,
  results,
  seenResults,
  sh,
  sleep,
  tail,
  waitExit,
  waitResult,
} from "../lib/runner.ts";

/** Send `pid` a quit Apple Event; `logout` adds kAEQuitReason = kAELogOut. */
async function quitEvent(pid: number, logout: boolean) {
  const reason = logout
    ? `ev.setAttributeDescriptorForKeyword(
         $.NSAppleEventDescriptor.descriptorWithEnumCode(0x6c6f676f), 0x7768793f);`
    : "";
  await sh("osascript", [
    "-l",
    "JavaScript",
    "-e",
    `
    ObjC.import('Foundation');
    const target = $.NSAppleEventDescriptor.descriptorWithProcessIdentifier(${pid});
    const ev = $.NSAppleEventDescriptor
      .appleEventWithEventClassEventIDTargetDescriptorReturnIDTransactionID(
        0x61657674, 0x71756974, target, -1, 0);
    ${reason}
    ev.sendEventWithOptionsTimeoutError(0x1, 5, null);`,
  ]);
}

export async function run(env: Env, rep: AreaReport) {
  if (OS !== "darwin") {
    rep.na("the OS's quits", "macOS here; laufey --quit-request on Windows");
    return;
  }
  const scheme = `dnxquit${env.nonce}`;
  const identifier = `dev.denext.e2e${env.nonce}.quit`;
  const app = await packageApp(env, {
    app: "quit",
    name: "E2EQuit",
    identifier,
    appJson: { origin: `${scheme}://app` },
    launch: { appId: identifier, customSchemes: [scheme] },
  });
  await clearResults("quit");
  const seen = await seenResults("quit");
  const token = `${env.nonce}-quit`;
  const l = await launch(env, app.exe, [], {
    env: { DENEXT_E2E_LAUNCH: token },
  });
  const first = await waitResult("quit", { seen, launch: token, ms: 90_000 });
  if (!first) {
    rep.check("the app wrote a result", false, await tail(l.logFile));
    await kill(l, app.artifact);
    return;
  }
  const data = async () =>
    (await results("quit")).find((x) => x.pid === first.pid)?.data ?? {};
  const until = async (
    f: (d: { reasons?: string[]; cancelQuit?: boolean }) => boolean,
  ) => {
    for (let i = 0; i < 100; i++) {
      if (f(await data())) return true;
      await sleep(100);
    }
    return false;
  };

  await sleep(1000);
  await quitEvent(first.pid, false);
  rep.check(
    "a quit Apple Event reaches beforequit, as a plain quit",
    await until((d) => d.reasons?.[0] === "quit"),
    await data(),
  );
  await sleep(500);
  rep.check(
    "a refusal keeps the app running",
    (await waitExit(l, 500)) === null,
  );

  await quitEvent(first.pid, true);
  rep.check(
    "a logout reaches beforequit as a session end",
    await until((d) => d.reasons?.[1] === "session-end"),
    await data(),
  );
  rep.check(
    "cancelQuit() refuses the held session end",
    await until((d) => d.cancelQuit === true),
    await data(),
  );
  rep.check("the app is still running", (await waitExit(l, 500)) === null);

  // Held and never answered: laufey's 10 s deadline ends it, whatever the app
  // does. (5 s of grace for the runner.)
  const held = Date.now();
  await quitEvent(first.pid, true);
  rep.check(
    "a third session end reaches beforequit",
    await until((d) => d.reasons?.[2] === "session-end"),
    await data(),
  );
  const st = await waitExit(l, 15_000);
  rep.check(
    "a held session end ends the app at the deadline",
    st !== null && Date.now() - held >= 9_000,
    { ms: Date.now() - held, st },
  );
  if (st === null) await kill(l, app.artifact);
  else rep.check("with exit code 0", st.code === 0, st);
}
