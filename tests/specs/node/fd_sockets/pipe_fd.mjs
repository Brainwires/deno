// A FIFO opened through node:fs is read through `net.Socket({ fd })`, with
// scoped read/write permissions only, as in Node.
import fs from "node:fs";
import net from "node:net";
import { execFileSync } from "node:child_process";

const fifo = "./test.fifo";
execFileSync("mkfifo", [fifo]);
const fd = fs.openSync(fifo, fs.constants.O_RDWR | fs.constants.O_NONBLOCK);
const socket = new net.Socket({ fd, readable: true, writable: false });
socket.setEncoding("utf8");
socket.on("data", (data) => {
  console.log("read:", data.trim());
  socket.destroy();
  fs.unlinkSync(fifo);
});
const writer = fs.openSync(fifo, "w");
fs.writeSync(writer, "through the pipe\n");
fs.closeSync(writer);
