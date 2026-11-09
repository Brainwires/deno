// Windows: a pipe opened through node:fs can't be taken over by a stream
// wrap; net.Socket({ fd }) refuses it with Node's errno error.
import fs from "node:fs";
import net from "node:net";

const name = `\\\\.\\pipe\\deno-fd-refuse-${process.pid}`;
const server = net.createServer().listen(name, () => {
  const fd = fs.openSync(name, "r+");
  try {
    new net.Socket({ fd });
    console.log("adopted");
  } catch (err) {
    console.log(err.code, err.syscall);
  }
  fs.closeSync(fd);
  server.close();
});
