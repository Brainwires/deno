// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// Native code in the confined process holds no descriptor that writes a
// regular file (no write grant): nothing opened before the sandbox applied,
// such as a cache, is writable through it. The confined run's own caches in
// its policy's overlay (DENO_DIR/sandboxed/<hash>, item 448) are the one
// write the policy has, opened after the sandbox applied.
const lib = Deno.dlopen(
  `lib/libfds.${Deno.build.os === "darwin" ? "dylib" : "so"}`,
  { writable_files: { parameters: ["buffer", "i32"], result: "i32" } },
);
const buf = new Uint8Array(16384);
lib.symbols.writable_files(buf, buf.length);
const text = new TextDecoder().decode(buf.subarray(0, buf.indexOf(0)));
const outside = text.split("\n").filter((line) =>
  line && !/\/sandboxed\/[0-9a-f]{16}\//.test(line)
);
console.log(`writable files: ${outside.length}`);
if (outside.length) console.log(outside.join("\n"));
