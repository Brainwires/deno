const worker = new Worker(import.meta.resolve("./worker.ts"), {
  type: "module",
  deno: { memoryMb: 32 },
});
worker.postMessage("grow_on_message");
