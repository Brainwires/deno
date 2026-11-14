// Copyright 2018-2026 the Deno authors. MIT license.

// node:fs.existsSync() / fs.exists() answer `false` for a special path
// (/proc, /dev, /sys) the read grant doesn't reach, as Node does, instead of
// throwing NotCapable: denoland/deno#37062. A path outside the grants, or in
// a deny scope, still throws.
import fs from "node:fs";

function existsAsync(path) {
  return new Promise((resolve) => fs.exists(path, resolve));
}

// `--sync-only`: fs.exists() has no way to report a refusal but an
// unhandled rejection, so the still-throwing cases check existsSync() alone.
const syncOnly = Deno.args[0] === "--sync-only";
for (const path of Deno.args.slice(syncOnly ? 1 : 0)) {
  let sync;
  try {
    sync = String(fs.existsSync(path));
  } catch (e) {
    sync = `threw ${e.name}`;
  }
  if (syncOnly) {
    console.log(`${path}: existsSync ${sync}`);
    continue;
  }
  const async = String(await existsAsync(path));
  console.log(`${path}: existsSync ${sync}, exists ${async}`);
}
