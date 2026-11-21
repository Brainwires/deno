// denext (DENEXT.md item 425): `fork()` inside a run the kernel sandbox
// confines. The npm resolution state the child is handed needs no write to
// the temp folder, and the confined image's own `--cached-only` isn't one
// of the program's flags (`process.execArgv`, which the child gets).
import { fork } from "node:child_process";
import process from "node:process";

const line = `cached-only: ${process.execArgv.includes("--cached-only")}`;
if (process.send) {
  process.send(line, () => process.disconnect());
} else {
  console.log(line);
  const child = fork(import.meta.filename);
  child.on("message", (message) => console.log(`child: ${message}`));
  child.on("error", (err) => console.log(`fork failed: ${err.message}`));
}
