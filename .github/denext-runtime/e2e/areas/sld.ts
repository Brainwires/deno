// Copyright 2018-2026 the Deno authors. MIT license.
// Global shortcuts, launch at login and DevTools control. The package ships
// `"inspectable": false` in laufey-launch.json (what denext writes for a
// release build); the first launch turns DevTools on with the
// LAUFEY_INSPECTABLE=1 override, the second runs with the file's setting.

import {
  type AreaReport,
  clearResults,
  type Env,
  launchAndCollect,
  packageApp,
  writeParams,
} from "../lib/runner.ts";

export async function run(env: Env, rep: AreaReport) {
  const identifier = `dev.denext.e2e${env.nonce}.sld`;
  const p = await packageApp(env, {
    app: "sld",
    name: "E2ESld",
    identifier,
    launch: { appId: identifier, inspectable: false },
  });
  await clearResults("sld");
  await writeParams("sld", { identifier, devtools: true });
  await launchAndCollect(env, rep, "DevTools on (LAUFEY_INSPECTABLE=1)", p, {
    env: { LAUFEY_INSPECTABLE: "1" },
    ms: 180_000,
  });
  await writeParams("sld", { identifier, devtools: false });
  await launchAndCollect(env, rep, "DevTools off (inspectable: false)", p, {
    ms: 180_000,
  });
}
