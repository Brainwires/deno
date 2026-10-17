import { fork } from "node:child_process";

const serialization = Deno.args[0];
const child = fork(
  new URL("./child.cjs", import.meta.url).pathname,
  [serialization],
  { env: { ...process.env, IPC_PROBE: "from-parent" }, serialization },
);
child.on("message", (msg) => {
  console.log("env", msg.env.IPC_PROBE);
  console.log("getter", msg.viaGetter);
  console.log("toJSON", msg.viaToJSON);
});
child.on("exit", (code) => console.log("exit", code));
