import { fork } from "node:child_process";
import process from "node:process";

console.log(JSON.stringify(process.execArgv));
const child = fork(import.meta.dirname + "/child.mjs");
child.on("message", (message) => {
  console.log(JSON.stringify(message));
});
