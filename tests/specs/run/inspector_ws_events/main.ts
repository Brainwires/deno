// Get an open port
const listener = Deno.listen({ port: 0 });
const port = (listener.addr as Deno.NetAddr).port;
listener.close();

const child = new Deno.Command(Deno.execPath(), {
  args: ["run", "--inspect=127.0.0.1:" + port, "--watch", "a.ts"],
  stdin: "null",
  stdout: "null",
  stderr: "null",
}).spawn();

// The `--watch` child never exits by itself: end it, and wait for it, before
// this process exits, or it is left running after the test. The socket closes
// with an error once the child is gone; only the first event counts.
let finishing = false;
async function finish() {
  if (finishing) return;
  finishing = true;
  try {
    child.kill();
  } catch {
    // Already gone.
  }
  await child.status;
  Deno.exit();
}

// Wait a bit for the inspector server to be ready
await new Promise((r) => setTimeout(r, 500));

const ws = new WebSocket(`ws://127.0.0.1:${port}/ws/events`);
ws.onopen = async () => {
  console.log("connected");
  await Deno.writeTextFile("a.ts", `console.log("bar");`);
};
ws.onmessage = async (msg) => {
  if (finishing) return;
  console.log("message", msg.data);
  await finish();
};

ws.onerror = async () => {
  if (finishing) return;
  console.log("connection_failed");
  await finish();
};
