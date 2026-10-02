// Copyright 2018-2026 the Deno authors. MIT license.
// e2e: the app identifier and the per-app web data directory.
//
// Packaged twice (identifiers A and B, the same origin). Each launch reads
// what the previous launch of the SAME app stored in localStorage and
// IndexedDB, stores its own value, and reports both; the runner checks that
// A's second launch sees A's first and that B never sees A's.

import { desktop, html, page, Report, sleep } from "../_shared/e2e.ts";

const r = new Report("appid");

const SCRIPT = `
const value = "launch-" + Date.now() + "-" + Math.random().toString(36).slice(2);
const out = { value, origin: location.origin };
out.localPrevious = localStorage.getItem("e2e");
localStorage.setItem("e2e", value);
out.localReadBack = localStorage.getItem("e2e");
const idb = (mode, f) => new Promise((resolve, reject) => {
  const open = indexedDB.open("e2e", 1);
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
    r.set("laufeyAppId", Deno.env.get("LAUFEY_APP_ID") ?? null);
    r.check("localStorage reads back", body.localReadBack === body.value, body);
    r.check("IndexedDB reads back", body.idbReadBack === body.value, body);
    r.check(
      "LAUFEY_APP_ID is the identifier",
      Deno.env.get("LAUFEY_APP_ID") === r.params.identifier,
      Deno.env.get("LAUFEY_APP_ID"),
    );
    r.done();
    // Let the engine flush its storage, then quit the way a user does (the
    // window closes, the engine shuts down) rather than killing the process.
    setTimeout(async () => {
      await sleep(1500);
      desktop.quit();
      await sleep(10000);
      Deno.exit(0);
    }, 0);
    return new Response("ok");
  }
  return html(page("e2e appid", "", SCRIPT));
});
