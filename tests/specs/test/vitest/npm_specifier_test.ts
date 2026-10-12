import { expect, test } from "npm:vitest@^3";

test("npm:vitest resolves to the built-in module", () => {
  expect(typeof test.each).toBe("function");
});
