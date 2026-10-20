// The main thread waits on a worker that burns CPU.
console.log("start");
new Worker(import.meta.resolve("./busy_worker_child.js"), { type: "module" });
