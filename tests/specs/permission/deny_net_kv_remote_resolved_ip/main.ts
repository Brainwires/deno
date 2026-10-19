// Regression test: remote `Deno.openKv` must run the post-resolution
// `--deny-net` check that fetch() and Deno.connect run, both for the metadata
// endpoint it is opened with and for the database URLs that endpoint hands
// back. A deny rule written as an IP literal must hold for a hostname that
// resolves to it (`localhost` here), so a KV Connect server cannot point
// KV traffic at a denied or internal address.

const DATABASE_TOKEN = "MOCKMOCKMOCKMOCKMOCKMOCKMOCK";
const ACCESS_TOKEN = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

// The data path server: any request that reaches it is a bypass. It answers
// 400 so that a request that does get through fails fast instead of retrying.
let dataPathHits = 0;
const dataServer = Deno.serve(
  { hostname: "0.0.0.0", port: 0, onListen: () => {} },
  () => {
    dataPathHits++;
    return new Response("not a real database", { status: 400 });
  },
);
const dataPort = (dataServer.addr as Deno.NetAddr).port;

// The metadata endpoint, which points the strong-consistency data path at the
// data server through `localhost`.
let metadataHits = 0;
const metadataServer = Deno.serve(
  { hostname: "0.0.0.0", port: 0, onListen: () => {} },
  () => {
    metadataHits++;
    return Response.json({
      version: 1,
      databaseId: "11111111-1111-1111-1111-111111111111",
      endpoints: [{
        url: `http://localhost:${dataPort}/db`,
        consistency: "strong",
      }],
      token: DATABASE_TOKEN,
      expiresAt: "2099-01-01T00:00:00Z",
    });
  },
);
const metadataPort = (metadataServer.addr as Deno.NetAddr).port;

async function runChild(denyPort: number, script: string) {
  const child = new Deno.Command(Deno.execPath(), {
    args: [
      "run",
      "--unstable-kv",
      "--allow-net",
      "--allow-env=DENO_KV_ACCESS_TOKEN",
      `--deny-net=127.0.0.1:${denyPort},[::1]:${denyPort}`,
      "-",
    ],
    env: { DENO_KV_ACCESS_TOKEN: ACCESS_TOKEN },
    stdin: "piped",
    stdout: "piped",
    // `denokv_remote` logs the refused data path attempt; only stdout is
    // compared.
    stderr: "null",
  }).spawn();
  const writer = child.stdin.getWriter();
  await writer.write(new TextEncoder().encode(script));
  await writer.close();
  const { stdout } = await child.output();
  await Deno.stdout.write(stdout);
}

// 1. The metadata endpoint itself resolves to a denied address.
await runChild(
  metadataPort,
  `
try {
  const kv = await Deno.openKv("http://localhost:${metadataPort}/meta");
  kv.close();
  console.log("FAIL: openKv through a denied metadata address was allowed");
} catch (e) {
  console.log(
    e instanceof Deno.errors.NotCapable
      ? "PASS: openKv through a denied metadata address denied"
      : "FAIL: wrong error: " + e,
  );
}
`,
);
console.log(
  metadataHits === 0
    ? "PASS: the metadata endpoint was never reached"
    : "FAIL: the metadata endpoint was reached",
);

// 2. The metadata endpoint is allowed, but the database URL it hands back
//    resolves to a denied address.
await runChild(
  dataPort,
  `
const kv = await Deno.openKv("http://localhost:${metadataPort}/meta");
try {
  await kv.get(["key"]);
  console.log("FAIL: a read through a denied database address was allowed");
} catch (e) {
  console.log(
    e instanceof Deno.errors.NotCapable
      ? "PASS: a read through a denied database address denied"
      : "FAIL: wrong error: " + e,
  );
} finally {
  kv.close();
}
`,
);
console.log(
  dataPathHits === 0
    ? "PASS: the database endpoint was never reached"
    : "FAIL: the database endpoint was reached",
);

await dataServer.shutdown();
await metadataServer.shutdown();
