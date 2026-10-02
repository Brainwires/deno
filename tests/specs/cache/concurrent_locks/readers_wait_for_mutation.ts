// A command reading the cache (Shared) waits while it is being mutated.
import { assert, deno, holdLock, locksDir, runDeno } from "./helpers.ts";

await runDeno("cache", "local.ts");

// `deno clean` holds this exclusively while it deletes
const releaseMutation = holdLock(`${locksDir}/package-cache-mutate.lock`, true);
const check = deno("check", "local.ts");
await check.waitForStderr(
  "Blocking waiting for file lock on shared package cache",
);
assert(await check.isRunning(), "check did not wait for the mutation");

releaseMutation();
await check.success();
console.log("ok");
