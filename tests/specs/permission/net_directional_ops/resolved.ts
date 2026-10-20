// The deny rules are checked again against the addresses a hostname resolves
// to, per direction: `localhost` passes the hostname check, and its loopback
// addresses are what --deny-net-connect / --deny-net-listen name.
import net from "node:net";
import { probe, settle } from "./probe.ts";

if (Deno.args[0] === "connect") {
  // Listening is allowed, on any address.
  const server = Deno.serve(
    { hostname: "0.0.0.0", port: 0, onListen() {} },
    (req) =>
      req.headers.get("upgrade") === "websocket"
        ? Deno.upgradeWebSocket(req).response
        : new Response("hello"),
  );
  const { port } = server.addr;
  await probe("fetch localhost", () => fetch(`http://localhost:${port}/`));
  await probe("websocket localhost", () => {
    const ws = new WebSocket(`ws://localhost:${port}/`);
    return new Promise((resolve, reject) => {
      // the event's message carries the connection error's
      ws.onerror = (e) => reject(new Error((e as ErrorEvent).message));
      ws.onopen = () => {
        ws.close();
        resolve(undefined);
      };
    });
  });
  await probe(
    "tcp listen localhost",
    () => Deno.listen({ hostname: "localhost", port: 0 }).close(),
  );
} else {
  await probe(
    "tcp listen localhost",
    () => Deno.listen({ hostname: "localhost", port: 0 }).close(),
  );
  await probe("node net listen localhost", () => {
    const server = net.createServer();
    server.listen(0, "localhost");
    return settle(server, "listening").finally(() => server.close());
  });
  await probe(
    "tcp connect localhost",
    () => Deno.connect({ hostname: "localhost", port: 1 }),
  );
}
Deno.exit(0);
