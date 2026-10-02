// Downloads (DownloadExclusive) wait for each other, but not for readers.
import { assert, deno, holdLock, locksDir, runDeno } from "./helpers.ts";

await runDeno("cache", "local.ts");

// another process downloading into the cache
const releaseDownload = holdLock(
  `${locksDir}/package-cache-download.lock`,
  true,
);
const download = deno("cache", "http://localhost:4545/subdir/print_hello.ts");
await download.waitForStderr("Blocking waiting for file lock on package cache");

// a reader with nothing to download is not held up by the download
const reader = deno("check", "local.ts");
await reader.success();
assert(
  !reader.stderr.includes("Blocking"),
  `the reader waited:\n${reader.stderr}`,
);
assert(await download.isRunning(), "the download did not wait");

releaseDownload();
await download.success();
console.log("ok");
