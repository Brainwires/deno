// Copyright 2018-2026 the Deno authors. MIT license.
// `Deno.test({ permissions })` narrows Deno's checks as before (API-level;
// the kernel holds the top-level grant).
Deno.test({ name: "narrowed", permissions: { read: false } }, () => {
  let name = "none";
  try {
    Deno.readTextFileSync("inside/a.txt");
  } catch (err) {
    name = (err as Error).name;
  }
  if (name !== "NotCapable") throw new Error(`expected NotCapable: ${name}`);
});

Deno.test({ name: "inherited", permissions: "inherit" }, () => {
  if (Deno.readTextFileSync("inside/a.txt").trim() !== "inside") {
    throw new Error("unexpected contents");
  }
});
