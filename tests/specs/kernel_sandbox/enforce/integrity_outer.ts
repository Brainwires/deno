// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// P4 (and P3): nothing outside the write scope changes in content, size,
// mode or times, whatever native code tries (per Landlock ABI: where the
// kernel can't hold a rule, enforce refuses to start, which also changes
// nothing).
function stat() {
  const s = Deno.statSync("outside.txt");
  return JSON.stringify([
    Deno.readTextFileSync("outside.txt"),
    s.size,
    s.mode,
    s.mtime?.getTime(),
  ]);
}
const before = stat();
const o = new Deno.Command(Deno.execPath(), {
  args: [
    "run",
    "--quiet",
    "--kernel-sandbox=enforce",
    "--allow-ffi",
    "--allow-read=inside",
    "--allow-write=inside",
    "integrity.ts",
  ],
  stderr: "piped",
}).outputSync();
const refused = new TextDecoder().decode(o.stderr).includes(
  "refuses to start",
);
console.log(
  `outside unchanged: ${stat() === before}${
    refused ? " (enforce refused to start)" : ""
  }`,
);
