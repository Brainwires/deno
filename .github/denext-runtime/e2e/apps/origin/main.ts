// Copyright 2018-2026 the Deno authors. MIT license.
// e2e: the configured app origin and the in-process memory transport.
//
// The page (served at the app origin through the scheme handler) reports
// what it sees; the Deno side then attacks its own WebSocket relay and a TCP
// `Deno.serve` the way another local process could, and checks every
// refusal. Packaged with `.deno-desktop/app.json` { origin, identifier }.

// deno-lint-ignore-file no-explicit-any

import { spawn, spawnSync } from "node:child_process";
import process from "node:process";

import {
  describeError,
  html,
  page,
  Report,
  sleep,
  waitFor,
} from "../_shared/e2e.ts";

const r = new Report("origin");
const ORIGIN = r.params.origin ?? "denexte2e://app";
const appOriginEnv = Deno.env.get("DENO_DESKTOP_APP_ORIGIN") ?? null;
const wsOriginEnv = Deno.env.get("DENO_DESKTOP_WS_ORIGIN") ?? null;
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
  const res = { url: info.wsOrigin + "/ws", messages: [] };
  let ws;
  const timer = setTimeout(() => { res.timeout = true; try { ws.close(); } catch {} resolve(res); }, 8000);
  try { ws = new WebSocket(res.url); } catch (e) { res.error = String(e); clearTimeout(timer); resolve(res); return; }
  ws.onopen = () => ws.send("ping");
  ws.onmessage = (ev) => { res.messages.push(String(ev.data)); if (res.messages.length >= 2) { clearTimeout(timer); ws.close(); resolve(res); } };
  ws.onerror = () => { res.error = "onerror"; };
  ws.onclose = (ev) => { res.close = ev.code; if (res.messages.length < 2) { clearTimeout(timer); resolve(res); } };
});
step("websocket");
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
const tcp = Deno.serve(
  { hostname: "127.0.0.1", port: 0, onListen() {} },
  (req) => {
    tcpHandled++;
    tcpOrigins.push(req.headers.get("origin"));
    return new Response("tcp ok", {
      headers: { "access-control-allow-origin": "*" },
    });
  },
);
const tcpPort = tcp.addr.port;

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

function upgrade(host: string, origins: string[]): string {
  return [
    "GET /ws HTTP/1.1",
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

  // --- the page's WebSocket through the relay ---
  r.check(
    "DENO_DESKTOP_WS_ORIGIN is a loopback ws:// address",
    /^ws:\/\/127\.0\.0\.1:\d+$/.test(wsOriginEnv ?? ""),
    wsOriginEnv,
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
      ["exact app origin", upgrade(host, [ORIGIN]), "101"],
    ];
    for (const [name, head, want] of cases) {
      const got = await rawStatus(Number(port), head);
      r.check(`relay: ${name} -> ${want}`, want.split("|").includes(got), got);
    }
    // Only the exact-origin upgrade reached Deno.serve.
    await sleep(300);
    r.check(
      "relay: only the exact-origin upgrade reached Deno.serve",
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
  } catch (e) {
    r.fail("child process env", describeError(e));
  }

  await nodeChildProcessChecks();

  await tcp.shutdown();
  r.finish();
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
}
