// kill(): the signal's number in onExit, exitCode 0, as node-pty reports a
// signalled child; pause() holds data back until resume().
const pty = require("node-pty");
const term = pty.spawn("sh", ["-c", "echo ready; exec sleep 30"], {
  cols: 80,
  rows: 24,
});
let out = "";
term.pause();
term.onData((data) => {
  out += data;
  if (out.includes("ready")) term.kill("SIGTERM");
});
term.onExit(({ exitCode, signal }) => {
  console.log(JSON.stringify(out), "exit", exitCode, signal);
});
setTimeout(() => {
  console.log("paused:", JSON.stringify(out));
  term.resume();
}, 300);
