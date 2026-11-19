// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// A Deno op the kernel sandbox refuses carries the note: on Linux reading a
// file made after start in a folder whose read allow is expanded around a
// deny (the kernel has no rule for the new file; Deno's check allows it), on
// macOS reading /dev/tty when no standard stream is a terminal (Deno's own
// check lets a read grant open it for reading; the profile allows only the
// standard streams' terminals).
try {
  if (Deno.build.os === "linux") {
    Deno.writeTextFileSync("inside/new.txt", "x");
    Deno.readTextFileSync("inside/new.txt");
  } else {
    Deno.openSync("/dev/tty").close();
  }
  console.log("allowed");
} catch (e) {
  console.log(`${(e as Error).name}: ${(e as Error).message}`);
}
