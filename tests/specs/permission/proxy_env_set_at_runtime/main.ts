// Regression test: the proxy environment variables (`HTTP_PROXY`,
// `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`, either case) are read once, before
// any user code runs. `fetch()` creates its HTTP client lazily, on the first
// request, and the connection to an environment proxy is not checked against
// `--allow-net` (it is operator configuration). Read at that point, a value set
// with `Deno.env.set()` let a program with `--allow-env` send its requests,
// and their bodies, to a host `--allow-net` does not list.

// The proxy: anything that reaches it is a bypass.
let proxyHits = 0;
const proxy = Deno.serve(
  { hostname: "127.0.0.1", port: 0, onListen: () => {} },
  () => {
    proxyHits++;
    return new Response("from the proxy");
  },
);
const proxyPort = (proxy.addr as Deno.NetAddr).port;

// The destination the child may reach.
const target = Deno.serve(
  { hostname: "127.0.0.1", port: 0, onListen: () => {} },
  () => new Response("from the target"),
);
const targetPort = (target.addr as Deno.NetAddr).port;

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

async function runChild(script: string, env: Record<string, string> = {}) {
  const child = new Deno.Command(Deno.execPath(), {
    args: [
      "run",
      `--allow-net=127.0.0.1:${targetPort}`,
      "--allow-env",
      "-",
    ],
    // Start from an environment with no proxy configured, apart from `env`.
    // Windows treats the two cases of a variable as one, so leave out the
    // other case of anything `env` sets.
    env: { ...withoutCaseOf(NO_PROXY_ENV, env), ...env, NO_COLOR: "1" },
    stdin: "piped",
    stdout: "piped",
    stderr: "null",
  }).spawn();
  const writer = child.stdin.getWriter();
  await writer.write(new TextEncoder().encode(script));
  await writer.close();
  const { stdout } = await child.output();
  await Deno.stdout.write(stdout);
  const hits = proxyHits;
  proxyHits = 0;
  return hits;
}

const fetchTarget = `
try {
  const res = await fetch("http://127.0.0.1:${targetPort}/");
  console.log(await res.text());
} catch (e) {
  console.log("fetch failed: " + e.name);
}
`;

// Each variable, set at runtime before the first fetch: not used.
for (const name of ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"]) {
  const hits = await runChild(
    `Deno.env.set("${name}", "http://127.0.0.1:${proxyPort}");\n` +
      fetchTarget,
  );
  console.log(
    hits === 0
      ? `PASS: ${name} set at runtime was not used`
      : `FAIL: ${name} set at runtime routed the request through the proxy`,
  );
}

// A NO_PROXY set at runtime can't switch off a proxy set at startup either.
{
  const hits = await runChild(
    `Deno.env.set("NO_PROXY", "127.0.0.1");\n` + fetchTarget,
    { HTTP_PROXY: `http://127.0.0.1:${proxyPort}` },
  );
  console.log(
    hits > 0
      ? "PASS: NO_PROXY set at runtime was not used"
      : "FAIL: NO_PROXY set at runtime bypassed the startup proxy",
  );
}

// Control: a proxy set when the process starts is still used.
{
  const hits = await runChild(fetchTarget, {
    HTTP_PROXY: `http://127.0.0.1:${proxyPort}`,
  });
  console.log(
    hits > 0
      ? "PASS: HTTP_PROXY set at startup is used"
      : "FAIL: HTTP_PROXY set at startup was not used",
  );
}

await proxy.shutdown();
await target.shutdown();
