// --deny-net applies to the destination of a request sent through an HTTP
// proxy, by name and by the addresses the name resolves to, for every client
// that takes a proxy: fetch() (plain and CONNECT), WebSocket, node:http and
// node:https with NODE_USE_ENV_PROXY (absolute-form and CONNECT), fetch() on
// Node's global proxy, every redirect hop, and a NO_PROXY host, which goes
// direct. A refused request never reaches the proxy.
//
// The destination is `localhost:<denied port>` with `--deny-net` on
// `127.0.0.1:<denied port>` and `[::1]:<denied port>`: the name passes the name check, so only the check of the resolved
// address refuses it. The proxy is a raw listener that counts every
// request it gets and never forwards one.

const DENIED_PORT_LISTENER = Deno.listen({ hostname: "127.0.0.1", port: 0 });
const deniedPort = DENIED_PORT_LISTENER.addr.port;
// Nothing may connect to the denied destination directly either.
let directHits = 0;
(async () => {
  for await (const conn of DENIED_PORT_LISTENER) {
    directHits++;
    conn.close();
  }
})();

const proxyListener = Deno.listen({ hostname: "127.0.0.1", port: 0 });
const proxyPort = proxyListener.addr.port;
const proxyUrl = `http://127.0.0.1:${proxyPort}`;
let proxyHits = 0;
const proxyLines: string[] = [];

async function serveProxyConn(conn: Deno.Conn) {
  const buf = new Uint8Array(16 * 1024);
  let data = "";
  try {
    while (!data.includes("\r\n\r\n")) {
      const n = await conn.read(buf);
      if (n === null) break;
      data += new TextDecoder().decode(buf.subarray(0, n));
    }
  } catch {
    // ignore
  }
  const line = data.split("\r\n")[0];
  if (line) {
    proxyHits++;
    proxyLines.push(line.split(" ").slice(0, 2).join(" "));
  }
  let response: string;
  if (line.startsWith("CONNECT ")) {
    response = "HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n";
  } else if (line.includes("/redirect")) {
    response =
      `HTTP/1.1 302 Found\r\nlocation: http://proxyonly.invalid:${deniedPort}/\r\n` +
      "content-length: 0\r\nconnection: close\r\n\r\n";
  } else {
    response =
      "HTTP/1.1 200 OK\r\ncontent-length: 9\r\nconnection: close\r\n\r\nvia proxy";
  }
  try {
    await conn.write(new TextEncoder().encode(response));
  } catch {
    // ignore
  }
  try {
    conn.close();
  } catch {
    // ignore
  }
}

(async () => {
  for await (const conn of proxyListener) serveProxyConn(conn);
})();

const NO_PROXY_ENV = Object.fromEntries(
  [
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
    "NODE_USE_ENV_PROXY",
    "DENO_PROXY_RESOLVES_NAMES",
  ].map((name) => [name, ""]),
);

function withoutCaseOf(
  base: Record<string, string>,
  set: Record<string, string>,
): Record<string, string> {
  const taken = new Set(Object.keys(set).map((name) => name.toLowerCase()));
  return Object.fromEntries(
    Object.entries(base).filter(([name]) => !taken.has(name.toLowerCase())),
  );
}

// Prints `refused` for a permission refusal, else what happened.
const REPORT = `
function report(e) {
  const text = String(e?.message ?? e) + " " + String(e?.cause ?? "");
  console.log(/Requires net access|NotCapable/.test(text) ? "refused" : "error: " + text);
}
`;

async function runChild(
  name: string,
  script: string,
  env: Record<string, string>,
  flags = [
    `--allow-net`,
    `--deny-net=127.0.0.1:${deniedPort},[::1]:${deniedPort}`,
  ],
) {
  proxyHits = 0;
  proxyLines.length = 0;
  directHits = 0;
  const child = new Deno.Command(Deno.execPath(), {
    args: ["run", ...flags, "--unsafely-ignore-certificate-errors", "-"],
    env: { ...withoutCaseOf(NO_PROXY_ENV, env), ...env, NO_COLOR: "1" },
    stdin: "piped",
    stdout: "piped",
    stderr: "null",
  }).spawn();
  const writer = child.stdin.getWriter();
  await writer.write(new TextEncoder().encode(REPORT + script));
  await writer.close();
  const { stdout } = await child.output();
  // Let a stray connection land before counting.
  await new Promise((resolve) => setTimeout(resolve, 100));
  const out = new TextDecoder().decode(stdout).trim();
  console.log(
    `${name}: ${out} (proxy: ${proxyHits}${
      proxyLines.length ? " " + proxyLines.join(", ") : ""
    }; direct: ${directHits})`,
  );
}

// Every proxied path, for an alias of a denied address (`localhost`) and for a
// name only the proxy could resolve (which fails closed under an address
// deny rule for its port).
for (
  const denied of [
    `localhost:${deniedPort}`,
    `proxyonly.invalid:${deniedPort}`,
  ]
) {
  await runChild(
    `fetch http ${denied}`,
    `try { const r = await fetch("http://${denied}/"); console.log("reached " + r.status); } catch (e) { report(e); }`,
    { HTTP_PROXY: proxyUrl },
  );

  await runChild(
    `fetch https (CONNECT) ${denied}`,
    `try { const r = await fetch("https://${denied}/"); console.log("reached " + r.status); } catch (e) { report(e); }`,
    { HTTPS_PROXY: proxyUrl },
  );

  await runChild(
    `WebSocket ${denied}`,
    `await new Promise((resolve) => {
      const ws = new WebSocket("ws://${denied}/");
      ws.onopen = () => { console.log("reached"); ws.close(); resolve(); };
      ws.onerror = (e) => { report(e.message ?? e); resolve(); };
    });`,
    { HTTP_PROXY: proxyUrl },
  );

  await runChild(
    `node:http ${denied}`,
    `import http from "node:http";
    await new Promise((resolve) => {
      try {
        const req = http.get("http://${denied}/", (res) => { console.log("reached " + res.statusCode); res.resume(); res.on("end", resolve); });
        req.on("error", (e) => { report(e); resolve(); });
      } catch (e) { report(e); resolve(); }
    });`,
    { HTTP_PROXY: proxyUrl, NODE_USE_ENV_PROXY: "1" },
  );

  await runChild(
    `node:https (CONNECT) ${denied}`,
    `import https from "node:https";
    await new Promise((resolve) => {
      try {
        const req = https.get("https://${denied}/", (res) => { console.log("reached " + res.statusCode); res.resume(); res.on("end", resolve); });
        req.on("error", (e) => { report(e); resolve(); });
      } catch (e) { report(e); resolve(); }
    });`,
    { HTTPS_PROXY: proxyUrl, NODE_USE_ENV_PROXY: "1" },
  );

  await runChild(
    `node:http agent proxyEnv ${denied}`,
    `import http from "node:http";
    const agent = new http.Agent({ proxyEnv: { HTTP_PROXY: "${proxyUrl}" } });
    await new Promise((resolve) => {
      try {
        const req = http.get("http://${denied}/", { agent }, (res) => { console.log("reached " + res.statusCode); res.resume(); res.on("end", resolve); });
        req.on("error", (e) => { report(e); resolve(); });
      } catch (e) { report(e); resolve(); }
    });`,
    {},
  );

  await runChild(
    `fetch on Node's global proxy ${denied}`,
    `import "node:http";
    try { const r = await fetch("http://${denied}/"); console.log("reached " + r.status); } catch (e) { report(e); }`,
    { HTTP_PROXY: proxyUrl, NODE_USE_ENV_PROXY: "1" },
  );
}

// A redirect through the proxy to a name only the proxy could resolve: the
// second hop is checked again and never sent.
await runChild(
  "fetch redirect hop",
  `try { const r = await fetch("http://127.0.0.1:${
    proxyPort + 1
  }/redirect"); console.log("reached " + r.status); } catch (e) { report(e); }`,
  { HTTP_PROXY: proxyUrl },
);

const denied = `localhost:${deniedPort}`;

await runChild(
  "fetch NO_PROXY host",
  `try { const r = await fetch("http://${denied}/"); console.log("reached " + r.status); } catch (e) { report(e); }`,
  { HTTP_PROXY: proxyUrl, NO_PROXY: "localhost" },
);

await runChild(
  "node:http NO_PROXY host",
  `import http from "node:http";
  await new Promise((resolve) => {
    const req = http.get("http://${denied}/", (res) => { console.log("reached " + res.statusCode); res.resume(); res.on("end", resolve); });
    req.on("error", (e) => { report(e); resolve(); });
  });`,
  { HTTP_PROXY: proxyUrl, NO_PROXY: "localhost", NODE_USE_ENV_PROXY: "1" },
);

await runChild(
  "createHttpClient proxy host denied",
  `try {
    const client = Deno.createHttpClient({ proxy: { url: "${proxyUrl}" } });
    const r = await fetch("http://127.0.0.1:${proxyPort + 1}/", { client });
    console.log("reached " + r.status);
  } catch (e) { report(e); }`,
  {},
  ["--allow-net", `--deny-net=127.0.0.1:${proxyPort}`],
);

// An http CONNECT through the proxy names its destination in its request
// target: that authority is checked, not only the host the request is
// addressed to.
await runChild(
  "node:http CONNECT target",
  `import http from "node:http";
  await new Promise((resolve) => {
    try {
      const req = http.request({ host: "localhost", port: ${
    proxyPort + 1
  }, method: "CONNECT", path: "localhost:${deniedPort}" });
      req.on("connect", (res, socket) => { console.log("reached " + res.statusCode); socket.destroy(); resolve(); });
      req.on("error", (e) => { report(e); resolve(); });
      req.end();
    } catch (e) { report(e); resolve(); }
  });`,
  { HTTP_PROXY: proxyUrl, NODE_USE_ENV_PROXY: "1" },
);

// DENO_PROXY_RESOLVES_NAMES=1 (the operator's word that only the proxy can
// resolve names): a name that doesn't resolve locally goes to the proxy; a
// name that does is still checked by address; and a program can't turn it
// on for itself.
const RESOLVES = { HTTP_PROXY: proxyUrl, DENO_PROXY_RESOLVES_NAMES: "1" };
await runChild(
  "proxy resolves: fetch name only the proxy resolves",
  `try { const r = await fetch("http://proxyonly.invalid:${deniedPort}/"); console.log("reached " + r.status); } catch (e) { report(e); }`,
  RESOLVES,
);
await runChild(
  "proxy resolves: node:http name only the proxy resolves",
  `import http from "node:http";
  await new Promise((resolve) => {
    try {
      const req = http.get("http://proxyonly.invalid:${deniedPort}/", (res) => { console.log("reached " + res.statusCode); res.resume(); res.on("end", resolve); });
      req.on("error", (e) => { report(e); resolve(); });
    } catch (e) { report(e); resolve(); }
  });`,
  { ...RESOLVES, NODE_USE_ENV_PROXY: "1" },
);
await runChild(
  "proxy resolves: fetch locally resolved denied name",
  `try { const r = await fetch("http://localhost:${deniedPort}/"); console.log("reached " + r.status); } catch (e) { report(e); }`,
  RESOLVES,
);
await runChild(
  "proxy resolves: set by the program",
  `Deno.env.set("DENO_PROXY_RESOLVES_NAMES", "1");
  try { const r = await fetch("http://proxyonly.invalid:${deniedPort}/"); console.log("reached " + r.status); } catch (e) { report(e); }`,
  { HTTP_PROXY: proxyUrl },
  [
    `--allow-net`,
    `--allow-env`,
    `--deny-net=127.0.0.1:${deniedPort},[::1]:${deniedPort}`,
  ],
);

// Controls: a granted destination goes through the proxy, and the
// environment proxy's host needs no grant of its own for fetch().
await runChild(
  "control: fetch granted destination",
  `try { const r = await fetch("http://localhost:${
    proxyPort + 1
  }/"); console.log("reached " + r.status); } catch (e) { report(e); }`,
  { HTTP_PROXY: proxyUrl },
  [
    `--allow-net=localhost`,
    `--deny-net=127.0.0.1:${deniedPort},[::1]:${deniedPort}`,
  ],
);

await runChild(
  "control: node:http granted destination",
  `import http from "node:http";
  await new Promise((resolve) => {
    const req = http.get("http://localhost:${
    proxyPort + 1
  }/", (res) => { console.log("reached " + res.statusCode); res.resume(); res.on("end", resolve); });
    req.on("error", (e) => { report(e); resolve(); });
  });`,
  { HTTP_PROXY: proxyUrl, NODE_USE_ENV_PROXY: "1" },
);

DENIED_PORT_LISTENER.close();
proxyListener.close();
