import pty, { spawn } from "node-pty";

console.log(typeof spawn, spawn === pty.spawn);
const term = spawn("sh", ["-c", "exit 3"], { cols: 80, rows: 24 });
term.onExit((e) => console.log("exit", e.exitCode, e.signal));
