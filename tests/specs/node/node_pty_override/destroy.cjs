// destroy(): the terminal's master closes at once, as node-pty's socket
// does, so `close` arrives even while a child that ignores SIGHUP still runs
// (it exits on its own a few seconds later).
const pty = require("node-pty");
const term = pty.spawn("sh", ["-c", "trap '' HUP; echo ready; sleep 3"], {
  cols: 80,
  rows: 24,
});
let out = "";
let destroyedAt;
term.onData((data) => {
  out += data;
  if (destroyedAt === undefined && out.includes("ready")) {
    destroyedAt = Date.now();
    term.destroy();
  }
});
term.on("close", () => {
  console.log(
    "closed before the child exited:",
    Date.now() - destroyedAt < 2000,
  );
});
term.onExit(() => console.log("exit"));
