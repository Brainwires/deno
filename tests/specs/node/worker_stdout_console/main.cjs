const { Worker } = require("node:worker_threads");

const code = `
console.log("log line");
console.info("info line");
console.error("error line");
console.warn("warn line");
process.stdout.write("write line\\n");
`;

const w = new Worker(code, { eval: true, stdout: true, stderr: true });
let out = "";
let err = "";
w.stdout.on("data", (d) => out += d);
w.stderr.on("data", (d) => err += d);
w.on("exit", () => {
  console.log(JSON.stringify({ out, err }));
});
