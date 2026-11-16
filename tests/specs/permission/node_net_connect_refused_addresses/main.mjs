import net from "node:net";

const listener = Deno.listen({ hostname: "127.0.0.1", port: 0 });
const { port } = listener.addr;
(async () => {
  for await (const conn of listener) conn.close();
})();

function connect(name, addresses) {
  return new Promise((resolve) => {
    const socket = net.connect({
      host: "several.addresses.test",
      port,
      lookup: (_host, _options, callback) =>
        callback(null, addresses.map((address) => ({ address, family: 4 }))),
    });
    socket.on("connect", () => {
      console.log(`${name}: connected to ${socket.remoteAddress}`);
      socket.destroy();
      resolve();
    });
    socket.on("error", (e) => {
      const refused = /Requires net access/.test(e.message);
      console.log(`${name}: ${refused ? "refused" : "error"} ${e.name}`);
      resolve();
    });
  });
}

await connect("every address denied", ["127.0.0.2", "127.0.0.3"]);
await connect("first address denied", ["127.0.0.2", "127.0.0.1"]);
listener.close();
