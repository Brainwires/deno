// The unconfined half: the servers the confined program reaches, then that
// program under --kernel-sandbox=broker and a host-scoped grant.

// a TCP echo server and an HTTP server the grant names, and an echo server
// it names only by another spelling of its address
function serveEcho(listener: Deno.Listener) {
  (async () => {
    for await (const conn of listener) {
      conn.readable.pipeTo(conn.writable).catch(() => {});
    }
  })().catch(() => {});
}
const echo = Deno.listen({ hostname: "127.0.0.1", port: 0 });
serveEcho(echo);
const http = Deno.serve(
  { hostname: "127.0.0.1", port: 0, onListen() {} },
  () => new Response("hello from the granted server"),
);
const other = Deno.listen({ hostname: "127.0.0.1", port: 0 });
serveEcho(other);
// a free port for the inner program to listen on
const probe = Deno.listen({ hostname: "127.0.0.1", port: 0 });
const listenPort = (probe.addr as Deno.NetAddr).port;
probe.close();

const echoPort = (echo.addr as Deno.NetAddr).port;
const httpPort = http.addr.port;
const otherPort = (other.addr as Deno.NetAddr).port;

const grant = [
  `localhost:${echoPort}`,
  `localhost:${httpPort}`,
  `127.0.0.1:${listenPort}`,
  // the decimal spelling of 127.0.0.1, granted as the name it is to Deno
  `2130706433:${otherPort}`,
  "127.0.0.2",
].join(",");
// BROKER_SPEC_AUTO: no mode is written; `auto` at the stage where it
// enforces takes the broker by itself (this run has no terminal)
const auto = Deno.env.get("BROKER_SPEC_AUTO") === "1";
const { code, stdout, stderr } = await new Deno.Command(Deno.execPath(), {
  env: auto ? { DENO_INTERNAL_KERNEL_SANDBOX_STAGE: "r4" } : {},
  args: [
    "run",
    "--quiet",
    "--no-prompt",
    "--unstable-net",
    ...(auto ? [] : ["--kernel-sandbox=broker"]),
    "--allow-ffi",
    `--allow-net=${grant}`,
    "net_inner.ts",
    String(echoPort),
    String(httpPort),
    String(otherPort),
    String(listenPort),
  ],
}).output();
await Deno.stdout.write(stdout);
await Deno.stderr.write(stderr);
console.log(`inner exited with ${code}`);
other.close();
echo.close();
await http.shutdown();
