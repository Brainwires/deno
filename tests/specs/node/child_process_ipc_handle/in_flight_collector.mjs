// A socket sent with `keepOpen` and destroyed at once is, while the child
// has not read its channel yet, referenced only by the message in flight. On
// macOS the kernel's collector for descriptors in flight (run whenever a
// Unix socket holding one is closed) counts such a socket as unreachable and
// flushes its receive side: the child read an end of stream, ended the
// socket and its reply failed with EPIPE. The sender now keeps its copy until
// the child acknowledges the handle (DENEXT.md item 537).
import { fork } from "node:child_process";
import net from "node:net";
import process from "node:process";

const serialization = process.argv[2] === "advanced" ? "advanced" : "json";
const self = new URL(import.meta.url).pathname;

if (process.argv[3] === "child") {
  process.on("message", (msg, handle) => {
    handle.on("error", (err) => {
      console.error(`child write failed: ${err.code}`);
      process.exit(1);
    });
    handle.end(`child-saw:${msg.greeting}\n`);
  });
  process.on("disconnect", () => process.exit(0));
} else if (process.argv[3] === "sink") {
  setTimeout(() => {}, 10_000);
} else {
  const server = net.createServer((socket) => {
    socket.setEncoding("utf8");
    socket.once("data", (data) => {
      // The child hasn't started reading its channel yet: the socket is in
      // flight, and destroying it leaves the message the only reference.
      child.send({ greeting: data.trim() }, socket, { keepOpen: true });
      socket.destroy();
      // Close a channel that still holds a descriptor in flight, which runs
      // the collector.
      const sink = fork(self, [serialization, "sink"], { serialization });
      sink.on("error", () => {});
      const extra = net.connect(server.address().port, "127.0.0.1", () => {
        sink.send("x", extra, { keepOpen: true }, () => {});
        sink.kill("SIGKILL");
        extra.destroy();
      });
    });
  });
  const child = fork(self, [serialization, "child"], { serialization });
  server.listen(0, "127.0.0.1", () => {
    const client = net.connect(
      server.address().port,
      "127.0.0.1",
      () => client.write("ping\n"),
    );
    let got = "";
    client.setEncoding("utf8");
    client.on("data", (d) => got += d);
    client.on("end", () => {
      console.log(`client got: ${got.trim() || "(end of stream)"}`);
      child.disconnect();
      server.close();
      process.exit(0);
    });
  });
  setTimeout(() => {
    console.error("timeout");
    process.exit(2);
  }, 20_000);
}
