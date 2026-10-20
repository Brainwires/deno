// Values that can't be a heap size throw from the constructor, naming the
// option; no worker starts.
for (const memoryMb of [0, -1, NaN, Infinity, "many", 2 ** 60]) {
  try {
    new Worker(import.meta.resolve("./worker.ts"), {
      type: "module",
      deno: { memoryMb: memoryMb as number },
    });
    console.log(`${memoryMb}: accepted`);
  } catch (e) {
    console.log(`${memoryMb}: ${(e as Error).name}: ${(e as Error).message}`);
  }
}
