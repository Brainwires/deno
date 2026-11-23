// Reads and discards everything, until the writer closes the connection.
const tls = require("node:tls");

const socket = tls.connect({
  port: Number(process.argv[2]),
  host: "127.0.0.1",
  rejectUnauthorized: false,
});
socket.on("data", () => {});
socket.on("error", () => {});
socket.on("close", () => process.exit(0));
