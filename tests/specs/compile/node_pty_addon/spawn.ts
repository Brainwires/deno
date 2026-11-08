// On Unix node-pty's `spawn` is the runtime's even when the package carries
// its addon, so the child's output and exit arrive (DENEXT.md item 269), and
// `native` is the package's addon under --allow-ffi, `null` without it.
import process from "node:process";
import pty from "node-pty";

const timer = setTimeout(() => {
  console.log("timed out: no data or no exit");
  process.exit(2);
}, 10_000);
console.log("native", pty.native === null ? null : pty.native.fork());
let out = "";
const term = pty.spawn("sh", ["-c", "echo hi; exit 3"], {});
term.onData((data: string) => out += data);
term.onExit(({ exitCode }: { exitCode: number }) => {
  clearTimeout(timer);
  console.log(JSON.stringify(out), "exit", exitCode);
});
