// Copyright 2018-2026 the Deno authors. MIT license.
// bout/ (the write allow) with keep/ (the write deny) and a folder beside it.
Deno.mkdirSync("bout/keep", { recursive: true });
Deno.mkdirSync("bout/sub", { recursive: true });
Deno.writeTextFileSync("bout/keep/kept.txt", "kept");
for (const stale of ["bout/new.txt", "bout/moved.txt", "bout/native-direct"]) {
  try {
    Deno.removeSync(stale);
  } catch {
    // not there
  }
}
