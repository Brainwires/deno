import { spawn } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const serialization = Deno.args[0];
const dir = mkdtempSync(join(tmpdir(), "ipc-watch-"));
const childPath = join(dir, "child.mjs");
const childSource = (run) =>
  `process.send({ run: ${run}, hasSend: typeof process.send });\n` +
  `setInterval(() => {}, 1000);\n`;
writeFileSync(childPath, childSource(1));

const child = spawn(
  Deno.execPath(),
  ["run", "--watch", "--no-clear-screen", "-A", childPath],
  { stdio: ["ignore", "pipe", "pipe", "ipc"], serialization },
);
let output = "";
child.stdout.on("data", (d) => output += d);
child.stderr.on("data", (d) => output += d);

const timeout = setTimeout(() => {
  console.log("timed out waiting for the restarted run's message");
  console.log(output);
  child.kill("SIGKILL");
  Deno.exit(1);
}, 30_000);

child.on("message", (msg) => {
  console.log(`message from run ${msg.run}`);
  if (msg.run === 1) {
    // Leave the watcher time to watch the file before changing it.
    setTimeout(() => writeFileSync(childPath, childSource(2)), 1000);
  } else {
    clearTimeout(timeout);
    child.kill("SIGKILL");
  }
});
