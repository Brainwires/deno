const worker = new Worker(import.meta.resolve("./worker.js"), {
  type: "module",
});
worker.onmessage = (e) => console.log(e.data);
