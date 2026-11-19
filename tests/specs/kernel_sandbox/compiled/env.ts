// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// A confined compiled program's environment holds only what its grant
// names (and the runtime's own short list): a child sees the same.
const out = new Deno.Command("env", { stdout: "piped" }).outputSync();
const names = new TextDecoder().decode(out.stdout).split("\n")
  .map((line) => line.split("=")[0]);
for (const name of ["P9_KEEP", "P9_SECRET"]) {
  console.log(`${name}: ${names.includes(name) ? "present" : "absent"}`);
}
