// Copyright 2018-2026 the Deno authors. MIT license.
// Under --kernel-sandbox=enforce --allow-net-listen=127.0.0.1:<port> on
// Linux: the address was bound before the sandbox applied, the process has
// no IP socket of its own, and every listener on the address is handed the
// one bound socket.
import net from "node:net";
import { tcpConnect } from "./native.ts";

const port = Number(Deno.args[0]);

// native code can't make a socket: there is no network grant to connect
tcpConnect("native socket", port);

const listener = Deno.listen({ hostname: "127.0.0.1", port });
console.log(
  `Deno.listen: ${(listener.addr as Deno.NetAddr).hostname}, the granted port: ${
    (listener.addr as Deno.NetAddr).port === port
  }`,
);
const conn = await listener.accept();
await conn.write(new TextEncoder().encode("deno"));
conn.close();
listener.close();

// again after a close, from node:net this time
await new Promise<void>((resolve, reject) => {
  const server = net.createServer((socket) => {
    socket.end("node");
    server.close(() => resolve());
  });
  server.on("error", reject);
  server.listen(port, "127.0.0.1", () => console.log("node:net listening"));
});

// another address is not the granted one
try {
  Deno.listen({ hostname: "127.0.0.1", port: port + 1 });
  console.log("another port: listening");
} catch (err) {
  console.log(`another port: ${(err as Error).name}`);
}
