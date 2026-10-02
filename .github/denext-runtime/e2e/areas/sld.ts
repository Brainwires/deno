// Copyright 2018-2026 the Deno authors. MIT license.
// Global shortcuts, launch at login and DevTools control. The package ships
// `"inspectable": false` in laufey-launch.json (what denext writes for a
// release build). The first launch sets LAUFEY_INSPECTABLE=1, which a
// shipped "off" must ignore (an inherited or injected environment can't turn
// DevTools back on); the second runs with the file's setting alone; the
// third runs after the launch file is rewritten with `"inspectable": true`.

import {
  adhocSign,
  type AreaReport,
  clearResults,
  type Env,
  laufeyLaunchPath,
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
  await writeParams("sld", { identifier, devtools: false });
  await launchAndCollect(
    env,
    rep,
    "DevTools stay off (LAUFEY_INSPECTABLE=1 vs a shipped inspectable: false)",
    p,
    { env: { LAUFEY_INSPECTABLE: "1" }, ms: 180_000 },
  );
  await launchAndCollect(env, rep, "DevTools off (inspectable: false)", p, {
    ms: 180_000,
  });
  await Deno.writeTextFile(
    laufeyLaunchPath(p.artifact),
    JSON.stringify({ appId: identifier, inspectable: true }, null, 2) + "\n",
  );
  await adhocSign(p.artifact);
  await writeParams("sld", { identifier, devtools: true });
  await launchAndCollect(env, rep, "DevTools on (inspectable: true)", p, {
    ms: 180_000,
  });
}
