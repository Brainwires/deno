// A parent that is not Deno (a service manager, here `env -u`) passes a bound
// listening socket as fd 3; the child adopts it with `listen({ fd: 3 })`,
// with only --allow-net, as Node does for socket activation.
import net from "node:net";
import { spawn } from "node:child_process";

const server = net.createServer();
server.listen(0, "127.0.0.1", () => {
  const { port } = server.address();
  // The listening socket's descriptor (what a service manager would bind).
  const fd = server._handle.fdForIpc();
  const child = spawn("env", [
    "-u",
    "DENO_EXTRA_STDIO_FDS",
    process.execPath,
    "run",
    "--allow-net",
    new URL("./socket_activation_child.mjs", import.meta.url).pathname,
  ], { stdio: ["inherit", "pipe", "inherit", fd] });
  // The parent stops listening; the inherited copy in the child takes over.
  server.close();
  child.stdout.once("data", (line) => {
    process.stdout.write(line);
    const conn = net.connect(port, "127.0.0.1");
    let reply = "";
    conn.setEncoding("utf8");
    conn.on("data", (d) => reply += d);
    conn.on("end", () => {
      console.log("parent got:", reply.trim());
      conn.end();
    });
  });
  child.on("exit", (code) => console.log("child exit", code));
});
