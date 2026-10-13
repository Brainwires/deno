// A plain Deno test: run on its own, no module gets the Vitest transform.
import { dep } from "./dep.ts";
import { kind, plainOnly } from "./mixed.ts";

Deno.test("the plain half", () => {
  if (kind(-1) !== "negative" || plainOnly() !== "plain" || dep(2) !== 2) {
    throw new Error("unexpected");
  }
});
