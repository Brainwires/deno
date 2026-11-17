// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// A nested deno with --kernel-sandbox=enforce of its own: its layer stacks
// on the parent's, so native code is held to the narrower of the two.
const lib = Deno.dlopen(
  Deno.build.os === "darwin" ? "/usr/lib/libSystem.B.dylib" : "libc.so.6",
  {
    open: { parameters: ["buffer", "i32"], result: "i32" },
    close: { parameters: ["i32"], result: "i32" },
  },
);
const cstr = (s) => new TextEncoder().encode(s + "\0");
for (const path of ["inside/sub/b.txt", "inside/a.txt", "outside.txt"]) {
  const fd = lib.symbols.open(cstr(path), 0);
  console.log(`native read ${path}: ${fd >= 0 ? "ok" : "refused"}`);
  if (fd >= 0) lib.symbols.close(fd);
}
