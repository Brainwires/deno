// Under --allow-net=localhost:<echo>,localhost:<http>,127.0.0.1:<listen>,
// 2130706433:<other>,127.0.0.2
import net from "node:net";
import nodeHttp from "node:http";

const [echoPort, httpPort, otherPort, listenPort] = Deno.args.map(Number);

function message(err: unknown): string {
  const text = err instanceof Error ? `${err.name}: ${err.message}` : `${err}`;
  // ports differ from run to run
  return text.replace(/:\d{2,5}\b/g, ":PORT").split("\n")[0];
}

async function attempt(what: string, fn: () => Promise<string>) {
  try {
    console.log(`${what}: ${await fn()}`);
  } catch (err) {
    console.log(`${what}: ${message(err)}`);
  }
}

async function echoOnce(conn: Deno.Conn): Promise<string> {
  await conn.write(new TextEncoder().encode("ping"));
  const buf = new Uint8Array(4);
  await conn.read(buf);
  conn.close();
  return new TextDecoder().decode(buf);
}

await attempt("Deno.connect, a granted name", async () =>
  await echoOnce(
    await Deno.connect({ hostname: "localhost", port: echoPort }),
  ));

await attempt(
  "fetch, a granted name",
  async () => await (await fetch(`http://localhost:${httpPort}/`)).text(),
);

await attempt(
  "node:net, a granted name",
  () =>
    new Promise((resolve, reject) => {
      const socket = net.connect({
        host: "localhost",
        port: echoPort,
        family: 4,
      });
      socket.on("error", reject);
      socket.on("connect", () => socket.write("pong"));
      socket.on("data", (data) => {
        socket.destroy();
        resolve(String(data));
      });
    }),
);

await attempt(
  "node:http, a granted name",
  () =>
    new Promise((resolve, reject) => {
      nodeHttp.get(
        { host: "localhost", port: httpPort, family: 4, agent: false },
        (res) => {
          let body = "";
          res.on("data", (chunk) => body += chunk);
          res.on("end", () => resolve(body));
        },
      ).on("error", reject);
    }),
);

await attempt("Deno.listen, a granted address", async () => {
  const listener = Deno.listen({ hostname: "127.0.0.1", port: listenPort });
  const accepted = listener.accept();
  const client = await Deno.connect({
    hostname: "127.0.0.1",
    port: listenPort,
  });
  const server = await accepted;
  server.readable.pipeTo(server.writable).catch(() => {});
  const text = await echoOnce(client);
  listener.close();
  return text;
});

await attempt(
  "node:net server, a granted address",
  () =>
    new Promise((resolve, reject) => {
      const server = net.createServer((socket) => socket.pipe(socket));
      server.on("error", reject);
      server.listen(listenPort, "127.0.0.1", () => {
        const socket = net.connect({ host: "127.0.0.1", port: listenPort });
        socket.on("error", reject);
        socket.on("connect", () => socket.write("node"));
        socket.on("data", (data) => {
          socket.destroy();
          server.close(() => resolve(String(data)));
        });
      });
    }),
);

// Deno's own check refuses what the grant doesn't name, before any route
await attempt(
  "Deno.connect, a port the grant doesn't name",
  async () =>
    await echoOnce(
      await Deno.connect({ hostname: "localhost", port: otherPort }),
    ),
);

// Deno takes "2130706433" as a name, and the grant names it; the resolver
// reads it as 127.0.0.1, an address the grant doesn't name. Deno's checks
// alone connect; the broker's authority refuses.
await attempt(
  "Deno.connect, another spelling of an address",
  async () =>
    await echoOnce(
      await Deno.connect({ hostname: "2130706433", port: otherPort }),
    ),
);

// a UDP socket can send to any address: the authority hands out none
// under a host-scoped grant (without it the bind is the OS's to answer:
// 127.0.0.2 is an address on Linux, not on macOS)
await attempt("Deno.listenDatagram, a granted address", () => {
  try {
    const socket = Deno.listenDatagram({
      hostname: "127.0.0.2",
      port: 0,
      transport: "udp",
    });
    socket.close();
    return Promise.resolve("left to the OS");
  } catch (err) {
    if (err instanceof Deno.errors.PermissionDenied) throw err;
    return Promise.resolve("left to the OS");
  }
});
