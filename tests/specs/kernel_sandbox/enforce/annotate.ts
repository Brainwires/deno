// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// A Deno op the kernel sandbox refuses carries the note: on Linux chmod by
// path (refused under enforce, inside the write scope too), on macOS
// reading /dev/tty when no standard stream is a terminal (Deno's own check
// lets a read grant open it for reading; the profile allows only the
// standard streams' terminals).
try {
  if (Deno.build.os === "linux") {
    Deno.chmodSync("inside/a.txt", 0o600);
  } else {
    Deno.openSync("/dev/tty").close();
  }
  console.log("allowed");
} catch (e) {
  console.log(`${(e as Error).name}: ${(e as Error).message}`);
}
