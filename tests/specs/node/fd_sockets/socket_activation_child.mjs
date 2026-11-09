import net from "node:net";
const server = net.createServer((conn) => {
  conn.end("hello from fd 3\n");
  server.close();
});
server.on("error", (err) => {
  console.log("child error", err.code);
  process.exit(1);
});
server.listen({ fd: 3 }, () => console.log("child listening on fd 3"));
