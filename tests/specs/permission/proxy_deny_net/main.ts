// Regression test: `--deny-net` must hold for requests routed through an
// environment proxy.
//
// The "proxy" below answers every request itself instead of forwarding it, so
// any request that reaches it for a denied destination (or while the proxy
// host itself is denied) is a failure. Each case runs in a child with its own
// flags and proxy environment.
//
// - An IP-address deny rule must apply to a proxied destination given as a
//   name. fetch() and node:http resolve the name locally to compare it; a
//   name that does not resolve locally (only the proxy's resolver might know
//   it) fails closed.
// - A name-form deny rule on the proxy host itself must stop the hop to the
//   proxy: the hop was compared only by resolved address against IP rules.

let proxyHits = 0;
const proxy = Deno.serve(
  { hostname: "127.0.0.1", port: 0, onListen: () => {} },
  () => {
    proxyHits++;
    return new Response("from the proxy");
  },
);
const proxyPort = (proxy.addr as Deno.NetAddr).port;
// A port nothing listens on: the destination is never connected to directly.
const targetPort = proxyPort === 65535 ? proxyPort - 1 : proxyPort + 1;

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

async function runChild(
  flags: string[],
  script: string,
  env: Record<string, string>,
): Promise<{ hits: number; out: string }> {
  const child = new Deno.Command(Deno.execPath(), {
    args: ["run", ...flags, "-"],
    env: { ...withoutCaseOf(NO_PROXY_ENV, env), ...env, NO_COLOR: "1" },
    stdin: "piped",
    stdout: "piped",
    stderr: "piped",
  }).spawn();
  const writer = child.stdin.getWriter();
  await writer.write(new TextEncoder().encode(script));
  await writer.close();
  const { stdout } = await child.output();
  const hits = proxyHits;
  proxyHits = 0;
  return { hits, out: new TextDecoder().decode(stdout).trim() };
}

function fetchScript(url: string) {
  return `
try {
  const res = await fetch(${JSON.stringify(url)});
  console.log("ok " + await res.text());
} catch (e) {
  console.log("denied " + (e.cause?.name ?? e.name));
}
`;
}

function nodeHttpScript(url: string) {
  return `
import http from "node:http";
try {
  const body = await new Promise((resolve, reject) => {
    const req = http.get(${JSON.stringify(url)}, (res) => {
      let data = "";
      res.setEncoding("utf8");
      res.on("data", (c) => data += c);
      res.on("end", () => resolve(data));
    });
    req.on("error", reject);
  });
  console.log("ok " + body);
} catch (e) {
  console.log("denied " + e.name);
}
`;
}

const httpProxy = `http://127.0.0.1:${proxyPort}`;

async function expectDenied(
  name: string,
  flags: string[],
  script: string,
  env: Record<string, string>,
) {
  const { hits, out } = await runChild(flags, script, env);
  console.log(
    hits === 0 && out.startsWith("denied")
      ? `PASS: ${name}`
      : `FAIL: ${name} (proxy hits: ${hits}, child: ${out})`,
  );
}

async function expectProxied(
  name: string,
  flags: string[],
  script: string,
  env: Record<string, string>,
) {
  const { hits, out } = await runChild(flags, script, env);
  console.log(
    hits > 0 && out === "ok from the proxy"
      ? `PASS: ${name}`
      : `FAIL: ${name} (proxy hits: ${hits}, child: ${out})`,
  );
}

// fetch(): a destination name that does not resolve locally, under an IP
// deny rule for its port, fails closed.
await expectDenied(
  "fetch, unresolvable name under an IP deny rule",
  ["--allow-net", `--deny-net=127.0.0.1:${targetPort}`],
  fetchScript(`http://proxyonly.invalid:${targetPort}/`),
  { HTTP_PROXY: httpProxy },
);

// The same, with the IP rule written as a subnet.
await expectDenied(
  "fetch, unresolvable name under a subnet deny rule",
  ["--allow-net", "--deny-net=10.0.0.0/8"],
  fetchScript(`http://proxyonly.invalid:${targetPort}/`),
  { HTTP_PROXY: httpProxy },
);

// fetch(): a resolvable alias of a denied address stays denied (it was
// already; kept as a guard for the rework).
await expectDenied(
  "fetch, resolvable alias of a denied address",
  ["--allow-net", `--deny-net=127.0.0.1:${targetPort}`],
  fetchScript(`http://localhost:${targetPort}/`),
  { HTTP_PROXY: httpProxy },
);

// Control: an IP rule for another port does not apply, so an unresolvable
// name still goes through the proxy.
await expectProxied(
  "fetch, IP deny rule for another port does not block",
  ["--allow-net", `--deny-net=127.0.0.1:${targetPort === 1 ? 2 : 1}`],
  fetchScript(`http://proxyonly.invalid:${targetPort}/`),
  { HTTP_PROXY: httpProxy },
);

// Control: with no IP deny rule at all, the proxy may reach names this
// process cannot resolve.
await expectProxied(
  "fetch, no IP deny rule does not block",
  ["--allow-net", "--deny-net=example.com"],
  fetchScript(`http://proxyonly.invalid:${targetPort}/`),
  { HTTP_PROXY: httpProxy },
);

// fetch(): a name-form deny on the proxy host stops the hop to the proxy.
await expectDenied(
  "fetch, name-form deny on the env proxy host",
  ["--allow-net", `--deny-net=localhost:${proxyPort}`],
  fetchScript(`http://127.0.0.1:${targetPort}/`),
  { HTTP_PROXY: `http://localhost:${proxyPort}` },
);

// Control: denying the proxy host on another port does not.
await expectProxied(
  "fetch, deny on the proxy host's other port does not block",
  ["--allow-net", `--deny-net=localhost:${targetPort}`],
  fetchScript(`http://127.0.0.2:${targetPort}/`),
  { HTTP_PROXY: `http://localhost:${proxyPort}` },
);

// node:http with NODE_USE_ENV_PROXY: a resolvable alias of a denied address.
await expectDenied(
  "node:http, resolvable alias of a denied address",
  ["--allow-net", `--deny-net=127.0.0.1:${targetPort}`],
  nodeHttpScript(`http://localhost:${targetPort}/`),
  { HTTP_PROXY: httpProxy, NODE_USE_ENV_PROXY: "1" },
);

// node:http: an unresolvable name under an IP deny rule fails closed.
await expectDenied(
  "node:http, unresolvable name under an IP deny rule",
  ["--allow-net", `--deny-net=127.0.0.1:${targetPort}`],
  nodeHttpScript(`http://proxyonly.invalid:${targetPort}/`),
  { HTTP_PROXY: httpProxy, NODE_USE_ENV_PROXY: "1" },
);

// Control: node:http with no IP deny rule still goes through the proxy.
await expectProxied(
  "node:http, no IP deny rule does not block",
  ["--allow-net", "--deny-net=example.com"],
  nodeHttpScript(`http://proxyonly.invalid:${targetPort}/`),
  { HTTP_PROXY: httpProxy, NODE_USE_ENV_PROXY: "1" },
);

await proxy.shutdown();
