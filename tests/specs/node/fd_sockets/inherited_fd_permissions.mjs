// Descriptors a process inherits at start-up need the permission their kind
// implies in every isolate: a TCP socket needs --allow-net, a pipe unscoped
// read and write. A parent that is not Deno (`env -u`) passes them as fd 3
// (and 4), as a service manager or a shell would.
import fs from "node:fs";
import net from "node:net";
import { spawn } from "node:child_process";

const here = (name) => new URL(name, import.meta.url).pathname;

function run(flags, script, stdio) {
  return spawn("env", [
    "-u",
    "DENO_EXTRA_STDIO_FDS",
    process.execPath,
    "run",
    ...flags,
    here(script),
  ], { stdio });
}

function collect(child) {
  let out = "";
  child.stdout.setEncoding("utf8");
  child.stdout.on("data", (d) => out += d);
  return new Promise((resolve) =>
    child.on("exit", (code) => resolve({ out, code }))
  );
}

function listener() {
  return new Promise((resolve) => {
    const server = net.createServer();
    server.listen(0, "127.0.0.1", () => resolve(server));
  });
}

async function connectAndRead(port) {
  return await new Promise((resolve) => {
    const conn = net.connect(port, "127.0.0.1");
    let reply = "";
    conn.setEncoding("utf8");
    conn.on("data", (d) => reply += d);
    conn.on("error", (err) => resolve(err.code));
    conn.on("end", () => {
      conn.end();
      resolve(reply.trim());
    });
  });
}

// Connects once the child says it listens; resolves without connecting if
// the child exits first.
async function replyOnceListening(child, port, marker) {
  const listening = await new Promise((resolve) => {
    const onData = (d) => {
      if (String(d).includes(marker)) {
        child.stdout.off("data", onData);
        resolve(true);
      }
    };
    child.stdout.on("data", onData);
    child.on("exit", () => resolve(false));
  });
  return listening ? await connectAndRead(port) : "(not listening)";
}

// 1. A worker without net is refused on an inherited socket and an inherited
//    pipe; the main thread, which holds net, read and write, adopts both.
{
  const server = await listener();
  const { port } = server.address();
  const fd = server._handle.fdForIpc();
  const child = run(
    [
      "--allow-net",
      "--allow-read",
      "--allow-write",
      "--unstable-worker-options",
    ],
    "inherited_fd_worker.mjs",
    ["inherit", "pipe", "inherit", fd, "pipe"],
  );
  server.close();
  const result = collect(child);
  // The parent's end of a pipe the child never reads: a reset there is expected.
  child.stdio[4].on("error", () => {});
  child.stdio[4].end("pipe payload\n");
  const reply = await replyOnceListening(child, port, "main listening");
  const { out, code } = await result;
  process.stdout.write(out);
  console.log("parent got:", reply, "| child exit", code);
}

// 2. The main thread without --allow-net is refused on an inherited socket
//    (with a flag given: a child with none would be run with every permission).
{
  const server = await listener();
  const { port } = server.address();
  const fd = server._handle.fdForIpc();
  const child = run(["--allow-read"], "socket_activation_child.mjs", [
    "inherit",
    "pipe",
    "inherit",
    fd,
  ]);
  server.close();
  const result = collect(child);
  const reply = await replyOnceListening(child, port, "child listening");
  const { out, code } = await result;
  process.stdout.write(out);
  console.log("parent got:", reply, "| no-net child exit", code);
}

// 3. An inherited pipe needs unscoped read and write: refused with read only.
for (const flags of [["--allow-read"], ["--allow-read", "--allow-write"]]) {
  const child = run(flags, "inherited_fd_pipe.mjs", [
    "inherit",
    "pipe",
    "inherit",
    "pipe",
  ]);
  const result = collect(child);
  // A refused child exits without reading: on Linux the parent's end then sees ECONNRESET.
  child.stdio[3].on("error", () => {});
  child.stdio[3].end("pipe payload\n");
  const { out, code } = await result;
  process.stdout.write(`${flags.join(" ")}: ${out}`);
  console.log("pipe child exit", code);
}

// 4. A regular file stays refused, even with every permission.
{
  const file = "./regular.txt";
  fs.writeFileSync(file, "a regular file\n");
  const fd = fs.openSync(file, "r");
  const child = run(["-A"], "inherited_fd_pipe.mjs", [
    "inherit",
    "pipe",
    "inherit",
    fd,
  ]);
  fs.closeSync(fd);
  const { out, code } = await collect(child);
  process.stdout.write(`file: ${out}`);
  console.log("file child exit", code);
}
