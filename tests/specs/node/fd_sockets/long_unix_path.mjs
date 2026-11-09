// macOS takes unix socket paths past sun_path's 104 bytes when sun_len covers
// them (libuv does this); listen and connect on a 150+ byte path.
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";

const base = fs.mkdtempSync(path.join(os.tmpdir(), "deno-long-sock-"));
let dir = base;
while (dir.length < 140) dir = path.join(dir, "nested-directory");
fs.mkdirSync(dir, { recursive: true });
const sock = path.join(dir, "server.sock");
console.log("path longer than 150 bytes:", Buffer.byteLength(sock) > 150);

const server = net.createServer((conn) => conn.end("pong"));
server.listen(sock, () => {
  const client = net.connect(sock);
  let reply = "";
  client.on("data", (d) => reply += d);
  client.on("end", () => {
    console.log("reply:", reply);
    server.close(() => {
      fs.rmSync(base, { recursive: true, force: true });
    });
  });
});
server.on("error", (e) => console.log("listen error", e.code));
