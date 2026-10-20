// Unix domain socket operations by direction. Paths live in the directory
// given as the first argument (created if needed), else a new temporary one;
// `a.sock` is connected to (it doesn't exist, so an allowed connect fails with
// NotFound) and `b.sock` listened on.
import net from "node:net";
import { probe, settle } from "./probe.ts";

const dir = Deno.args[0] ?? Deno.makeTempDirSync();
Deno.mkdirSync(dir, { recursive: true });
const a = `${dir}/a.sock`;
const b = `${dir}/b.sock`;
const cleanup = () => {
  try {
    Deno.removeSync(b);
  } catch {
    // not created
  }
};

cleanup();
await probe("unix connect", () => Deno.connect({ transport: "unix", path: a }));
await probe("node pipe connect", () => {
  const socket = net.connect(a);
  return settle(socket, "connect").finally(() => socket.destroy());
});
await probe("sql postgres unix", async () => {
  const sql = new Deno.SQL({ host: dir, port: 5432 });
  try {
    await sql`select 1`;
  } finally {
    await sql.close();
  }
});
await probe("unix listen", () => {
  Deno.listen({ transport: "unix", path: b }).close();
  cleanup();
});
await probe("node pipe listen", () => {
  const server = net.createServer();
  server.listen(b);
  return settle(server, "listening").finally(() => {
    server.close();
    cleanup();
  });
});
Deno.exit(0);
