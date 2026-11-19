// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// Windows, the small tier: an --allow-run child inherits the Low integrity
// level, so it can't write outside the grant either.
for (const file of ["inside\\child.txt", "child_outside.txt"]) {
  new Deno.Command("cmd", {
    args: ["/c", `echo x> ${file}`],
    stdout: "null",
    stderr: "null",
  }).outputSync();
  let made = true;
  try {
    Deno.statSync(file);
  } catch {
    made = false;
  }
  console.log(`child wrote ${file}: ${made}`);
}
