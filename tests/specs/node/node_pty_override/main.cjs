// node-pty's API, served over Deno.openPty: spawn, the disposable onData /
// onExit events, the legacy emitter, write, resize, pid / cols / rows /
// process, kill.
const pty = require("node-pty");

console.log(
  typeof pty.spawn,
  typeof pty.fork,
  typeof pty.createTerminal,
  pty.native,
);

const term = pty.spawn("sh", [
  "-c",
  "stty size; read line; echo got:$line; echo TERM=$TERM; exit 7",
], {
  name: "xterm-color",
  cols: 100,
  rows: 30,
  cwd: process.cwd(),
  env: process.env,
});
console.log(
  "pid",
  term.pid > 0,
  "size",
  term.cols,
  term.rows,
  "process",
  term.process,
);

let out = "";
let legacy = "";
term.on("data", (data) => legacy += data);
let sent = false;
const sub = term.onData((data) => {
  out += data;
  if (!sent && out.includes("30 100")) {
    sent = true;
    sub.dispose();
    term.onData((data) => out += data);
    term.resize(120, 40);
    term.write("hello\r");
  }
});
term.onExit(({ exitCode, signal }) => {
  console.log(JSON.stringify(out));
  console.log("legacy emitter saw the same", legacy === out);
  console.log("exit", exitCode, signal, "size", term.cols, term.rows);
  console.log("handleFlowControl", term.handleFlowControl);
  term.kill(); // swallowed, as in node-pty: the child is gone
});
