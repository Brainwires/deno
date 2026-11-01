// Copyright 2018-2026 the Deno authors. MIT license.
// Drag and drop, file dialogs and the rich clipboard: one launch; the app
// makes the checks (apps/dnd/main.ts).

import {
  type AreaReport,
  clearResults,
  type Env,
  launchAndCollect,
  packageApp,
  writeParams,
} from "../lib/runner.ts";

export async function run(env: Env, rep: AreaReport) {
  const p = await packageApp(env, {
    app: "dnd",
    name: "E2EDnd",
    identifier: `dev.denext.e2e${env.nonce}.dnd`,
  });
  await clearResults("dnd");
  await writeParams("dnd", {
    // Under CEF, laufey's GTK connection never has keyboard focus (Chromium's
    // connection owns the window), so it sees the clipboard only through
    // ext-data-control-v1, which wlroots has from 0.19 and this sway doesn't.
    clipboardChangeRefused:
      Deno.env.get("E2E_DISPLAY") === "wayland" && env.backend === "cef"
        ? "CEF on Wayland needs ext-data-control-v1 for these, and the runner's sway (wlroots 0.15) doesn't have it"
        : undefined,
  });
  await launchAndCollect(env, rep, "launch", p, { ms: 180_000 });
}
