// Copyright 2018-2026 the Deno authors. MIT license.
// The app origin + memory transport: one launch; the app makes every check
// (see apps/origin/main.ts).

import {
  type AreaReport,
  clearResults,
  type Env,
  kill,
  launch,
  launchAndCollect,
  packageApp,
  seenPids,
  waitResult,
  writeParams,
} from "../lib/runner.ts";

export async function run(env: Env, rep: AreaReport) {
  const scheme = `dnxorigin${env.nonce}`;
  const origin = `${scheme}://app`;
  const identifier = `dev.denext.e2e${env.nonce}.origin`;
  const p = await packageApp(env, {
    app: "origin",
    name: "E2EOrigin",
    identifier,
    appJson: { origin },
    launch: { appId: identifier, customSchemes: [scheme] },
  });
  await clearResults("origin");
  await writeParams("origin", { origin });
  await launchAndCollect(env, rep, "launch", p);

  if (env.backend === "cef") {
    // Diagnostic, not a check: the same app with Chromium's Local Network
    // Access checks off (CEF takes switches from the command line), to tell
    // whether they are what stops the page reaching loopback.
    const seen = await seenPids("origin");
    const l = await launch(env, p.exe, [
      "--disable-features=LocalNetworkAccessChecks,LocalNetworkAccessChecksWebSockets",
    ]);
    const r = await waitResult("origin", { seen, ms: 90_000 });
    rep.note(
      `with --disable-features=LocalNetworkAccessChecks: ${
        r
          ? JSON.stringify({
            ws: r.data.page?.ws,
            crossOrigin: r.data.page?.crossOrigin,
          })
          : "no result"
      }`,
    );
    await kill(l, p.artifact);
  }
}
