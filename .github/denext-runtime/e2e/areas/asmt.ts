// Copyright 2018-2026 the Deno authors. MIT license.
// Auth sessions and native code on the UI thread (laufey API 42): one
// launch; the app makes the checks (apps/asmt/main.ts).

import {
  type AreaReport,
  clearResults,
  type Env,
  launchAndCollect,
  packageApp,
} from "../lib/runner.ts";

export async function run(env: Env, rep: AreaReport) {
  const p = await packageApp(env, {
    app: "asmt",
    name: "E2EAsmt",
    identifier: `dev.denext.e2e${env.nonce}.asmt`,
    include: ["worker.ts"],
  });
  await clearResults("asmt");
  await launchAndCollect(env, rep, "launch", p, { ms: 240_000 });
}
