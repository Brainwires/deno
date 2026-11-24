// Copyright 2018-2026 the Deno authors. MIT license.
// Builds the two programs of this test: `bin/helper` (on the run list: it
// makes one exec call in the form its first argument names) and `bin/other`
// (not on it).
const cc = Deno.env.get("CC") ?? "cc";
Deno.mkdirSync("bin", { recursive: true });
for (const name of ["helper", "other"]) {
  const built = new Deno.Command(cc, {
    args: ["-O1", "-w", "-o", `bin/${name}`, `${name}.c`],
  }).outputSync();
  if (!built.success) {
    throw new Error(
      `cannot build ${name}.c with ${cc}: ${
        new TextDecoder().decode(built.stderr)
      }`,
    );
  }
}
