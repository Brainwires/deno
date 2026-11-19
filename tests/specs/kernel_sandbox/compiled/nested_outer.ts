// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// A compiled program baked with enforce, started by a confined `deno run`:
// Linux stacks its own layer (the narrower grant holds for native code);
// macOS can't stack a second Seatbelt profile, so the program refuses.
const out = new Deno.Command("./prog_files", {
  stdout: "piped",
  stderr: "piped",
})
  .outputSync();
console.log(`exit ${out.code}`);
console.log(new TextDecoder().decode(out.stdout).trim());
const err = new TextDecoder().decode(out.stderr);
if (err.includes("refuses to start")) {
  console.log(err.split("\n")[0].replace(/^error: /, ""));
}
