// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// P3 (b): an --allow-run child inherits the rules.
for (const file of ["inside/a.txt", "outside.txt"]) {
  const out = new Deno.Command("cat", {
    args: [file],
    stdout: "null",
    stderr: "null",
  }).outputSync();
  console.log(`cat ${file}: exit ${out.code}`);
}
