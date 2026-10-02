// Concurrent writers of one deno.lock each add their entries: a write
// happens under a lock and merges in what was written since it was read.
import {
  artifactLockFiles,
  assert,
  denoIn,
  holdLock,
  runDenoIn,
} from "./helpers.ts";

const urls = {
  "a.ts": "http://localhost:4545/echo.ts",
  "b.ts": "http://localhost:4545/subdir/print_hello.ts",
  "c.ts": "http://localhost:4545/subdir/mod3.js",
};
Deno.mkdirSync("project");
Deno.writeTextFileSync("project/deno.json", "{}\n");
for (const [file, url] of Object.entries(urls)) {
  Deno.writeTextFileSync(`project/${file}`, `import "${url}";\n`);
}

const lockFiles = artifactLockFiles(
  await runDenoIn("project", "-L", "trace", "cache", "--allow-import", "a.ts"),
);
assert(lockFiles.length === 1, `expected one lockfile lock: ${lockFiles}`);

// b.ts and c.ts both read deno.lock (with a.ts's entry), then both wait to
// write it
const releaseLockfile = holdLock(lockFiles[0], true);
const b = denoIn("project", "cache", "--allow-import", "b.ts");
const c = denoIn("project", "cache", "--allow-import", "c.ts");
await b.waitForStderr("Blocking waiting for file lock on lockfile");
await c.waitForStderr("Blocking waiting for file lock on lockfile");

releaseLockfile();
await b.success();
await c.success();
const remote = JSON.parse(Deno.readTextFileSync("project/deno.lock")).remote;
for (const url of Object.values(urls)) {
  assert(url in remote, `deno.lock lost ${url}:\n${JSON.stringify(remote)}`);
}
console.log("ok");
