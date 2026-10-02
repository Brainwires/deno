// Copyright 2018-2026 the Deno authors. MIT license.
// Passkeys error paths: one launch; the app makes the checks
// (apps/passkeys/main.ts).

import {
  type AreaReport,
  clearResults,
  type Env,
  launchAndCollect,
  packageApp,
} from "../lib/runner.ts";

export async function run(env: Env, rep: AreaReport) {
  const p = await packageApp(env, {
    app: "passkeys",
    name: "E2EPasskeys",
    identifier: `dev.denext.e2e${env.nonce}.passkeys`,
    include: ["worker.ts"],
  });
  await clearResults("passkeys");
  await launchAndCollect(env, rep, "launch", p, { ms: 240_000 });
}
