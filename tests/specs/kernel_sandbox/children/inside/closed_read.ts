// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// A nested deno whose grants are unknown: every read is refused by Deno's
// own check (NotCapable), inside the parent's grant too, and the error says
// why.
const label = Deno.args[0] ?? "";
for (
  const [name, path] of [["inside", "inside/a.txt"], ["outside", "outside.txt"]]
) {
  try {
    Deno.readTextFileSync(path);
    console.log(`${label}${name}: ok`);
  } catch (err) {
    const e = err as Error;
    console.log(
      `${label}${name}: ${e.name}${
        e.message.includes("grants unknown") ? " (grants unknown)" : ""
      }`,
    );
  }
}
