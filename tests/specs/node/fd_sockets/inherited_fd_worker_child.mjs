import net from "node:net";

const results = [];
await new Promise((resolve) => {
  const server = net.createServer();
  server.on("error", (err) => {
    results.push(`listen fd 3: ${err.code}`);
    resolve();
  });
  server.listen({ fd: 3 }, () => {
    results.push("listen fd 3: listening");
    server.close();
    resolve();
  });
});
try {
  const socket = new net.Socket({ fd: 4, readable: true, writable: false });
  results.push("socket fd 4: opened");
  socket.destroy();
} catch (err) {
  results.push(`socket fd 4: ${err.code}`);
}
self.postMessage(results);
