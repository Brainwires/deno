// Copyright 2018-2026 the Deno authors. MIT license.
// Menus and notifications: one launch where the app makes the checks
// (apps/menus/main.ts); on Windows also the cold-start click: a click on a
// toast while the app isn't running makes COM start the app, and the click
// arrives in Deno.desktop.launchNotificationResponses.

import {
  type AreaReport,
  clearResults,
  type Env,
  HERE,
  killByPath,
  launchAndCollect,
  OS,
  packageApp,
  seenPids,
  sh,
  waitResult,
  writeParams,
} from "../lib/runner.ts";

export async function run(env: Env, rep: AreaReport) {
  const identifier = `dev.denext.e2e${env.nonce}.menus`;
  const p = await packageApp(env, {
    app: "menus",
    name: "E2EMenus",
    identifier,
    launch: { appId: identifier },
  });
  const toastClick = OS === "windows"
    ? decodeURIComponent(new URL("windows/toast-click.ps1", HERE).pathname)
      .replace(/^\/([A-Za-z]:)/, "$1").replace(/\//g, "\\")
    : "";
  await clearResults("menus");
  await writeParams("menus", { identifier, toastClick, mode: "main" });
  await launchAndCollect(env, rep, "launch", p, { ms: 240_000 });

  if (OS !== "windows") {
    rep.na(
      "a click on a notification while the app is not running launches it",
      OS === "linux"
        ? "freedesktop notification servers send a click only to the process that posted it (capabilities().coldStart is false on Linux)"
        : "macOS delivers a click only from a person clicking the banner",
    );
    return;
  }
  await writeParams("menus", { identifier, toastClick, mode: "cold" });
  const seen = await seenPids("menus");
  const c = await sh("powershell", [
    "-NoProfile",
    "-ExecutionPolicy",
    "Bypass",
    "-File",
    toastClick,
    "-Aumid",
    identifier,
    "-Tag",
    "e2e-cold",
    "-Action",
    "cold-action",
    "-Data",
    '{"c":1}',
  ], { timeoutMs: 120_000 });
  rep.check(
    "cold-start toast click: COM activation succeeded",
    c.code === 0,
    c.out,
  );
  const r = await waitResult("menus", { seen, ms: 90_000 });
  rep.merge("cold-start toast click", r);
  await killByPath(p.artifact);
}
