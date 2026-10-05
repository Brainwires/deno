// Copyright 2018-2026 the Deno authors. MIT license.
// e2e: requests that carry cookies complete, whatever the session's Secret
// Service can do (laufey API 45).
//
// CEF encrypts its cookie store with a key it keeps in the Secret Service.
// With a locked keyring and no one to answer the unlock prompt (a headless
// session: e2e.sh E2E_SECRET_SERVICE=locked), it used to wait for that key
// forever, and every navigation, fetch and WebSocket handshake that carries
// a cookie waited with it. The page here sets a cookie, navigates (the
// navigation carries it), fetches with it and opens a WebSocket with it, on
// a loopback http origin (the app's custom scheme keeps no cookies); the
// app reports what each request carried. A stall leaves no result, which
// the runner reports as a timeout.

import {
  BrowserWindow,
  desktop,
  html,
  page,
  Report,
  sleep,
  waitFor,
} from "../_shared/e2e.ts";

const r = new Report("keyring");
const want = r.params as {
  secretService?: string;
  cookieEncryption?: string | null;
};

// Optional: a runtime older than laufey API 45 has none (the cookie checks
// still run, so the stall it had shows).
const features = desktop.platformFeatures?.() ?? null;
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

const SECOND = `
const out = { cookie: document.cookie };
const t0 = performance.now();
const res = await fetch("/echo", { credentials: "same-origin" });
out.fetchCookie = await res.text();
out.socket = await new Promise((resolve) => {
  const timer = setTimeout(() => resolve("timeout"), 30000);
  const ws = new WebSocket(location.origin.replace(/^http/, "ws") + "/ws");
  ws.onmessage = (e) => { clearTimeout(timer); resolve(String(e.data)); ws.close(); };
  ws.onerror = () => { clearTimeout(timer); resolve("error"); };
});
out.ms = performance.now() - t0;
await fetch("/result", { method: "POST", body: JSON.stringify(out) });
`;

// The app's own page (its custom-scheme origin keeps no cookies). It says
// when it loaded: navigating the window before that would be overtaken by it.
let appPageLoaded = false;
Deno.serve((req) => {
  if (new URL(req.url).pathname === "/loaded") {
    appPageLoaded = true;
    return new Response("ok");
  }
  return html(page("e2e keyring", "", `fetch("/loaded");`));
});

// A loopback http origin, which does: the second window's page.
const server = Deno.serve({ hostname: "127.0.0.1", port: 0 }, async (req) => {
  const url = new URL(req.url);
  const cookie = req.headers.get("cookie") ?? "";
  if (url.pathname === "/ws") {
    const { socket, response } = Deno.upgradeWebSocket(req);
    socket.onopen = () => socket.send(cookie);
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
    r.check(
      "a WebSocket handshake carrying a cookie completes",
      String(body.socket).includes("e2e=1"),
      body,
    );
    r.done();
    setTimeout(async () => {
      await sleep(500);
      // Close the loopback window and its server first: the app then ends
      // the way a user's quit does.
      win.close();
      await server.shutdown();
      desktop.quit();
      await sleep(10000);
      Deno.exit(0);
    }, 0);
    return new Response("ok");
  }
  // The first page sets the cookie, then navigates.
  const res = html(page("e2e keyring", "", `location.href = "/second";`));
  res.headers.set("set-cookie", "e2e=1; Path=/; SameSite=Lax");
  return res;
});

// The main window (adopted), once the app's page is in it.
const win = new BrowserWindow();
r.set("appPageLoaded", await waitFor(() => appPageLoaded, 60_000));
win.navigate(`http://127.0.0.1:${server.addr.port}/`);
