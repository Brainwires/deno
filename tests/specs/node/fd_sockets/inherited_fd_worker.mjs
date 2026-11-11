// fd 3: an inherited listening socket; fd 4: an inherited pipe. A worker
// with no permissions may adopt neither; the main thread then adopts both.
import net from "node:net";

const worker = new Worker(
  new URL("./inherited_fd_worker_child.mjs", import.meta.url),
  { type: "module", deno: { permissions: "none" } },
);
const results = await new Promise((resolve) => {
  worker.onmessage = (e) => resolve(e.data);
});
worker.terminate();
for (const line of results) console.log("worker", line);

try {
  const pipe = new net.Socket({ fd: 4, readable: true, writable: false });
  pipe.setEncoding("utf8");
  let text = "";
  pipe.on("data", (d) => text += d);
  await new Promise((resolve) => pipe.on("end", resolve));
  console.log("main read pipe:", text.trim());
} catch (err) {
  console.log("main pipe error:", err.message);
}

const server = net.createServer((conn) => {
  conn.end("hello from fd 3\n");
  server.close();
});
server.on("error", (err) => {
  console.log("main error", err.code);
  process.exit(1);
});
server.listen({ fd: 3 }, () => console.log("main listening on fd 3"));
