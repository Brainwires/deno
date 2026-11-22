// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// Confined, with a grant to `cc` only: Node's compile cache authenticates its
// entries with the per-user key in DENO_DIR, which the confined image can't
// read; the image before the re-exec hands it over.
import module from "node:module";
const { compileCacheStatus } = module.constants;
const { status, message } = module.enableCompileCache("cc");
const name = Object.keys(compileCacheStatus).find((key) =>
  compileCacheStatus[key] === status
);
console.log(`compile cache: ${name}${message ? ` (${message})` : ""}`);
