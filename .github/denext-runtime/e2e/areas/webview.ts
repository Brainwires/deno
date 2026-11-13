// Copyright 2018-2026 the Deno authors. MIT license.
// Embedded web views (laufey API 58): one launch; the app makes the checks
// (apps/webview/main.ts) and must end itself with Deno.desktop.quit().

import {
  type AreaReport,
  clearResults,
  type Env,
  launchAndCollect,
  packageApp,
} from "../lib/runner.ts";

export async function run(env: Env, rep: AreaReport) {
  const p = await packageApp(env, {
    app: "webview",
    name: "E2EWebView",
    identifier: `dev.denext.e2e${env.nonce}.webview`,
  });
  await clearResults("webview");
  await launchAndCollect(env, rep, "launch", p, { ms: 240_000 });
}
