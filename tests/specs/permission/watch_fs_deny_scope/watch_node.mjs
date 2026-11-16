// node:fs.watch recursive goes through the same watcher.
import fs from "node:fs";
const delay = (ms) => new Promise((r) => setTimeout(r, ms));
const seen = [];
let sawOk = false;
let resolveOk;
const okSeen = new Promise((r) => resolveOk = r);
const watcher = fs.watch("watched", { recursive: true }, (_event, name) => {
  if (name == null) return;
  seen.push(String(name));
  if (String(name).endsWith("ok.txt")) {
    sawOk = true;
    resolveOk();
  }
});
await delay(500);
fs.writeFileSync("watched/secret/hidden.txt", "x");
try {
  fs.lstatSync("watched/link");
  fs.writeFileSync("watched/link/hidden2.txt", "x");
} catch {
  // no link (the `nolink` setup)
}
await delay(500);
fs.writeFileSync("watched/ok/ok.txt", "x");
let timer;
await Promise.race([
  okSeen,
  new Promise((r) => timer = setTimeout(r, 10000)),
]);
clearTimeout(timer);
await delay(200);
watcher.close();
const reported = seen.filter((p) => p.includes("hidden"));
console.log(
  `reported: ${reported.length === 0 ? "none" : reported.join(", ")}`,
);
console.log(`saw the allowed change: ${sawOk}`);
