// Copyright 2018-2026 the Deno authors. MIT license.
// e2e: how an app ends. Each launch reads what the previous launch stored in
// localStorage and IndexedDB, stores its own value, reports both, and then
// ends at once (no pause for the engine to flush) the way the runner's params
// say: `Deno.exit(code)`, node's `process.exit(code)`, or `desktop.quit()`.
// The runner checks the exit code, that the process ended, and that the next
// launch reads the value back: the engine wrote its profile before the
// process ended.

import process from "node:process";
import { desktop, html, page, Report } from "../_shared/e2e.ts";

const r = new Report("exit");
const how: string = r.params.how ?? "deno-exit";
const code: number = r.params.code ?? 0;

// Deno.exit() dispatches `unload` before the process ends.
globalThis.addEventListener("unload", () => {
  r.set("unloadRan", true);
});

const SCRIPT = `
const value = "launch-" + Date.now() + "-" + Math.random().toString(36).slice(2);
const out = { value, origin: location.origin };
out.localPrevious = localStorage.getItem("e2e-exit");
localStorage.setItem("e2e-exit", value);
out.localReadBack = localStorage.getItem("e2e-exit");
const idb = (mode, f) => new Promise((resolve, reject) => {
  const open = indexedDB.open("e2e-exit", 1);
  open.onupgradeneeded = () => open.result.createObjectStore("kv");
  open.onerror = () => reject(open.error);
  open.onsuccess = () => {
    const tx = open.result.transaction("kv", mode);
    const req = f(tx.objectStore("kv"));
    tx.oncomplete = () => { open.result.close(); resolve(req.result ?? null); };
    tx.onerror = () => reject(tx.error);
  };
});
try {
  out.idbPrevious = await idb("readonly", (s) => s.get("e2e"));
  await idb("readwrite", (s) => s.put(value, "e2e"));
  out.idbReadBack = await idb("readonly", (s) => s.get("e2e"));
} catch (e) { out.idbError = String(e); }
await fetch("/result", { method: "POST", body: JSON.stringify(out) });
`;

Deno.serve(async (req) => {
  const url = new URL(req.url);
  if (url.pathname === "/result") {
    const body = JSON.parse(await req.text());
    r.set("page", body);
    r.set("how", how);
    r.check("localStorage reads back", body.localReadBack === body.value, body);
    r.check("IndexedDB reads back", body.idbReadBack === body.value, body);
    r.done();
    // End at once: what the page stored must survive without a pause.
    setTimeout(() => {
      r.set("ending", new Date().toISOString());
      if (how === "quit") desktop.quit();
      else if (how === "process-exit") process.exit(code);
      else Deno.exit(code);
    }, 0);
    return new Response("ok");
  }
  return html(page("e2e exit", "", SCRIPT));
});
