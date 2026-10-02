// Copyright 2018-2026 the Deno authors. MIT license.
// The window API: one launch; the app makes the checks (apps/window/main.ts)
// and must end itself with Deno.desktop.quit().

import {
  type AreaReport,
  clearResults,
  type Env,
  launchAndCollect,
  packageApp,
  writeParams,
} from "../lib/runner.ts";

export async function run(env: Env, rep: AreaReport) {
  const initialWindow = { width: 720, height: 480 };
  const p = await packageApp(env, {
    app: "window",
    name: "E2EWindow",
    identifier: `dev.denext.e2e${env.nonce}.window`,
    appJson: { initialWindow },
  });
  await clearResults("window");
  await writeParams("window", { initialWindow });
  await launchAndCollect(env, rep, "launch", p, { ms: 240_000 });
}
