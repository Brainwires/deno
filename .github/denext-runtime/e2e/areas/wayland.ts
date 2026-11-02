// Copyright 2018-2026 the Deno authors. MIT license.
// E2E_DISPLAY=wayland only (run.ts): what the compositor sees of a window, and
// what it can make the window do (apps/wayland/main.ts).

import {
  type AreaReport,
  clearResults,
  type Env,
  launchAndCollect,
  packageApp,
  writeParams,
} from "../lib/runner.ts";

export async function run(env: Env, rep: AreaReport) {
  const identifier = `dev.denext.e2e${env.nonce}.wayland`;
  const p = await packageApp(env, {
    app: "wayland",
    name: "E2EWayland",
    identifier,
    // What denext's packaging writes: the id a stock package has none of.
    launch: { appId: identifier },
  });
  await clearResults("wayland");
  await writeParams("wayland", { identifier });
  await launchAndCollect(env, rep, "launch", p, { ms: 120_000 });
}
