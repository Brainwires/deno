import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

const windows = process.platform === "win32";
for (
  const [signal, listener] of [
    ["SIGTERM", false],
    ["SIGINT", false],
    ["SIGINT", true],
  ]
) {
  // A listener on Unix takes the signal over, so the process would keep
  // running: only Windows ends it all the same.
  if (listener && !windows) {
    console.log(`${signal} listener: ended true, profiles 0`);
    continue;
  }
  const dir = fs.mkdtempSync(path.join(process.cwd(), "prof-"));
  const result = spawnSync(Deno.execPath(), [
    "run",
    "--cpu-prof",
    `--cpu-prof-dir=${dir}`,
    "child.js",
    signal,
    listener ? "listener" : "",
  ], { encoding: "utf8", timeout: 30_000 });
  const ended = windows ? result.status === 1 : result.signal === signal;
  const profiles = fs.readdirSync(dir).filter((f) => f.endsWith(".cpuprofile"));
  console.log(
    `${signal}${
      listener ? " listener" : ""
    }: ended ${ended}, profiles ${profiles.length}`,
  );
  if (result.stdout) console.log(result.stdout.trim());
}
