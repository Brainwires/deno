// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// P3 (g) as the model holds it: native dlopen of a library outside the READ
// scope is refused (Linux: Landlock read rights; macOS: Seatbelt, which
// doesn't hold loading to the --allow-ffi paths alone, so the model claims
// the read scope on both).
import { cstr, libc } from "./native.ts";

const paths = JSON.parse(Deno.readTextFileSync("lib/paths.json")) as Record<
  string,
  string
>;
// the one outside first, so nothing of the same image is loaded before it
for (const dir of ["other", "lib"]) {
  const handle = libc.symbols.dlopen(cstr(paths[dir]), 2 /* RTLD_NOW */);
  console.log(`dlopen ${dir}: ${handle === null ? "refused" : "ok"}`);
}
