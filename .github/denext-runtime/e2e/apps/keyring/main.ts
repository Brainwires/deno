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
  secretServicePrompt?: boolean;
  sessionType?: string;
  cookieEncryption?: string | null;
  cookieEncryptionWait?: string | null;
  kwallet?: string | null;
  secureStoreReason?: string;
  secureStoreGetMayBeNull?: boolean;
  secureStoreMac?: boolean;
};

// Optional: a runtime older than laufey API 45 has none (the cookie checks
// still run, so the stall it had shows). A promise since it left the
// JavaScript thread (an earlier API 45 build answered synchronously).
const features = (await desktop.platformFeatures?.()) ?? null;
r.set("platformFeatures", features);
if (want.secretService !== undefined) {
  r.check(
    `secretService is ${want.secretService}`,
    features?.secretService === want.secretService,
    features?.secretService,
  );
}
if (want.secretServicePrompt !== undefined) {
  r.check(
    `secretServicePrompt is ${want.secretServicePrompt} (a display alone is not a person)`,
    features?.secretServicePrompt === want.secretServicePrompt,
    features?.secretServicePrompt,
  );
}
if (want.sessionType !== undefined) {
  r.check(
    `sessionType is ${want.sessionType}`,
    features?.sessionType === want.sessionType,
    features?.sessionType,
  );
}
if (want.cookieEncryption !== undefined) {
  r.check(
    `cookieEncryption is ${want.cookieEncryption}`,
    features?.cookieEncryption === want.cookieEncryption,
    features?.cookieEncryption,
  );
}

if (want.cookieEncryptionWait !== undefined) {
  r.check(
    `cookieEncryptionWait is ${want.cookieEncryptionWait}`,
    features?.cookieEncryptionWait === want.cookieEncryptionWait,
    features?.cookieEncryptionWait,
  );
}
if (want.kwallet !== undefined) {
  r.check(
    `kwallet is ${want.kwallet}`,
    features?.kwallet === want.kwallet,
    features?.kwallet,
  );
}

// The secure store (laufey API 47): under this Secret Service no write can
// succeed, and no call may wait for an unlock no one can give: each answers
// at once, a refusal with the reason (never a plaintext fallback). Off Linux (and on a runtime
// without it) the store is the OS's own, which these checks don't touch.
const store = (desktop as unknown as {
  secureStore?: {
    supported: boolean;
    get(s: string, a: string, o?: { timeout?: number }): Promise<unknown>;
    set(
      s: string,
      a: string,
      v: string,
      o?: { timeout?: number },
    ): Promise<unknown>;
  };
}).secureStore;
if (want.secureStoreReason !== undefined) {
  r.check("secureStore is supported (Linux)", store?.supported === true);
  if (!store) throw new Error("no Deno.desktop.secureStore");
  const t0 = performance.now();
  const outcome = async (p: Promise<unknown>) => {
    try {
      return { value: await p };
    } catch (e) {
      return { name: (e as Error).name, message: (e as Error).message };
    }
  };
  const got = await outcome(
    store.get("dev.denext.e2e", "keyring", { timeout: 20000 }),
  );
  const set = await outcome(
    store.set("dev.denext.e2e", "keyring", "v", { timeout: 20000 }),
  );
  const ms = Math.round(performance.now() - t0);
  r.set("secureStore", { got, set, ms });
  // A locked keyring still answers a search: a key that was never stored is
  // simply not there (null). Anything else is refused with the reason.
  r.check(
    `secureStore.get of a key never stored: ${
      want.secureStoreGetMayBeNull ? "null or " : ""
    }SecureStoreUnavailable`,
    (want.secureStoreGetMayBeNull === true && "value" in got &&
      got.value === null) ||
      (got.name === "SecureStoreUnavailable" &&
        String(got.message).includes(want.secureStoreReason)),
    got,
  );
  r.check(
    `secureStore.set is refused (${want.secureStoreReason}; never a plaintext fallback)`,
    set.name === "SecureStoreUnavailable" &&
      String(set.message).includes(want.secureStoreReason),
    set,
  );
  r.check("both answer at once (no unlock to wait for)", ms < 10000, ms);
}

// macOS (laufey API 47): the Keychain, as an item only this app may read.
if (want.secureStoreMac) {
  r.check("secureStore is supported (macOS)", store?.supported === true);
  if (!store) throw new Error("no Deno.desktop.secureStore");
  const full = store as typeof store & {
    delete(s: string, a: string, o?: { timeout?: number }): Promise<unknown>;
  };
  const service = `dev.denext.e2e.keyring.${Deno.pid}`;
  const o = { timeout: 20000 };
  const outcome = async (p: Promise<unknown>) => {
    try {
      return { value: await p };
    } catch (e) {
      return { name: (e as Error).name, message: (e as Error).message };
    }
  };
  // Another program of the user: `security`, killed if it waits on
  // macOS's prompt (what it must do, or be denied, for the app's item).
  const security = async (args: string[], ms = 8000) => {
    const child = new Deno.Command("/usr/bin/security", {
      args,
      stdin: "null",
      stdout: "piped",
      stderr: "piped",
    }).spawn();
    const timer = setTimeout(() => {
      try {
        child.kill("SIGKILL");
      } catch { /* gone */ }
    }, ms);
    const out = await child.output();
    clearTimeout(timer);
    return {
      code: out.code,
      signal: out.signal,
      stdout: new TextDecoder().decode(out.stdout),
    };
  };
  const secret = 'e2e \u2713 "q"\nline 2';
  const set1 = await outcome(full.set(service, "a", "one", o));
  const set2 = await outcome(full.set(service, "a", secret, o));
  const got = await outcome(full.get(service, "a", o));
  r.set("secureStoreMac", { set1, set2, got });
  r.check(
    "secureStore set / replace / get round trip (Keychain)",
    !("name" in set1) && !("name" in set2) && "value" in got &&
      got.value === secret,
    { set1, set2, got },
  );
  const cli = await security([
    "find-generic-password",
    "-s",
    service,
    "-a",
    "a",
    "-w",
  ]);
  r.set("securityCliRead", cli);
  r.check(
    "`security find-generic-password -w` (another program) doesn't get the secret",
    !cli.stdout.includes("line 2") && (cli.code !== 0 || cli.signal !== null),
    cli,
  );
  // An item the `security` CLI wrote (the old way: /usr/bin/security is its
  // trusted app, so any program reads it through `security`) is not the
  // app's: never read here, and in the way of a set.
  await security([
    "add-generic-password",
    "-s",
    service,
    "-a",
    "legacy",
    "-w",
    "old",
  ]);
  const legacyCli = await security([
    "find-generic-password",
    "-s",
    service,
    "-a",
    "legacy",
    "-w",
  ]);
  const legacyGet = await outcome(full.get(service, "legacy", o));
  const legacySet = await outcome(full.set(service, "legacy", "new", o));
  await security(["delete-generic-password", "-s", service, "-a", "legacy"]);
  r.set("secureStoreLegacy", { legacyCli, legacyGet, legacySet });
  r.check(
    "fail first: the old way's item is read back by `security -w`",
    legacyCli.code === 0 && legacyCli.stdout === "old\n",
    legacyCli,
  );
  r.check(
    "an item another program wrote is not read (null)",
    "value" in legacyGet && legacyGet.value === null,
    legacyGet,
  );
  r.check(
    "and is in the way of a set (SecureStoreUnavailable)",
    legacySet.name === "SecureStoreUnavailable" &&
      String(legacySet.message).includes("in the way"),
    legacySet,
  );
  const deleted = await outcome(full.delete(service, "a", o));
  const after = await outcome(full.get(service, "a", o));
  r.check(
    "delete, then get is null",
    !("name" in deleted) && "value" in after && after.value === null,
    { deleted, after },
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
