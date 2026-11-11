import process, { version } from "node:process";
import { Worker } from "node:worker_threads";

console.log(process.version, version === process.version);
console.log(process.versions.node);
const { name, lts, sourceUrl } = process.release;
console.log(name, lts, sourceUrl);
// A worker reports the same version.
const worker = new Worker(
  "const { parentPort } = require('node:worker_threads'); parentPort.postMessage(process.version);",
  { eval: true },
);
worker.on("message", (v) => {
  console.log("worker", v === process.version);
  worker.terminate();
});
