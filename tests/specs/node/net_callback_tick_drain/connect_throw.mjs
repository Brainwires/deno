// An exception thrown in a client's `connect` callback reaches
// `uncaughtException`, as in Node, instead of being dropped.
import net from "node:net";

process.on("uncaughtException", (err) => {
  console.log("uncaught:", err.message);
  process.exit(0);
});
const server = net.createServer((conn) => conn.end());
server.listen(0, "127.0.0.1", () => {
  net.connect(server.address().port, "127.0.0.1", () => {
    throw new Error("from connect");
  });
});
setTimeout(() => {
  console.log("exception was dropped");
  process.exit(1);
}, 5000).unref();
