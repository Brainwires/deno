// Copyright 2018-2026 the Deno authors. MIT license.
// e2e: the configured app origin and the in-process memory transport.
//
// The page (served at the app origin through the scheme handler) reports
// what it sees; the Deno side then attacks its own WebSocket relay and a TCP
// `Deno.serve` the way another local process could, and checks every
// refusal. Packaged with `.deno-desktop/app.json` { origin, identifier }.

// deno-lint-ignore-file no-explicit-any

import { fork, spawn, spawnSync } from "node:child_process";
import process from "node:process";
import { fileURLToPath } from "node:url";

import {
  BrowserWindow,
  describeError,
  desktop,
  html,
  page,
  Report,
  sleep,
  waitFor,
} from "../_shared/e2e.ts";

// This app started by itself as `spawn(process.execPath, [SPAWN_CHILD], {
// stdio: [..., "ipc"] })` (see nodeChildProcessChecks): a compiled binary
// runs its own entrypoint with the arguments, here headless. Answer over IPC
// and exit, before anything of the app starts.
const SPAWN_CHILD = "e2e-spawn-ipc-child";
if (Deno.args.includes(SPAWN_CHILD) && process.send) {
  await new Promise<void>((resolve) =>
    process.send!(
      { ok: true, pid: process.pid, args: Deno.args },
      () => resolve(),
    )
  );
  process.disconnect();
  Deno.exit(0);
}

const r = new Report("origin");
// Launches forwarded to this instance (it holds the single-instance lock):
// a worker launch must never be one.
const secondInstances: unknown[] = [];
desktop.addEventListener("secondinstance", (e: CustomEvent) => {
  secondInstances.push(e.detail);
});
const ORIGIN = r.params.origin ?? "denexte2e://app";
const appOriginEnv = Deno.env.get("DENO_DESKTOP_APP_ORIGIN") ?? null;
const wsOriginEnv = Deno.env.get("DENO_DESKTOP_WS_ORIGIN") ?? null;
// The relay URL with this launch's token: what the page dials (plus a path).
const wsUrlEnv = Deno.env.get("DENO_DESKTOP_WS_URL") ?? null;
const serveAddressEnv = Deno.env.get("DENO_SERVE_ADDRESS") ?? null;

const SCRIPT = `
const out = {};
const post = (path, body) => fetch(path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
const step = (s) => post("/log", { step: s }).catch(() => {});
addEventListener("error", (e) => post("/log", { error: String(e.message) }));
addEventListener("unhandledrejection", (e) => post("/log", { error: String(e.reason) }));
step("loaded");
out.origin = location.origin;
out.href = location.href;
out.isSecureContext = globalThis.isSecureContext;
out.cryptoSubtle = typeof crypto.subtle;
out.digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode("x")).then((b) => b.byteLength, (e) => String(e));
const info = await (await fetch("/info")).json();
step("info");
out.info = info;
// A streamed response arrives chunk by chunk.
try {
  const t0 = performance.now();
  const res = await fetch("/stream", { cache: "no-store" });
  const reader = res.body.getReader();
  const at = [];
  for (;;) { const { done } = await reader.read(); if (done) break; at.push(Math.round(performance.now() - t0)); }
  out.stream = { status: res.status, at };
} catch (e) { out.stream = { error: String(e) }; }
step("stream");
// A response the page gives up on (one chunk, then nothing): the app's stream
// must be cancelled, not left open for good.
try {
  const ac = new AbortController();
  const res = await fetch("/hang", { cache: "no-store", signal: ac.signal });
  const reader = res.body.getReader();
  await reader.read();
  ac.abort();
  out.hang = { status: res.status };
} catch (e) { out.hang = { error: String(e) }; }
step("hang");
// A WebSocket through the relay.
out.ws = await new Promise((resolve) => {
  const res = { url: info.wsUrl + "/ws", messages: [] };
  let ws;
  const timer = setTimeout(() => { res.timeout = true; try { ws.close(); } catch {} resolve(res); }, 8000);
  try { ws = new WebSocket(res.url); } catch (e) { res.error = String(e); clearTimeout(timer); resolve(res); return; }
  ws.onopen = () => ws.send("ping");
  ws.onmessage = (ev) => { res.messages.push(String(ev.data)); if (res.messages.length >= 2) { clearTimeout(timer); ws.close(); resolve(res); } };
  ws.onerror = () => { res.error = "onerror"; };
  ws.onclose = (ev) => { res.close = ev.code; if (res.messages.length < 2) { clearTimeout(timer); resolve(res); } };
});
step("websocket");
// The page's own POST, and one from a document of another (opaque) origin:
// the scheme bridge marks only the second (x-deno-desktop-cross-origin).
try {
  await fetch("/marker/same", { method: "POST", body: "x" });
  const frame = document.createElement("iframe");
  frame.sandbox = "allow-scripts";
  frame.srcdoc = "<script>fetch(" + JSON.stringify(location.origin + "/marker/cross") +
    ", { method: 'POST', mode: 'no-cors', body: 'x' }).catch(() => {});</" + "script>";
  document.body.appendChild(frame);
} catch (e) { post("/log", { error: "marker: " + String(e) }); }
step("marker");
// A reader that stops: the app's stream must be held back (laufey API 44
// backpressure), not drained into the backend's memory.
try {
  const ac = new AbortController();
  const res = await fetch("/flood", { cache: "no-store", signal: ac.signal });
  const reader = res.body.getReader();
  let read = 0;
  while (read < 1024 * 1024) {
    const { value, done } = await reader.read();
    if (done) break;
    read += value.byteLength;
  }
  await new Promise((r) => setTimeout(r, 3000));
  await post("/flood-paused", { read });
  ac.abort();
} catch (e) { post("/log", { error: "flood: " + String(e) }); }
step("flood");
// The app's own document calls a binding: the handler learns its origin.
try {
  out.whoami = await window.bindings.e2eWhoami();
} catch (e) { out.whoami = "ERR: " + String(e?.message ?? e); }
step("bindings");
// A cross-origin fetch carries the app origin.
try {
  const res = await fetch("http://127.0.0.1:" + info.tcpPort + "/cors", { cache: "no-store", signal: AbortSignal.timeout(10000) });
  out.crossOrigin = { status: res.status, body: await res.text() };
} catch (e) { out.crossOrigin = { error: String(e) }; }
step("cross-origin");
await post("/result", out);
`;

let pageResult: any = null;
const hang = { started: false, cancelled: false };
let pageRequest: Record<string, unknown> | null = null;
// /flood: what the app produced, and what it had produced when the page
// (having read 1 MiB) reported its pause.
const flood = { produced: 0, atPause: -1, pageRead: -1 };
const FLOOD_TOTAL = 256 * 1024 * 1024;
// What the app saw of /marker/<kind> requests.
const markerSeen: Record<
  string,
  { marker: string | null; origin: string | null; site: string | null }
> = {};
const wsUpgrades: Record<string, unknown>[] = [];

Deno.serve((req, info) => {
  const url = new URL(req.url);
  switch (url.pathname) {
    case "/":
      return html(page("e2e origin", "", SCRIPT));
    case "/info":
      return Response.json({
        tcpPort,
        wsOrigin: wsOriginEnv,
        wsUrl: wsUrlEnv,
        reqUrl: req.url,
        remoteAddr: info.remoteAddr,
      });
    case "/hang": {
      const enc = new TextEncoder();
      hang.started = true;
      return new Response(
        new ReadableStream<Uint8Array>({
          start(c) {
            c.enqueue(enc.encode(`first ${"x".repeat(8192)}\n`));
          },
          cancel() {
            hang.cancelled = true;
          },
        }),
        { headers: { "cache-control": "no-store" } },
      );
    }
    case "/stream": {
      const enc = new TextEncoder();
      return new Response(
        new ReadableStream<Uint8Array>({
          async start(c) {
            for (let i = 0; i < 5; i++) {
              // 8 KiB each: an engine may hold back a tiny chunk.
              c.enqueue(enc.encode(`chunk ${i} ${"x".repeat(8192)}\n`));
              await sleep(300);
            }
            c.close();
          },
        }),
        {
          headers: {
            "content-type": "text/plain",
            "cache-control": "no-store",
          },
        },
      );
    }
    case "/ws": {
      wsUpgrades.push({
        origin: req.headers.get("origin"),
        url: req.url,
        transport: (info.remoteAddr as { transport?: string }).transport,
      });
      const { socket, response } = Deno.upgradeWebSocket(req);
      socket.onopen = () => socket.send("hello");
      socket.onmessage = (ev) => socket.send(`echo:${ev.data}`);
      return response;
    }
    case "/flood": {
      const chunk = new Uint8Array(1024 * 1024);
      return new Response(
        new ReadableStream<Uint8Array>({
          pull(c) {
            if (flood.produced >= FLOOD_TOTAL) return c.close();
            flood.produced += chunk.byteLength;
            c.enqueue(chunk.slice());
          },
        }, { highWaterMark: 1 }),
        { headers: { "cache-control": "no-store" } },
      );
    }
    case "/flood-paused":
      return req.json().then((body) => {
        flood.atPause = flood.produced;
        flood.pageRead = body.read;
        return new Response("ok");
      });
    case "/marker/same":
    case "/marker/cross":
      markerSeen[url.pathname.slice("/marker/".length)] = {
        marker: req.headers.get("x-deno-desktop-cross-origin"),
        origin: req.headers.get("origin"),
        site: req.headers.get("sec-fetch-site"),
      };
      return new Response("ok");
    case "/log":
      return req.text().then((t) => {
        r.mark(`page: ${t}`);
        return new Response("ok");
      });
    case "/result":
      return req.json().then((body) => {
        pageResult = body;
        pageRequest = {
          url: req.url,
          origin: req.headers.get("origin"),
          host: req.headers.get("host"),
          remoteAddr: info.remoteAddr,
        };
        queueMicrotask(() =>
          afterPage().catch((e) => {
            r.fail("server-side checks threw", describeError(e));
            r.finish();
          })
        );
        return new Response("ok");
      });
    default:
      return new Response("not found", { status: 404 });
  }
});

// A plain TCP server, as any app could run next to the desktop one (started
// after the app server: the first Deno.serve takes DENO_SERVE_ADDRESS): the page
// fetches it cross-origin (its Origin header must be the app origin), and the
// forgery checks below send it `http+memory:` targets.
let tcpHandled = 0;
const tcpOrigins: (string | null)[] = [];
// What a page at another origin (this TCP server) got from the app's
// bindings, after the main window navigated there.
let remoteBridge: Record<string, unknown> | null = null;
const REMOTE_BRIDGE_PAGE = page(
  "e2e remote",
  "",
  `const out = {};
const call = (name) => window.bindings[name]().then((v) => ({ value: v }), (e) => ({ error: String(e?.message ?? e) }));
try {
  out.whoami = await call("e2eWhoami");
  out.remoteOk = await call("e2eRemoteOk");
} catch (e) { out.error = String(e); }
await fetch("/bridge-result", { method: "POST", body: JSON.stringify(out) });`,
);
const tcp = Deno.serve(
  { hostname: "127.0.0.1", port: 0, onListen() {} },
  (req) => {
    const path = new URL(req.url).pathname;
    if (path === "/bridge") return html(REMOTE_BRIDGE_PAGE);
    if (path === "/bridge-result") {
      return req.json().then((body) => {
        remoteBridge = body;
        return new Response("ok");
      });
    }
    tcpHandled++;
    tcpOrigins.push(req.headers.get("origin"));
    return new Response("tcp ok", {
      headers: { "access-control-allow-origin": "*" },
    });
  },
);
const tcpPort = tcp.addr.port;
const tcpOrigin = `http://127.0.0.1:${tcpPort}`;

// The app's bindings (laufey API 44 reports the calling document's origin):
// `e2eWhoami` answers only the app's own documents and tells the handler
// which one called; `e2eRemoteOk` also opts the TCP server's origin in.
const win = new BrowserWindow();
win.bind(
  "e2eWhoami",
  (caller: { origin: string; windowId: number }) => caller.origin,
  { withCaller: true },
);
win.bind("e2eRemoteOk", () => "ok", { origins: [tcpOrigin] });

/** Send `head` over a raw TCP connection; the response's status code. */
async function rawStatus(port: number, head: string): Promise<string> {
  const conn = await Deno.connect({ hostname: "127.0.0.1", port });
  try {
    await conn.write(new TextEncoder().encode(head));
    const buf = new Uint8Array(4096);
    let text = "";
    const deadline = Date.now() + 5000;
    while (Date.now() < deadline && !text.includes("\r\n")) {
      const n = await Promise.race([
        conn.read(buf),
        sleep(5000).then(() => null),
      ]);
      if (n === null) break;
      text += new TextDecoder().decode(buf.subarray(0, n));
    }
    return /^HTTP\/1\.1 (\d{3})/.exec(text)?.[1] ??
      `(no status: ${JSON.stringify(text.slice(0, 80))})`;
  } catch (e) {
    return `(error: ${e})`;
  } finally {
    try {
      conn.close();
    } catch { /* closed by the peer */ }
  }
}

/** A WebSocket handshake to `target` (by default `/ws` through this launch's
 * relay token). */
function upgrade(
  host: string,
  origins: string[],
  target = `${new URL(wsUrlEnv ?? "ws://x/").pathname}/ws`,
): string {
  return [
    `GET ${target} HTTP/1.1`,
    `Host: ${host}`,
    "Upgrade: websocket",
    "Connection: Upgrade",
    "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==",
    "Sec-WebSocket-Version: 13",
    ...origins.map((o) => `Origin: ${o}`),
    "",
    "",
  ].join("\r\n");
}

async function afterPage() {
  const p = pageResult;
  r.set("page", p);
  r.set("pageRequest", pageRequest);
  r.set("env", {
    DENO_DESKTOP_APP_ORIGIN: appOriginEnv,
    DENO_DESKTOP_WS_ORIGIN: wsOriginEnv,
    DENO_DESKTOP_WS_URL: wsUrlEnv ? "(set)" : null,
    DENO_SERVE_ADDRESS: serveAddressEnv,
  });

  // --- what the page sees ---
  r.check(
    "page origin is the configured origin",
    p.origin === ORIGIN,
    p.origin,
  );
  r.check(
    "page href is under the origin",
    String(p.href).startsWith(`${ORIGIN}/`),
    p.href,
  );
  r.check("the page is a secure context", p.isSecureContext === true);
  r.check(
    "crypto.subtle works in the page",
    p.cryptoSubtle === "object" && p.digest === 32,
    p.digest,
  );
  r.check(
    "DENO_DESKTOP_APP_ORIGIN is the origin",
    appOriginEnv === ORIGIN,
    appOriginEnv,
  );

  // --- the memory transport, as Deno.serve sees it ---
  r.check(
    "DENO_SERVE_ADDRESS is the memory transport",
    /^memory:./.test(serveAddressEnv ?? ""),
    serveAddressEnv,
  );
  const ra = pageRequest?.remoteAddr as
    | { transport?: string; name?: string }
    | undefined;
  r.check(
    "page requests arrive over the memory transport",
    ra?.transport === "memory" && typeof ra.name === "string" &&
      ra.name.length > 0,
    ra,
  );
  const reqUrl = new URL(String(pageRequest?.url));
  r.check(
    "request.url is http+memory://<origin host>",
    reqUrl.protocol === "http+memory:" && reqUrl.host === new URL(ORIGIN).host,
    pageRequest?.url,
  );
  const s = p.stream ?? {};
  // The server sends a chunk every 300 ms for 1.2 s: reads must come in at
  // least three separate bursts (an engine may coalesce two chunks or split
  // one), the first well before the last.
  const at: number[] = s.at ?? [];
  const bursts = at.filter((t, i) => i === 0 || t - at[i - 1] >= 100).length;
  r.check(
    "a streamed response arrives incrementally",
    s.status === 200 && bursts >= 3 && at[at.length - 1] - at[0] >= 600,
    s,
  );

  // --- a request the page cancels reaches the app ---
  r.check(
    "a response the page aborts is cancelled in the app (laufey on_cancel)",
    hang.started && await waitFor(() => hang.cancelled, 10000),
    { page: p.hang, hang },
  );

  // --- Origin header on a cross-origin request ---
  r.check(
    "a cross-origin fetch succeeds and carries Origin: <app origin>",
    p.crossOrigin?.status === 200 && tcpOrigins.includes(ORIGIN),
    { page: p.crossOrigin, seen: tcpOrigins },
  );

  // --- a response the page stops reading is held back ---
  // laufey answers a write with 0 at its high-water mark, and the bridge then
  // stops pulling the app's stream. CEF reads the response at the page's
  // pace; the WebKit engines and WebView2 read it into their own memory as
  // fast as laufey offers it, so there the bound is the engine's, not the
  // bridge's.
  const held = flood.atPause > 0 && flood.pageRead > 0 &&
    flood.atPause <= 64 * 1024 * 1024 && flood.atPause < FLOOD_TOTAL;
  if (held || r.params.backend === "cef") {
    r.check(
      "a stream the page stops reading is held back (backpressure), not drained",
      held,
      flood,
    );
  } else {
    r.na(
      "a stream the page stops reading is held back (backpressure), not drained",
      `the ${r.params.backend} engine read the whole response into its own memory (${flood.atPause} bytes produced while the page had read ${flood.pageRead}); laufey's high-water mark bounds only its own queue`,
    );
  }

  // --- requests from documents of another origin are marked ---
  r.check(
    "the page's own POST is not marked cross-origin",
    markerSeen.same !== undefined && markerSeen.same.marker === null,
    markerSeen.same,
  );
  if (
    await waitFor(() => markerSeen.cross !== undefined, 5000) &&
    (markerSeen.cross.origin !== null || markerSeen.cross.site !== null)
  ) {
    r.check(
      "a POST from an opaque-origin frame is marked x-deno-desktop-cross-origin: 1",
      markerSeen.cross.marker === "1",
      markerSeen.cross,
    );
  } else if (markerSeen.cross !== undefined) {
    // The bridge can only mark what the engine discloses.
    r.na(
      "a POST from an opaque-origin frame is marked x-deno-desktop-cross-origin: 1",
      "the engine sent the custom-scheme request without Origin or Sec-Fetch-Site",
    );
  } else {
    r.na(
      "a POST from an opaque-origin frame is marked x-deno-desktop-cross-origin: 1",
      "the engine did not deliver a sandboxed frame's no-cors POST to the app scheme",
    );
  }

  // --- the page's WebSocket through the relay ---
  r.check(
    "DENO_DESKTOP_WS_ORIGIN is a loopback ws:// address",
    /^ws:\/\/127\.0\.0\.1:\d+$/.test(wsOriginEnv ?? ""),
    wsOriginEnv,
  );
  r.check(
    "DENO_DESKTOP_WS_URL is the relay origin + /.deno-desktop-relay/<token>",
    wsUrlEnv !== null && wsOriginEnv !== null &&
      wsUrlEnv.startsWith(`${wsOriginEnv}/.deno-desktop-relay/`) &&
      /\/\.deno-desktop-relay\/[0-9a-f]{64}$/.test(wsUrlEnv),
    wsUrlEnv ? "(set)" : null,
  );
  r.check(
    "the page's WebSocket reaches Deno.serve through the relay",
    p.ws?.messages?.[0] === "hello" && p.ws?.messages?.[1] === "echo:ping",
    p.ws,
  );
  r.check(
    "the relayed upgrade carries the app origin over the memory transport",
    wsUpgrades.length >= 1 && wsUpgrades[0].origin === ORIGIN &&
      wsUpgrades[0].transport === "memory",
    wsUpgrades,
  );
  r.check(
    "the relay hands Deno.serve the page's path, without its token",
    wsUpgrades.length >= 1 &&
      new URL(String(wsUpgrades[0].url)).pathname === "/ws",
    wsUpgrades[0]?.url,
  );

  // --- the relay refuses what a page elsewhere (or a local process) sends ---
  if (wsOriginEnv) {
    const { hostname, port } = new URL(wsOriginEnv);
    const host = `${hostname}:${port}`;
    const before = wsUpgrades.length;
    const cases: [string, string, string][] = [
      ["foreign https origin", upgrade(host, ["https://evil.example"]), "403"],
      ["foreign loopback origin", upgrade(host, [`http://${host}`]), "403"],
      [
        "same scheme, other host",
        upgrade(host, [ORIGIN.replace(/\/\/.*$/, "//evil")]),
        "403",
      ],
      ["upper-cased app origin", upgrade(host, [ORIGIN.toUpperCase()]), "403"],
      ["null origin", upgrade(host, ["null"]), "403"],
      ["no origin", upgrade(host, []), "403"],
      [
        "duplicate origin",
        upgrade(host, [ORIGIN, "https://evil.example"]),
        "403",
      ],
      [
        "plain HTTP GET",
        `GET / HTTP/1.1\r\nHost: ${host}\r\nOrigin: ${ORIGIN}\r\n\r\n`,
        "400",
      ],
      // The app origin is not enough: another app at the same origin (every
      // app without one runs at app://localhost) or any local process can
      // send it. Only the page, which got the token from the app, gets in.
      ["exact app origin, no token", upgrade(host, [ORIGIN], "/ws"), "403"],
      [
        "exact app origin, another token",
        upgrade(host, [ORIGIN], `/.deno-desktop-relay/${"0".repeat(64)}/ws`),
        "403",
      ],
      ["exact app origin + token", upgrade(host, [ORIGIN]), "101"],
    ];
    for (const [name, head, want] of cases) {
      const got = await rawStatus(Number(port), head);
      r.check(`relay: ${name} -> ${want}`, want.split("|").includes(got), got);
    }
    // Only the exact-origin upgrade reached Deno.serve.
    await sleep(300);
    r.check(
      "relay: only the exact-origin upgrade with the token reached Deno.serve",
      wsUpgrades.length === before + 1,
      wsUpgrades.slice(before),
    );
  } else {
    r.fail("relay checks: no DENO_DESKTOP_WS_ORIGIN");
  }

  // --- a TCP Deno.serve refuses an http+memory: request target ---
  const handledBefore = tcpHandled;
  for (
    const [name, head] of [
      [
        "POST http+memory://app/x",
        `POST http+memory://app/x HTTP/1.1\r\nHost: app\r\nContent-Length: 0\r\n\r\n`,
      ],
      [
        "GET HTTP+MEMORY://app/x",
        `GET HTTP+MEMORY://app/x HTTP/1.1\r\nHost: app\r\n\r\n`,
      ],
    ]
  ) {
    const got = await rawStatus(tcpPort, head);
    r.check(`tcp: a forged ${name} target is 400`, got === "400", got);
  }
  r.check(
    "tcp: the forged requests never reached the handler",
    tcpHandled === handledBefore,
  );
  const plain = await rawStatus(
    tcpPort,
    `GET /x HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n`,
  );
  r.check("tcp: an ordinary request is still served", plain === "200", plain);

  // --- the env overlay reaches child processes ---
  try {
    const cmd = Deno.build.os === "windows"
      ? new Deno.Command("cmd", { args: ["/c", "set"], stdout: "piped" })
      : new Deno.Command("/usr/bin/env", { stdout: "piped" });
    const env = new TextDecoder().decode((await cmd.output()).stdout);
    const lines = env.split(/\r?\n/);
    const line = (name: string) =>
      lines.find((l) => l.toUpperCase().startsWith(`${name}=`));
    r.check(
      "a child process inherits DENO_DESKTOP_APP_ORIGIN (the env overlay)",
      line("DENO_DESKTOP_APP_ORIGIN") === `DENO_DESKTOP_APP_ORIGIN=${ORIGIN}`,
      line("DENO_DESKTOP_APP_ORIGIN") ?? "(absent)",
    );
    // The memory serve address names a listener in this process: a child
    // Deno that inherited it would serve on a channel nobody can reach.
    r.check(
      "a child process does not inherit DENO_SERVE_ADDRESS=memory:",
      line("DENO_SERVE_ADDRESS") === undefined,
      line("DENO_SERVE_ADDRESS") ?? "(absent)",
    );
    // The relay token is the app's secret.
    r.check(
      "a child process does not inherit DENO_DESKTOP_WS_URL (the relay token)",
      line("DENO_DESKTOP_WS_URL") === undefined,
      line("DENO_DESKTOP_WS_URL") === undefined ? "(absent)" : "(present)",
    );
  } catch (e) {
    r.fail("child process env", describeError(e));
  }

  await nodeChildProcessChecks();
  await remoteBridgeChecks(p);

  await tcp.shutdown();
  r.finish();
}

async function remoteBridgeChecks(p: any) {
  r.check(
    "a binding answers the app's own page; withCaller passes its origin",
    p.whoami === ORIGIN,
    p.whoami,
  );
  // The main window navigated to a page at another origin keeps the app's
  // bindings in its document (the launch file lets every origin reach the
  // runtime: `bridgeOrigins: ["*"]`), but the runtime refuses its calls
  // unless the binding opted that origin in.
  win.navigate(`${tcpOrigin}/bridge`);
  if (!await waitFor(() => remoteBridge !== null, 20_000)) {
    r.fail("a page at another origin reported its binding calls", "timed out");
    return;
  }
  const rb = remoteBridge as any;
  r.check(
    "a binding refuses a page at another origin",
    typeof rb.whoami?.error === "string" &&
      rb.whoami.error.includes("may not call"),
    rb.whoami,
  );
  r.check(
    "a binding that lists the page's origin answers it",
    rb.remoteOk?.value === "ok",
    rb.remoteOk,
  );
}

/** The output of a program that prints its environment, run through
 * `node:child_process` with `opts`. */
function envLines(opts: Record<string, unknown> = {}): string[] {
  const [cmd, args] = Deno.build.os === "windows"
    ? ["cmd", ["/c", "set"]]
    : ["/usr/bin/env", []];
  const out = spawnSync(cmd, args as string[], { encoding: "utf8", ...opts });
  return String(out.stdout ?? "").split(/\r?\n/);
}

function envLine(lines: string[], name: string): string | undefined {
  return lines.find((l) => l.toUpperCase().startsWith(`${name}=`));
}

type ForkOutcome = { code: number | null; message: any; stderr: string };

function forkAndWait(module: string, ms = 60_000): Promise<ForkOutcome> {
  return childOutcome(
    fork(module, [], { stdio: ["ignore", "ignore", "pipe", "ipc"] }),
    ms,
  );
}

function childOutcome(child: any, ms: number): Promise<ForkOutcome> {
  return new Promise((resolve) => {
    let message: any = null;
    let stderr = "";
    child.stderr?.on("data", (d: Uint8Array) => {
      stderr += new TextDecoder().decode(d);
    });
    child.on("message", (m: unknown) => {
      message = m;
    });
    const t = setTimeout(() => {
      child.kill("SIGKILL");
      resolve({ code: null, message, stderr: stderr + " (timed out)" });
    }, ms);
    child.on("exit", (code: number | null) => {
      clearTimeout(t);
      resolve({ code, message, stderr: stderr.slice(-2000) });
    });
  });
}

async function nodeChildProcessChecks() {
  // --- node:child_process: the env overlay (DENO_SERVE_ADDRESS stays in this
  // process even where the app copies process.env into the child's env) ---
  try {
    const plain = envLines();
    r.check(
      "node:child_process: a child does not inherit DENO_SERVE_ADDRESS=memory:",
      envLine(plain, "DENO_SERVE_ADDRESS") === undefined,
      envLine(plain, "DENO_SERVE_ADDRESS") ?? "(absent)",
    );
    r.check(
      "node:child_process: a child inherits DENO_DESKTOP_APP_ORIGIN",
      envLine(plain, "DENO_DESKTOP_APP_ORIGIN") ===
        `DENO_DESKTOP_APP_ORIGIN=${ORIGIN}`,
      envLine(plain, "DENO_DESKTOP_APP_ORIGIN") ?? "(absent)",
    );
    const spread = envLines({
      env: { ...process.env, DENEXT_E2E_EXTRA: "1" },
    });
    r.check(
      "node:child_process: { ...process.env } does not carry DENO_SERVE_ADDRESS",
      envLine(spread, "DENO_SERVE_ADDRESS") === undefined &&
        envLine(spread, "DENEXT_E2E_EXTRA") === "DENEXT_E2E_EXTRA=1",
      {
        serve: envLine(spread, "DENO_SERVE_ADDRESS") ?? "(absent)",
        extra: envLine(spread, "DENEXT_E2E_EXTRA") ?? "(absent)",
      },
    );
    const own = envLines({
      env: { ...process.env, DENO_SERVE_ADDRESS: "127.0.0.1:9" },
    });
    r.check(
      "node:child_process: a DENO_SERVE_ADDRESS the app sets for the child is kept",
      envLine(own, "DENO_SERVE_ADDRESS") === "DENO_SERVE_ADDRESS=127.0.0.1:9",
      envLine(own, "DENO_SERVE_ADDRESS") ?? "(absent)",
    );
    // The asynchronous spawn path builds the environment the same way.
    const [cmd, args] = Deno.build.os === "windows"
      ? ["cmd", ["/c", "set"]]
      : ["/usr/bin/env", []];
    const asyncOut = await new Promise<string>((resolve) => {
      let out = "";
      const child = spawn(cmd, args as string[]);
      child.stdout.on("data", (d: Uint8Array) => {
        out += new TextDecoder().decode(d);
      });
      child.on("close", () => resolve(out));
    });
    r.check(
      "node:child_process: an async spawn does not inherit DENO_SERVE_ADDRESS",
      envLine(asyncOut.split(/\r?\n/), "DENO_SERVE_ADDRESS") === undefined,
    );
  } catch (e) {
    r.fail("node:child_process env", describeError(e));
  }

  // --- forked workers: the app's own modules run headless, nothing else ---
  try {
    const shipped = await forkAndWait(
      fileURLToPath(new URL("./fork_child.js", import.meta.url)),
    );
    r.check(
      "fork: a module the app ships runs headless and answers over IPC",
      shipped.code === 0 && shipped.message?.ok === true,
      shipped,
    );
    r.check(
      "fork: the worker does not inherit DENO_SERVE_ADDRESS=memory:",
      shipped.message !== null && shipped.message.serveAddress === null,
      shipped.message,
    );
    const dir = await Deno.makeTempDir({ prefix: "denext-e2e-fork-" });
    const marker = `${dir}/ran`;
    const outside = `${dir}/outside.js`;
    await Deno.writeTextFile(
      outside,
      `Deno.writeTextFileSync(${JSON.stringify(marker)}, "ran");\n`,
    );
    const refused = await forkAndWait(outside);
    const ran = await Deno.lstat(marker).then(() => true, () => false);
    r.check(
      "fork: a script outside the packaged app is refused, not run",
      refused.code !== 0 && refused.code !== null && !ran,
      { ...refused, ran },
    );
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  } catch (e) {
    r.fail("fork", describeError(e));
  }

  // --- spawn(process.execPath, [script], ipc): a worker launched the env way
  // (argv `<exe> <script>`, only NODE_CHANNEL_FD and the worker token mark
  // it). The host runs it headless before its single-instance check, so the
  // runtime must too: it runs, answers, and is not forwarded here ---
  try {
    const forwardedBefore = secondInstances.length;
    const spawned = await childOutcome(
      spawn(process.execPath, [SPAWN_CHILD], {
        stdio: ["ignore", "ignore", "pipe", "ipc"],
      }),
      60_000,
    );
    r.check(
      "spawn(execPath, [script], ipc): runs headless and answers over IPC",
      spawned.code === 0 && spawned.message?.ok === true &&
        spawned.message.pid !== Deno.pid,
      spawned,
    );
    // A forwarded launch would reach this instance shortly after it exits.
    await sleep(2000);
    r.check(
      "spawn(execPath, [script], ipc): not forwarded as a second instance",
      secondInstances.length === forwardedBefore,
      secondInstances.slice(forwardedBefore),
    );
  } catch (e) {
    r.fail("spawn(execPath, [script], ipc)", describeError(e));
  }
}
