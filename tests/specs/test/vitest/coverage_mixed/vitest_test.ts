import { expect, test } from "vitest";
import { dep } from "./dep.ts";
import { kind, vitestOnly } from "./mixed.ts";

test("the Vitest half", () => {
  expect(kind(1)).toBe("positive");
  expect(vitestOnly()).toBe("vitest");
  expect(dep(-3)).toBe(3);
});
