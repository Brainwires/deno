// Copyright 2018-2026 the Deno authors. MIT license.

// Run by fd_test.js with --allow-ffi scoped to the addon: loading it works,
// reaching the descriptor it opens through node:fs does not.
import fs from "node:fs";
import process from "node:process";

const module = {};
process.dlopen(module, Deno.args[0], 0);
const [readFd] = module.exports.openPipe("scoped");
let line;
try {
  fs.fstatSync(readFd);
  line = "not refused";
} catch (e) {
  line = `refused ${e.name} ${e.message}`;
}
// deno-lint-ignore no-console
console.log(line);
