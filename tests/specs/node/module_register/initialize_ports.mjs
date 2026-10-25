import { register } from "node:module";
import { MessageChannel } from "node:worker_threads";
import { once } from "node:events";

const { port1, port2 } = new MessageChannel();
const messages = [];
port1.on("message", (msg) => messages.push(msg));
console.log(
  "register",
  register("./hooks_port.mjs", import.meta.url, {
    data: { port: port2 },
    transferList: [port2],
  }),
);
await Promise.all([
  once(port1, "message").then(() => once(port1, "message")),
  import("node:os"),
]);
console.log(messages.join("\n"));
port1.close();
