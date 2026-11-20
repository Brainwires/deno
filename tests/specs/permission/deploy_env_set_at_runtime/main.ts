// Regression test: Deno Deploy's infrastructure endpoints
// (`DENO_UNSTABLE_CRON_SOCK`, `DENO_CACHE_LSC_ENDPOINT`) are read once, before
// any user code runs. A worker connects to them without a permission check, so
// a value set later with `Deno.env.set()` must not reach a `Worker` started
// afterwards: that let code with `--allow-env` but no `--allow-net` open a TCP
// connection to any address (the cron socket is dialed when the worker
// starts) or send HTTP requests to any host (the Cache API).

// Anything that connects here is a bypass: the child has no net access.
let connections = 0;
let onConnection: (() => void) | undefined;
const listener = Deno.listen({ hostname: "127.0.0.1", port: 0 });
const port = listener.addr.port;
const accepting = (async () => {
  for await (const conn of listener) {
    connections++;
    onConnection?.();
    conn.close();
  }
})();

async function runChild(
  name: string,
  script: string,
  env: Record<string, string> = {},
) {
  const child = new Deno.Command(Deno.execPath(), {
    args: ["run", "--allow-env", "--unstable-cron", "-"],
    env,
    stdin: "piped",
    stdout: "piped",
    stderr: "null",
  }).spawn();
  const writer = child.stdin.getWriter();
  await writer.write(new TextEncoder().encode(script));
  await writer.close();
  const { stdout } = await child.output();
  await Deno.stdout.write(stdout);
  // Give a stray connection a chance to arrive.
  await new Promise((resolve) => setTimeout(resolve, 300));
  const seen = connections;
  connections = 0;
  return seen;
}

function expectNoConnection(name: string, seen: number) {
  console.log(
    seen === 0
      ? `PASS: ${name} set at runtime was not used`
      : `FAIL: ${name} set at runtime was connected to`,
  );
}

// Control: the variable set when the process starts is still honored.
//
// The cron socket is dialed from the child's event loop, so a child that
// exits at once may never dial it. This one waits on its stdin, which stays
// open until the connection has arrived here (or a generous deadline for a
// regression to report itself has passed).
async function runStartupChild(): Promise<boolean> {
  const script = await Deno.makeTempFile({ suffix: ".ts" });
  await Deno.writeTextFile(
    script,
    `await Deno.stdin.readable.pipeTo(new WritableStream());
console.log("startup child done");`,
  );
  const connected = new Promise<boolean>((resolve) => {
    onConnection = () => resolve(true);
  });
  let timer: ReturnType<typeof setTimeout> | undefined;
  const deadline = new Promise<boolean>((resolve) => {
    timer = setTimeout(() => resolve(false), 60_000);
  });
  const child = new Deno.Command(Deno.execPath(), {
    args: ["run", "--allow-env", "--unstable-cron", script],
    env: { DENO_UNSTABLE_CRON_SOCK: `tcp:127.0.0.1:${port}` },
    stdin: "piped",
    stdout: "piped",
    stderr: "null",
  }).spawn();
  const seen = await Promise.race([connected, deadline]);
  clearTimeout(timer);
  onConnection = undefined;
  await child.stdin.close();
  const { stdout } = await child.output();
  await Deno.stdout.write(stdout);
  await Deno.remove(script);
  connections = 0;
  return seen;
}

const atStartup = await runStartupChild();
console.log(
  atStartup
    ? "PASS: DENO_UNSTABLE_CRON_SOCK set at startup is used"
    : "FAIL: DENO_UNSTABLE_CRON_SOCK set at startup was not used",
);

expectNoConnection(
  "DENO_UNSTABLE_CRON_SOCK",
  await runChild(
    "DENO_UNSTABLE_CRON_SOCK",
    `
Deno.env.set("DENO_UNSTABLE_CRON_SOCK", "tcp:127.0.0.1:${port}");
const worker = new Worker(
  "data:application/javascript,postMessage('ready')",
  { type: "module" },
);
await new Promise((resolve) => (worker.onmessage = resolve));
await new Promise((resolve) => setTimeout(resolve, 500));
worker.terminate();
console.log("cron worker done");
`,
  ),
);

const cacheWorker = `
try {
  const cache = await caches.open("c");
  await cache.match("https://example.com/a");
} catch {
  // a failure is fine; only a connection is a bypass
}
postMessage("done");
`;
expectNoConnection(
  "DENO_CACHE_LSC_ENDPOINT",
  await runChild(
    "DENO_CACHE_LSC_ENDPOINT",
    `
Deno.env.set("DENO_CACHE_LSC_ENDPOINT", "http://127.0.0.1:${port},token");
const worker = new Worker(
  "data:application/javascript," + encodeURIComponent(${
      JSON.stringify(cacheWorker)
    }),
  { type: "module" },
);
await new Promise((resolve) => {
  worker.onmessage = resolve;
  setTimeout(resolve, 3000);
});
worker.terminate();
console.log("cache worker done");
`,
  ),
);

listener.close();
await accepting.catch(() => {});
