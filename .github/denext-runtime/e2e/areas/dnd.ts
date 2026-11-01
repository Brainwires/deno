// Copyright 2018-2026 the Deno authors. MIT license.
// Drag and drop, file dialogs and the rich clipboard: one launch; the app
// makes the checks (apps/dnd/main.ts).

import {
  type AreaReport,
  clearResults,
  type Env,
  launchAndCollect,
  packageApp,
} from "../lib/runner.ts";

export async function run(env: Env, rep: AreaReport) {
  const p = await packageApp(env, {
    app: "dnd",
    name: "E2EDnd",
    identifier: `dev.denext.e2e${env.nonce}.dnd`,
  });
  await clearResults("dnd");
  await launchAndCollect(env, rep, "launch", p, { ms: 180_000 });
}
