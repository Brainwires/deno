import { parentPort } from "node:worker_threads";

parentPort.on("message", (sab) => {
  const ia = new Int32Array(sab);
  const { async, value } = Atomics.waitAsync(ia, 0, 0);
  if (!async) throw new Error("not async");
  value.then((result) => parentPort.postMessage(`woken: ${result}`));
  Atomics.store(ia, 1, 1);
  // Stay in this iteration until the main thread has notified.
  while (Atomics.load(ia, 2) !== 1);
});
parentPort.postMessage("ready");
