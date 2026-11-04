// The compile cache refuses a directory others can write to, and the test's
// temporary directory takes the source directory's mode (group-writable under
// umask 002). Evaluated before add.cjs, the first CommonJS module compiled.
import { chmodSync } from "node:fs";
import process from "node:process";

if (process.platform !== "win32") chmodSync(".", 0o700);
