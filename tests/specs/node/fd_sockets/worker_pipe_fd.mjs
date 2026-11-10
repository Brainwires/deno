// A FIFO the main thread opened through node:fs is read in a worker through
// `net.Socket({ fd })`: the worker's wrap gets a dup, and the main thread
// keeps (and later closes) its own descriptor. With only scoped read/write
// access the worker can't adopt another isolate's descriptor.
import fs from "node:fs";
import { execFileSync } from "node:child_process";
import { Worker } from "node:worker_threads";

const fifo = "./worker.fifo";
// Only PATH: a test harness's library-path variables in the child's
// environment need an unscoped --allow-run.
execFileSync("mkfifo", [fifo], {
  env: { PATH: process.env.PATH },
  stdio: ["ignore", "pipe", "pipe"],
});
const fd = fs.openSync(fifo, fs.constants.O_RDWR | fs.constants.O_NONBLOCK);
const worker = new Worker(
  `
  const net = require("node:net");
  const { parentPort, workerData } = require("node:worker_threads");
  let socket;
  try {
    socket = new net.Socket({ fd: workerData.fd, readable: true, writable: false });
  } catch (err) {
    parentPort.postMessage("refused: " + err.code);
  }
  if (socket) {
    socket.setEncoding("utf8");
    socket.on("data", (data) => {
      parentPort.postMessage("worker read: " + data.trim());
      socket.destroy();
    });
    parentPort.postMessage("adopted");
  }
  `,
  { eval: true, workerData: { fd } },
);
worker.on("message", (message) => {
  console.log(message);
  if (message === "adopted") {
    const writer = fs.openSync(fifo, "w");
    fs.writeSync(writer, "through the pipe\n");
    fs.closeSync(writer);
  }
});
worker.on("exit", () => {
  fs.closeSync(fd);
  fs.unlinkSync(fifo);
  console.log("main closed its fd");
});
