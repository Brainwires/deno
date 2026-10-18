import dgram from "node:dgram";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { UDP } = require("internal/test/binding").internalBinding("udp_wrap");

// A UDP socket of this isolate, and an untracked duplicate of its fd.
const socket = dgram.createSocket("udp4");
await new Promise<void>((resolve) => socket.bind(0, "127.0.0.1", resolve));
// deno-lint-ignore no-explicit-any
const fd: number = (socket as any)._handle.fdForIpc();

const worker = new Worker(
  new URL("./isolate_udp_fd_ownership_worker.ts", import.meta.url),
  { type: "module", deno: { permissions: "none" } },
);
const workerResult = await new Promise<number>((resolve, reject) => {
  worker.onmessage = (event) => resolve(event.data);
  worker.onerror = (event) => reject(event.error);
  worker.postMessage(fd);
});
worker.terminate();
if (workerResult === 0) {
  throw new Error("worker adopted a UDP descriptor owned by another isolate");
}
console.log("PASS: worker rejected an unregistered UDP descriptor");

// Without all permissions this isolate can't adopt an untracked fd either.
if (new UDP().open(fd) === 0) {
  throw new Error("adopted an untracked UDP descriptor without -A");
}
console.log("PASS: untracked UDP descriptor needs all permissions");
socket.close();
