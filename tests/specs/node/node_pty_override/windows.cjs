// Windows: node-pty's spawn runs on the runtime's ConPTY (DENEXT.md item
// 259). The output is a console's rendering (VT sequences around the text),
// so only the text is checked.
const pty = require("node-pty");

const term = pty.spawn("cmd.exe", ["/d", "/c", "echo hi from conpty"], {
  cols: 80,
  rows: 24,
});
console.log("pid", term.pid > 0);
let out = "";
term.onData((data) => out += data);
term.onExit(({ exitCode }) => {
  console.log("saw the output", out.includes("hi from conpty"));
  console.log("exit", exitCode);
});
