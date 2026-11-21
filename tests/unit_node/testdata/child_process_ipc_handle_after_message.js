// Sends a plain message and then a handle, back to back, each time the
// parent says "go". See the test of the same name in child_process_test.ts.
const net = require("node:net");

const server = net.createServer();
server.listen(0, "127.0.0.1", () => {
  process.on("message", (message) => {
    if (message === "done") {
      server.close();
      process.disconnect();
      return;
    }
    process.send({ plain: message.round });
    process.send({ handle: message.round }, server);
  });
  process.send("ready");
});
