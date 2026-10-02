// Copyright 2018-2026 the Deno authors. MIT license.
// The app origin + memory transport: a Deno.serve app and a node:http app;
// each makes its own checks (see apps/origin/main.ts, apps/originnode/main.ts).

import {
  type AreaReport,
  clearResults,
  type Env,
  launchAndCollect,
  packageApp,
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

  // The same transport with a node:http server as the app's server.
  const nodeScheme = `dnxoriginnode${env.nonce}`;
  const nodeOrigin = `${nodeScheme}://app`;
  const nodeId = `dev.denext.e2e${env.nonce}.originnode`;
  const pn = await packageApp(env, {
    app: "originnode",
    name: "E2EOriginNode",
    identifier: nodeId,
    appJson: { origin: nodeOrigin },
    launch: { appId: nodeId, customSchemes: [nodeScheme] },
  });
  await writeParams("origin", { origin: nodeOrigin });
  await launchAndCollect(env, rep, "node:http", pn);
}
