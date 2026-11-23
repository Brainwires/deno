// `function write() { socket.write(chunk, write) }` on a TLS socket whose
// peer, in another process, keeps the connection's buffer empty: each
// record is written to the TCP handle in full on the spot, and the callback
// of that write writes the next one. libuv runs such a callback in the
// pending phase of the next iteration and makes one pass over that queue, so
// the timers phase comes round between two writes. Completed in a loop until
// a write did not complete on the spot, the callbacks kept the event loop in
// its I/O phase for as long as the reader kept up, and the timer below never
// fired.
//
// (A plain `net.Socket` is no use here: its write completes during the call,
// so its callback comes from `process.nextTick`, and the same writer keeps
// the tick queue busy in Node.js too.)
const tls = require("node:tls");
const fs = require("node:fs");
const { spawn } = require("node:child_process");

// Small enough that the reader drains each one before the next is written.
const chunk = Buffer.alloc(16, "x");

const server = tls.createServer({
  key: fs.readFileSync("../../../testdata/tls/localhost.key"),
  cert: fs.readFileSync("../../../testdata/tls/localhost.crt"),
}, (socket) => {
  let writes = 0;
  let done = false;
  socket.on("error", () => {});
  function write() {
    if (done) return;
    writes++;
    socket.write(chunk, write);
  }
  write();
  setTimeout(() => {
    done = true;
    console.log(`the timer fired, after writes: ${writes > 0}`);
    socket.destroy();
    server.close();
  }, 200);
});

server.listen(0, "127.0.0.1", () => {
  const reader = spawn(
    process.execPath,
    ["reader.cjs", `${server.address().port}`],
    { stdio: ["ignore", "inherit", "inherit"] },
  );
  reader.on("exit", () => console.log("the reader saw the end"));
});
