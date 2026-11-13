// Reads the descriptor inherited as fd 3 through node:fs's fd APIs, as a
// server reads a start-up token a parent passes on fd 3. The parent is not
// Deno (`env -u DENO_EXTRA_STDIO_FDS`), so nothing announced the fd: node:fs
// has to find it among the descriptors the process inherited.
import fs from "node:fs";
import { Buffer } from "node:buffer";

const report = (line) => console.log(line);
const kind = (st) =>
  st.isFile() ? "file" : st.isFIFO() || st.isSocket() ? "stream" : "other";

try {
  report(`fstatSync: ${kind(fs.fstatSync(3))}`);
} catch (err) {
  report(`refused: ${err.code ?? err.name}`);
  Deno.exit(0);
}

report(`fstat: ${
  kind(
    await new Promise((resolve, reject) =>
      fs.fstat(3, (err, st) => err ? reject(err) : resolve(st))
    ),
  )
}`);

// A worker (where a denext module sidecar runs) reaches it too.
const probe = `
  import fs from "node:fs";
  try {
    const st = fs.fstatSync(3);
    postMessage(st.isFile() || st.isFIFO() || st.isSocket() ? "ok" : "other");
  } catch (err) {
    postMessage("error " + (err.code ?? err.name));
  }
`;
const worker = new Worker(
  `data:text/javascript,${encodeURIComponent(probe)}`,
  { type: "module" },
);
report(
  `worker fstatSync: ${await new Promise((resolve) =>
    worker.onmessage = (e) => resolve(e.data)
  )}`,
);
worker.terminate();

const chunks = [];
const buf = Buffer.alloc(64);
for (;;) {
  let n;
  try {
    n = fs.readSync(3, buf, 0, buf.length, null);
  } catch (err) {
    if (err.code !== "EAGAIN") throw err;
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 10);
    continue;
  }
  if (n === 0) break;
  chunks.push(Buffer.from(buf.subarray(0, n)));
}
report(`readSync: ${Buffer.concat(chunks).toString().trim()}`);

fs.closeSync(3);
try {
  fs.fstatSync(3);
  report("fstatSync after close: ok");
} catch (err) {
  report(`fstatSync after close: ${err.code}`);
}
