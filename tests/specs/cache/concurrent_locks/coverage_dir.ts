// Two `deno test --coverage` runs on one directory take turns: the second
// would otherwise clear the directory while the first writes its coverage.
import { assert, deno } from "./helpers.ts";

const args = ["test", "--allow-read", "--coverage=cov", "wait_test.ts"];
const first = deno(...args);
await first.waitForStdout("running 1 test");

const second = deno(...args);
await second.waitForStderr(
  "Blocking waiting for file lock on coverage directory cov",
);
assert(!second.stdout.includes("running"), "the second run did not wait");

Deno.writeTextFileSync("release", "");
await first.success();
await second.success();
for (const run of [first, second]) {
  const output = run.stdout + run.stderr;
  assert(output.includes("covered.ts"), `no coverage report:\n${output}`);
}
console.log("ok");
