// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// A deny ceiling (D2: -A with --deny-read / --deny-write under auto at R4):
// native code and a child are refused the denied folder and nothing else.
import { check, cstr, libc, openRead } from "./native.ts";

openRead("native read outside the deny", "open.txt");
openRead("native read of the deny", "secret/key.txt");
check(
  "native create in the deny",
  libc.symbols.creat(cstr("secret/new.txt"), 0o600),
);
check("native create elsewhere", libc.symbols.creat(cstr("made.txt"), 0o600));
function cat(path: string): string {
  const out = new Deno.Command("cat", {
    args: [path],
    stdout: "null",
    stderr: "null",
  }).outputSync();
  return out.success ? "ok" : "refused";
}
console.log(`child read of the deny: ${cat("secret/key.txt")}`);
console.log(`child read elsewhere: ${cat("open.txt")}`);
// nothing else changes: sockets, the environment
const fd = libc.symbols.socket(2, 1, 0);
check("native socket", fd);
if (fd >= 0) libc.symbols.close(fd);
console.log(`env kept: ${Deno.env.get("DENY_ONLY_PROBE") ?? "gone"}`);
