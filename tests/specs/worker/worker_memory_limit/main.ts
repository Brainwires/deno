// Usage: main.ts <memoryMb> <mode>. Starts a worker with `deno.memoryMb`
// (`worker_grow.ts` for "grow", else `worker.ts` sent the mode), prints what it
// reports or the error event's message, then shows that the parent is still
// alive by running a second, unlimited worker.
const [memoryMb, mode] = Deno.args;

function runAnotherWorker() {
  const worker = new Worker(import.meta.resolve("./worker.ts"), {
    type: "module",
  });
  worker.onmessage = (e) => {
    console.log(`parent still running, second worker: ${e.data}`);
    worker.terminate();
  };
  worker.postMessage("fixed");
}

const worker = new Worker(
  import.meta.resolve(mode === "grow" ? "./worker_grow.ts" : "./worker.ts"),
  { type: "module", deno: { memoryMb: Number(memoryMb) } },
);

worker.onmessage = (e) => {
  console.log(e.data);
  worker.terminate();
  runAnotherWorker();
};

worker.onerror = (e) => {
  e.preventDefault();
  console.log(e.message);
  runAnotherWorker();
};

worker.postMessage(mode);
