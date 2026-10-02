// Two `deno compile`s to one output take turns: the second would otherwise
// delete the first's temporary file or race it to the final rename.
import {
  artifactLockFiles,
  assert,
  deno,
  holdLock,
  runDeno,
} from "./helpers.ts";

const args = ["compile", "--output", "out", "hello.ts"];
const lockFiles = artifactLockFiles(await runDeno("-L", "trace", ...args));
assert(lockFiles.length === 1, `expected one output lock: ${lockFiles}`);

// another `deno compile` writing the same output
const releaseOutput = holdLock(lockFiles[0], true);
const compile = deno(...args);
await compile.waitForStderr("Blocking waiting for file lock on output file");
assert(await compile.isRunning(), "compile did not wait");

releaseOutput();
await compile.success();
const out = new Deno.Command(
  Deno.build.os === "windows" ? "./out.exe" : "./out",
).outputSync();
assert(
  new TextDecoder().decode(out.stdout) === "hello\n",
  "the compiled binary does not work",
);
console.log("ok");
