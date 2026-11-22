// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// Compiled with --kernel-sandbox=enforce --allow-ffi --allow-read=.
// --allow-write=bout --deny-write=bout/keep: the program runs as the
// confined child of its own supervisor.
import { check, cstr, libc } from "./native.ts";

function attempt(label: string, fn: () => void) {
  try {
    fn();
    console.log(`${label}: ok`);
  } catch (err) {
    console.log(`${label}: ${(err as Error).name}`);
  }
}

// the program's own operations directly in the allowed folder
attempt("create", () => Deno.writeTextFileSync("bout/new.txt", "new"));
attempt("rename", () => Deno.renameSync("bout/new.txt", "bout/moved.txt"));
attempt("remove", () => Deno.removeSync("bout/moved.txt"));
// the deny holds for them
attempt(
  "create in the deny",
  () => Deno.writeTextFileSync("bout/keep/x.txt", "x"),
);
attempt("remove in the deny", () => Deno.removeSync("bout/keep/kept.txt"));
// native code: the kernel alone
check(
  "native create in the deny",
  libc.symbols.creat(cstr("bout/keep/n"), 0o600),
);
check(
  "native create in an existing folder",
  libc.symbols.creat(cstr("bout/sub/n"), 0o600),
);
// directly in the allowed folder: Linux refuses native code there (the
// documented cost of holding the deny); macOS allows it
{
  const fd = libc.symbols.creat(cstr("bout/native-direct"), 0o600);
  const refused = fd < 0;
  const expected = Deno.build.os === "linux";
  console.log(
    `native create directly in the allowed folder: ${
      refused === expected ? "as documented" : refused ? "refused" : "ok"
    }`,
  );
}
console.log("kept:", Deno.readTextFileSync("bout/keep/kept.txt"));
