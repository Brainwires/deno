// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// A nested deno (started with -A by a confined parent) reads through Deno's
// own API: inside the parent's grant it works; outside it Deno's check
// refuses with NotCapable before the kernel would (a bare EACCES). A
// TypeScript module: the nested deno transpiles it, on a stack it can't
// measure through /proc/self.
const label = Deno.args[0] ?? "";
for (
  const [name, path] of [["inside", "inside/a.txt"], ["outside", "outside.txt"]]
) {
  try {
    Deno.readTextFileSync(path);
    console.log(`${label}${name}: ok`);
  } catch (err) {
    console.log(`${label}${name}: ${(err as Error).name}`);
  }
}
