import { fork } from "node:child_process";
import { fileURLToPath } from "node:url";

if (process.argv[2] === "child") {
  process.send({ hello: process.argv[3] });
  process.exit(0);
}

const serialization = process.argv[2];
const CHILDREN = 120;
const AT_ONCE = 12;

function run(id) {
  return new Promise((resolve, reject) => {
    const child = fork(fileURLToPath(import.meta.url), ["child", String(id)], {
      serialization,
    });
    let message = null;
    let exited = false;
    child.on("message", (m) => {
      message = m;
    });
    child.on("exit", (code) => {
      exited = code === 0;
    });
    child.on("error", reject);
    child.on("close", () => {
      if (message?.hello !== String(id)) {
        reject(new Error(`child ${id}: 'close' before its message`));
      } else if (!exited) {
        reject(new Error(`child ${id}: 'close' without a clean 'exit'`));
      } else {
        resolve();
      }
    });
  });
}

let next = 0;
async function worker() {
  while (next < CHILDREN) {
    await run(next++);
  }
}
await Promise.all(Array.from({ length: AT_ONCE }, worker));
console.log(`${CHILDREN} children: each message came before 'close'`);
