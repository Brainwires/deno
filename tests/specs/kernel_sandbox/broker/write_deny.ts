// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// --allow-write=out --deny-write=out/keep under the broker.
import { check, cstr, libc } from "./native.ts";

function attempt(label: string, fn: () => void) {
  try {
    fn();
    console.log(`${label}: ok`);
  } catch (err) {
    console.log(`${label}: ${(err as Error).name}`);
  }
}

// Deno's own operations directly in the allowed folder
attempt("deno create", () => Deno.writeTextFileSync("out/new.txt", "new"));
attempt("deno rename", () => Deno.renameSync("out/new.txt", "out/moved.txt"));
attempt("deno mkdir", () => Deno.mkdirSync("out/made"));
// a folder made after the sandbox applied
attempt(
  "deno create in the new folder",
  () => Deno.writeTextFileSync("out/made/f.txt", "f"),
);
attempt(
  "deno overwrite",
  () => Deno.writeTextFileSync("out/existing.txt", "changed"),
);
attempt("deno truncate", () => Deno.truncateSync("out/existing.txt", 3));
attempt("deno chmod", () => Deno.chmodSync("out/existing.txt", 0o600));
attempt("deno remove", () => Deno.removeSync("out/moved.txt"));
attempt(
  "deno remove tree",
  () => Deno.removeSync("out/made", { recursive: true }),
);
attempt(
  "deno create in an existing folder",
  () => Deno.writeTextFileSync("out/sub/s.txt", "s"),
);
console.log("content:", Deno.readTextFileSync("out/existing.txt"));

// the deny holds for Deno's operations, by name and through a link
attempt(
  "deno create in the deny",
  () => Deno.writeTextFileSync("out/keep/x.txt", "x"),
);
attempt(
  "deno create through a link into the deny",
  () => Deno.writeTextFileSync("out/to-keep/x.txt", "x"),
);
attempt(
  "deno overwrite in the deny",
  () => Deno.writeTextFileSync("out/keep/kept.txt", "x"),
);
attempt("deno remove in the deny", () => Deno.removeSync("out/keep/kept.txt"));
attempt(
  "deno rename into the deny",
  () => Deno.renameSync("out/existing.txt", "out/keep/in.txt"),
);
attempt("deno rename the deny", () => Deno.renameSync("out/keep", "out/gone"));

// native code: the kernel alone
check(
  "native create in the deny",
  libc.symbols.creat(cstr("out/keep/n"), 0o600),
);
check(
  "native create through a link into the deny",
  libc.symbols.creat(cstr("out/to-keep/n"), 0o600),
);
check(
  "native unlink in the deny",
  libc.symbols.unlink(cstr("out/keep/kept.txt")),
);
check(
  "native create in an existing folder",
  libc.symbols.creat(cstr("out/sub/n"), 0o600),
);
// directly in the allowed folder: Linux refuses native code there (the
// documented cost of holding the deny); macOS allows it
{
  const fd = libc.symbols.creat(cstr("out/native-direct"), 0o600);
  const refused = fd < 0;
  const expected = Deno.build.os === "linux";
  console.log(
    `native create directly in the allowed folder: ${
      refused === expected ? "as documented" : refused ? "refused" : "ok"
    }`,
  );
}
console.log("kept:", Deno.readTextFileSync("out/keep/kept.txt"));
