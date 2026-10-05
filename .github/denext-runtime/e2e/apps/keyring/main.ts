// Copyright 2018-2026 the Deno authors. MIT license.
// e2e: requests that carry cookies complete, whatever the session's Secret
// Service can do (laufey API 45).
//
// CEF encrypts its cookie store with a key it keeps in the Secret Service.
// With a locked keyring and no one to answer the unlock prompt (a headless
// session: e2e.sh E2E_SECRET_SERVICE=locked), it used to wait for that key
// forever, and every navigation, fetch and WebSocket handshake that carries
// a cookie waited with it. The page here sets a cookie, navigates (the
// navigation carries it), fetches with it, and opens a WebSocket through the
// runtime's relay (the handshake goes through the same cookie store); the
// app reports what each request carried. A stall leaves no result, which
// the runner reports as a timeout.

import { desktop, html, page, Report, sleep } from "../_shared/e2e.ts";

const r = new Report("keyring");
const want = r.params as {
  secretService?: string;
  cookieEncryption?: string | null;
};

const features = desktop.platformFeatures();
r.set("platformFeatures", features);
if (want.secretService !== undefined) {
  r.check(
    `secretService is ${want.secretService}`,
    features?.secretService === want.secretService,
    features?.secretService,
  );
}
if (want.cookieEncryption !== undefined) {
  r.check(
    `cookieEncryption is ${want.cookieEncryption}`,
    features?.cookieEncryption === want.cookieEncryption,
    features?.cookieEncryption,
  );
}

// The relay URL with this launch's token (what a page's WebSocket dials).
const wsUrl = Deno.env.get("DENO_DESKTOP_WS_URL") ?? null;

const SECOND = `
const out = { cookie: document.cookie };
const t0 = performance.now();
const res = await fetch("/echo", { credentials: "same-origin" });
out.fetchCookie = await res.text();
out.socket = await new Promise((resolve) => {
  const timer = setTimeout(() => resolve("timeout"), 30000);
  let ws;
  try { ws = new WebSocket(${JSON.stringify(wsUrl)} + "/ws"); }
  catch (e) { clearTimeout(timer); resolve(String(e)); return; }
  ws.onmessage = (e) => { clearTimeout(timer); resolve(String(e.data)); ws.close(); };
  ws.onerror = () => { clearTimeout(timer); resolve("error"); };
});
out.ms = performance.now() - t0;
await fetch("/result", { method: "POST", body: JSON.stringify(out) });
`;

Deno.serve(async (req) => {
  const url = new URL(req.url);
  const cookie = req.headers.get("cookie") ?? "";
  if (url.pathname.endsWith("/ws")) {
    const { socket, response } = Deno.upgradeWebSocket(req);
    socket.onopen = () => socket.send("open");
    return response;
  }
  if (url.pathname === "/echo") return new Response(cookie);
  if (url.pathname === "/second") {
    r.set("navigationCookie", cookie);
    r.check(
      "a navigation carrying a cookie completes",
      cookie.includes("e2e=1"),
      cookie,
    );
    return html(page("e2e keyring 2", "", SECOND));
  }
  if (url.pathname === "/result") {
    const body = JSON.parse(await req.text());
    r.set("page", body);
    r.check(
      "a fetch carrying a cookie completes",
      String(body.fetchCookie).includes("e2e=1"),
      body,
    );
    if (wsUrl) {
      r.check(
        "a WebSocket handshake through the relay completes",
        body.socket === "open",
        body,
      );
    } else {
      r.na("a WebSocket through the relay", "no DENO_DESKTOP_WS_URL");
    }
    r.done();
    setTimeout(async () => {
      await sleep(500);
      desktop.quit();
      await sleep(10000);
      Deno.exit(0);
    }, 0);
    return new Response("ok");
  }
  // The first page sets the cookie, then navigates.
  const res = html(
    page("e2e keyring", "", `location.href = "/second";`),
  );
  res.headers.set("set-cookie", "e2e=1; Path=/; SameSite=Lax");
  return res;
});
