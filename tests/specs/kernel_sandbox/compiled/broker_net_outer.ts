// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// The unconfined half: the servers, then a program compiled with a
// host-scoped network grant baked in (the ports are only known now), then
// that program. Compiled with `enforce` it takes its supervising broker by
// itself; with `broker` it was asked for.
const [mode] = Deno.args;
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
const echoPort = (echo.addr as Deno.NetAddr).port;
const httpPort = http.addr.port;
const otherPort = (other.addr as Deno.NetAddr).port;

const grant = [
  `localhost:${echoPort}`,
  `localhost:${httpPort}`,
  // the decimal spelling of 127.0.0.1, granted as the name it is to Deno
  `2130706433:${otherPort}`,
].join(",");
const exe = `./prog_bnet_${mode}`;
const compiled = await new Deno.Command(Deno.execPath(), {
  args: [
    "compile",
    "--quiet",
    `--kernel-sandbox=${mode}`,
    "--allow-ffi",
    `--allow-net=${grant}`,
    "--output",
    exe,
    "broker_net_inner.ts",
  ],
}).output();
await Deno.stderr.write(compiled.stderr);
console.log(`compiled: ${compiled.success}`);

const child = new Deno.Command(exe, {
  args: [String(echoPort), String(httpPort), String(otherPort)],
  stdout: "piped",
  stderr: "piped",
}).spawn();
const launched = child.pid;
const { code, stdout, stderr } = await child.output();
// its first line is its parent's PID: the PID that was started here is the
// supervisor's, and the program is that process's child
const text = new TextDecoder().decode(stdout);
const end = text.indexOf("\n");
console.log(
  `the program is a child of the launched PID: ${
    Number(text.slice(0, end)) === launched
  }`,
);
await Deno.stdout.write(new TextEncoder().encode(text.slice(end + 1)));
await Deno.stderr.write(stderr);
console.log(`the program exited with ${code}`);
other.close();
echo.close();
await http.shutdown();
