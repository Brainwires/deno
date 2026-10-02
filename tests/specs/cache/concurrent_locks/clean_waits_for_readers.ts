// `deno clean` (MutateExclusive) waits for processes reading the cache.
import {
  assert,
  deno,
  denoDir,
  exists,
  holdLock,
  locksDir,
  runDeno,
} from "./helpers.ts";

await runDeno("cache", "http://localhost:4545/echo.ts");
assert(exists(`${denoDir}/remote`), "the cache was not populated");

// a `deno test` or `deno check` holds this shared while it runs
const releaseReader = holdLock(`${locksDir}/package-cache-mutate.lock`, false);
const clean = deno("clean");
await clean.waitForStderr(
  "Blocking waiting for file lock on package cache mutation",
);
assert(await clean.isRunning(), "clean did not wait for the reader");
assert(exists(`${denoDir}/remote`), "clean deleted the cache under a reader");

releaseReader();
await clean.success();
assert(!exists(`${denoDir}/remote`), "clean did not run after the reader");
assert(exists(locksDir), "clean deleted the lock files");
console.log("ok");
