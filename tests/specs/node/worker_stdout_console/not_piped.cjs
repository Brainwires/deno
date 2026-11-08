const { Worker } = require("node:worker_threads");

// Without `stdout: true` the worker's console output goes to the process's
// stdout.
const w = new Worker(`console.log("from worker");`, { eval: true });
w.on("exit", () => {
  console.log("exited");
});
