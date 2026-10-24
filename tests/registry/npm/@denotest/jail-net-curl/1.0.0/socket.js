// a raw TCP connection from `node` (a Deno process in the jail)
const net = require("node:net");
function done(result) {
  console.log(`node socket 4546: ${result}`);
}
try {
  const socket = net.connect(4546, "127.0.0.1");
  socket.on("connect", () => {
    done("allowed");
    socket.destroy();
  });
  socket.on("error", () => done("denied"));
} catch {
  done("denied");
}
