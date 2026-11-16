// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// Native code in the confined process holds no descriptor that writes a
// regular file (no write grant): nothing opened before the sandbox applied,
// such as a cache, is writable through it.
const lib = Deno.dlopen(
  `lib/libfds.${Deno.build.os === "darwin" ? "dylib" : "so"}`,
  { writable_files: { parameters: ["buffer", "i32"], result: "i32" } },
);
const buf = new Uint8Array(16384);
const count = lib.symbols.writable_files(buf, buf.length);
console.log(`writable files: ${count}`);
const text = new TextDecoder().decode(buf.subarray(0, buf.indexOf(0)));
if (text) console.log(text.trimEnd());
