// Copyright 2018-2026 the Deno authors. MIT license.
// e2e: a profile's cookies encrypted with the OS key survive a launch that
// can't reach the key (laufey API 45; areas/keyringpersist.ts drives the
// keyring between launches).
//
// Each launch reads what to do from its params: optionally set a
// persistent cookie on a loopback http origin, then navigate there again
// and report which cookies the navigation carried; or (`stall`) navigate
// and report, without waiting for an answer, whether the request reached
// the server while the cookie store waits for the OS key. Every launch
// checks platformFeatures' cookie-store fields.

import {
  BrowserWindow,
  desktop,
  html,
  page,
  Report,
  sleep,
  waitFor,
} from "../_shared/e2e.ts";

const r = new Report("keyringpersist");
const want = r.params as {
  step: string;
  set?: string;
  expect?: string[];
  stall?: boolean;
  cookieEncryption?: string;
  wait?: boolean;
};
r.set("step", want.step);

const features = (await desktop.platformFeatures?.()) ?? null;
r.set("platformFeatures", features);
if (want.cookieEncryption !== undefined) {
  r.check(
    `cookieEncryption is ${want.cookieEncryption}`,
    features?.cookieEncryption === want.cookieEncryption,
    features?.cookieEncryption,
  );
}
if (want.wait !== undefined) {
  const waiting = typeof features?.cookieEncryptionWait === "string" &&
    features.cookieEncryptionWait.length > 0;
  r.check(
    want.wait
      ? "cookieEncryptionWait says why the cookie store waits for the OS key"
      : "cookieEncryptionWait is null",
    waiting === want.wait,
    features?.cookieEncryptionWait,
  );
}

async function finish() {
  r.done();
  await sleep(500);
  // The loopback window and its server first: the app then ends the way a
  // user's quit does (Chromium writes its cookies out on the way).
  win.close();
  await server.shutdown().catch(() => {});
  desktop.quit();
  await sleep(10000);
  Deno.exit(0);
}

// The app's own page (its custom-scheme origin keeps no cookies).
let appPageLoaded = false;
Deno.serve((req) => {
  if (new URL(req.url).pathname === "/loaded") {
    appPageLoaded = true;
    return new Response("ok");
  }
  return html(page("e2e keyringpersist", "", `fetch("/loaded");`));
});

let reached = false;
const server = Deno.serve({ hostname: "127.0.0.1", port: 0 }, (req) => {
  const url = new URL(req.url);
  const cookie = req.headers.get("cookie") ?? "";
  reached = true;
  if (url.pathname === "/check") {
    r.set("cookie", cookie);
    const names = cookie.split(/;\s*/).map((c) => c.split("=")[0]);
    for (const name of want.expect ?? []) {
      r.check(
        `the navigation carries the cookie ${name}`,
        names.includes(name),
        cookie,
      );
    }
    setTimeout(finish, 0);
    return html(page("e2e keyringpersist check", "", ""));
  }
  // The first page sets the cookie (persistent: it outlives the launch),
  // then navigates.
  const res = html(page("e2e keyringpersist", "", `location.href = "/check";`));
  if (want.set) {
    res.headers.set(
      "set-cookie",
      `${want.set}=1; Path=/; Max-Age=86400; SameSite=Lax`,
    );
  }
  return res;
});

const win = new BrowserWindow();
r.set("appPageLoaded", await waitFor(() => appPageLoaded, 60_000));
win.navigate(`http://127.0.0.1:${server.addr.port}/`);
if (want.stall) {
  // The cookie store waits for the key: the navigation (it would carry
  // cookies) is held. Recorded, not required: what matters is that the
  // cookies outlive this launch.
  await sleep(8000);
  r.set("requestReachedServer", reached);
  r.done();
  Deno.exit(0);
}
