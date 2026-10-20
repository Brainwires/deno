// Every network operation the permission system gates, by direction. A
// closed port (1) is the connect target, port 0 the listen address; see
// probe.ts for how a result is read.
import dgram from "node:dgram";
import dns from "node:dns";
import http from "node:http";
import inspector from "node:inspector";
import net from "node:net";
import { probe, settle } from "./probe.ts";

const host = "127.0.0.1";
const cert = Deno.readTextFileSync(
  new URL("../../../testdata/tls/localhost.crt", import.meta.url),
);
const key = Deno.readTextFileSync(
  new URL("../../../testdata/tls/localhost.key", import.meta.url),
);

// connect
await probe("tcp connect", () => Deno.connect({ hostname: host, port: 1 }));
await probe("tls connect", () => Deno.connectTls({ hostname: host, port: 1 }));
await probe(
  "quic connect",
  () => Deno.connectQuic({ hostname: host, port: 1, alpnProtocols: ["test"] }),
);
await probe("fetch", () => fetch(`http://${host}:1/`));
await probe("websocket", () => {
  const ws = new WebSocket(`ws://${host}:1/`);
  return new Promise((resolve) => {
    ws.onerror = resolve;
    ws.onclose = resolve;
  });
});
await probe(
  "http client proxy",
  () => Deno.createHttpClient({ proxy: { url: `http://${host}:1` } }).close(),
);
await probe(
  "resolveDns",
  () =>
    Deno.resolveDns("example.com", "A", {
      nameServer: { ipAddr: host, port: 1 },
    }),
);
await probe("kv remote", () => Deno.openKv(`http://${host}:1/`));
await probe("sql postgres", async () => {
  const sql = new Deno.SQL(`postgres://u:p@${host}:1/db?sslmode=disable`);
  try {
    await sql`select 1`;
  } finally {
    await sql.close();
  }
});
await probe("node net connect", () => {
  const socket = net.connect(1, host);
  return settle(socket, "connect").finally(() => socket.destroy());
});
await probe("node http request", () => {
  const req = http.get(`http://${host}:1/`);
  return settle(req, "response");
});
await probe(
  "node dns lookup",
  () => dns.promises.lookup("localhost"),
);

// listen
await probe(
  "tcp listen",
  () => Deno.listen({ hostname: host, port: 0 }).close(),
);
await probe(
  "tls listen",
  () => Deno.listenTls({ hostname: host, port: 0, cert, key }).close(),
);
await probe(
  "udp listen",
  () =>
    Deno.listenDatagram({ hostname: host, port: 0, transport: "udp" })
      .close(),
);
await probe(
  "quic endpoint",
  () => new Deno.QuicEndpoint({ hostname: host, port: 0 }).close(),
);
await probe("Deno.serve", () => {
  const server = Deno.serve(
    { hostname: host, port: 0, onListen() {} },
    () => new Response(),
  );
  return server.shutdown();
});
await probe("node net listen", () => {
  const server = net.createServer();
  server.listen(0, host);
  return settle(server, "listening").finally(() => server.close());
});
await probe("node http listen", () => {
  const server = http.createServer();
  server.listen(0, host);
  return settle(server, "listening").finally(() => server.close());
});
await probe("node dgram bind", () => {
  const socket = dgram.createSocket("udp4");
  socket.bind(0, host);
  return settle(socket, "listening").finally(() => socket.close());
});

// both: a datagram socket is bound (listen) before it sends (connect)
await probe("udp send", async () => {
  const socket = Deno.listenDatagram({
    hostname: host,
    port: 0,
    transport: "udp",
  });
  try {
    await socket.send(new Uint8Array(1), {
      transport: "udp",
      hostname: host,
      port: 1,
    });
  } finally {
    socket.close();
  }
});
await probe("node dgram send", () => {
  const socket = dgram.createSocket("udp4");
  return new Promise<void>((resolve, reject) => {
    socket.once("error", reject);
    socket.send(new Uint8Array(1), 1, host, (err) => {
      socket.close();
      err ? reject(err) : resolve();
    });
  });
});

// last: an inspector that opens prints where it listens
await probe("node inspector open", () => {
  inspector.open(0, host);
  inspector.close();
});
Deno.exit(0);
