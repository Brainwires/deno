// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// P6: the child-failure hint in a nested deno (confined by the sandbox it
// inherited, with no layer of its own).
const nested = new Deno.Command(Deno.execPath(), {
  args: ["run", "--quiet", "-A", "inside/fail.js"],
  stderr: "piped",
}).outputSync();
const stderr = new TextDecoder().decode(nested.stderr);
console.log(
  `nested hint: ${
    stderr.includes(
      "hint: a child process exited with code 3 inside the kernel sandbox",
    )
  }`,
);
