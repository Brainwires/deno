// Copyright 2018-2026 the Deno authors. MIT license.
// out/ (the write allow) with keep/ (the write deny), an existing folder
// and file beside it, and a link in out/ that leads into the deny.
Deno.mkdirSync("out/keep", { recursive: true });
Deno.mkdirSync("out/sub", { recursive: true });
Deno.writeTextFileSync("out/keep/kept.txt", "kept");
Deno.writeTextFileSync("out/existing.txt", "existing");
try {
  Deno.removeSync("out/to-keep");
} catch {
  // not there yet
}
Deno.symlinkSync("keep", "out/to-keep");
for (const stale of ["out/new.txt", "out/moved.txt", "out/made"]) {
  try {
    Deno.removeSync(stale, { recursive: true });
  } catch {
    // not there
  }
}
