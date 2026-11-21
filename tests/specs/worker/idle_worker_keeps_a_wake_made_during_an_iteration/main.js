// The main thread notifies an `Atomics.waitAsync` of the worker while the
// worker is in the middle of an event loop iteration (its message handler is
// still running). V8 posts the promise's resolution to the worker's isolate
// as a foreground task and wakes the worker's loop. The iteration then ends
// with nothing pending (a `node:worker_threads` worker waits for messages
// with an unref'd op, and stays alive for its `message` listener), and the
// worker goes back to idling: the task must run without another message
// arriving.
import { Worker } from "node:worker_threads";

const sab = new SharedArrayBuffer(16);
const ia = new Int32Array(sab);
const WAITING = 1;
const NOTIFIED = 2;

const worker = new Worker(new URL("./worker.mjs", import.meta.url));

const timeout = setTimeout(() => {
  console.log("the worker never ran the notified task");
  process.exit(1);
}, 30_000);

worker.on("message", (data) => {
  if (data === "ready") {
    worker.postMessage(sab);
    // Wait for the worker's handler to be waiting, without returning to the
    // event loop: the notification lands while that handler runs.
    while (Atomics.load(ia, WAITING) !== 1);
    console.log("notified:", Atomics.notify(ia, 0));
    Atomics.store(ia, NOTIFIED, 1);
    return;
  }
  console.log(data);
  clearTimeout(timeout);
  worker.terminate();
});
