// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// Under a baked enforce the set is fixed: a permission that isn't granted
// is refused, never asked for.
try {
  Deno.readTextFileSync("outside.txt");
  console.log("read");
} catch (err) {
  console.log((err as Error).name);
}
console.log((await Deno.permissions.request({ name: "env" })).state);
