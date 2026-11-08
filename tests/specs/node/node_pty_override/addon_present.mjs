// node-pty whose package carries its addon (here an empty
// prebuilds/<platform>-<arch>/pty.node, see addon_present_setup.mjs): on Unix
// `spawn` is still the runtime's, so the child's output and exit arrive
// (DENEXT.md item 269). With the package's own code, as from 3.0.0-denext.13,
// node-pty 1.1.0's `spawn` returned a pid and never delivered either; the
// stand-in's own entry throws. `native` is the addon, which this empty file
// is not, so it reads `null`.
import process from "node:process";
import pty, { spawn } from "node-pty";

const timer = setTimeout(() => {
  console.log("timed out: no data or no exit");
  process.exit(2);
}, 10_000);
console.log("native", pty.native);
let out = "";
const term = spawn("sh", ["-c", "echo hi; exit 3"], { cols: 80, rows: 24 });
term.onData((data) => out += data);
term.onExit(({ exitCode, signal }) => {
  clearTimeout(timer);
  console.log(JSON.stringify(out), "exit", exitCode, signal);
});
