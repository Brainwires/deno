// Copyright 2018-2026 the Deno authors. MIT license.
// e2e: a `node:http` server as the desktop app's server. The runtime's
// DENO_SERVE_ADDRESS=memory:<name> must reach node:http too (frameworks
// built on it: Express, Next's standalone server, ...), not only Deno.serve:
// node:http used to fail with "unknown override kind: 5" and never serve, so
// the window stayed blank. Results go to the "origin" area.

// deno-lint-ignore-file no-explicit-any

import * as http from "node:http";
import { describeError, page, Report } from "../_shared/e2e.ts";

const r = new Report("origin");
const ORIGIN = r.params.origin ?? "denexte2e://app";
const serveAddressEnv = Deno.env.get("DENO_SERVE_ADDRESS") ?? null;

const SCRIPT = `
const out = { origin: location.origin };
try {
  const res = await fetch("/echo?q=1", { method: "POST", headers: { "content-type": "text/plain" }, body: "ping" });
  out.echo = { status: res.status, body: await res.json() };
} catch (e) { out.echo = { error: String(e) }; }
await fetch("/result", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(out) });
`;

function readBody(req: http.IncomingMessage): Promise<string> {
  return new Promise((resolve, reject) => {
    let body = "";
    req.setEncoding("utf8");
    req.on("data", (c) => body += c);
    req.on("end", () => resolve(body));
    req.on("error", reject);
  });
}

const server = http.createServer(async (req, res) => {
  try {
    if (req.url === "/") {
      res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      res.end(page("e2e origin node:http", "", SCRIPT));
    } else if (req.url?.startsWith("/echo")) {
      const body = await readBody(req);
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({
        method: req.method,
        url: req.url,
        host: req.headers.host,
        body,
        remoteAddress: req.socket.remoteAddress ?? null,
      }));
    } else if (req.url === "/result") {
      const out = JSON.parse(await readBody(req));
      res.end("ok");
      queueMicrotask(() => check(out));
    } else {
      res.writeHead(404);
      res.end("not found");
    }
  } catch (e) {
    r.fail("node:http handler threw", describeError(e));
    res.writeHead(500);
    res.end();
  }
});
server.on("error", (e) => {
  r.fail(
    "the node:http server listens on the memory transport",
    describeError(e),
  );
  r.finish();
});
// The app asks for a TCP port; the override puts the server on the memory
// channel instead (nothing is bound on the port).
server.listen(0, "127.0.0.1", () => r.mark("listening"));

function check(out: any) {
  r.set("page", out);
  r.check(
    "DENO_SERVE_ADDRESS is the memory transport",
    /^memory:./.test(serveAddressEnv ?? ""),
    serveAddressEnv,
  );
  r.check(
    "node:http: the page loads at the configured origin",
    out.origin === ORIGIN,
    out.origin,
  );
  const echo = out.echo?.body ?? {};
  r.check(
    "node:http: a POST with a body round-trips",
    out.echo?.status === 200 && echo.method === "POST" &&
      echo.url === "/echo?q=1" && echo.body === "ping",
    out.echo,
  );
  r.check(
    "node:http: the Host header is the origin's host",
    echo.host === new URL(ORIGIN).host,
    echo.host,
  );
  r.check(
    "node:http: the socket names the memory listener",
    typeof echo.remoteAddress === "string" &&
      echo.remoteAddress.startsWith("memory:"),
    echo.remoteAddress,
  );
  server.close();
  r.finish();
}
