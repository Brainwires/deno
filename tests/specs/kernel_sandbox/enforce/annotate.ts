// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// A Deno op the kernel sandbox refuses carries the note: on Linux chmod by
// path (refused under enforce, inside the write scope too), on macOS a
// terminal that isn't this process's own (the profile allows only those;
// Deno's own check allows it under an unscoped --allow-write).
try {
  if (Deno.build.os === "linux") {
    Deno.chmodSync("inside/a.txt", 0o600);
  } else {
    Deno.openSync("/dev/tty", { write: true }).close();
  }
  console.log("allowed");
} catch (e) {
  console.log(`${(e as Error).name}: ${(e as Error).message}`);
}
