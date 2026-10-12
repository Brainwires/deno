import { expect, test, vi } from "vitest";
import { calc } from "./calc.ts";

vi.mock("./double.ts", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./double.ts")>();
  return { double: (x: number) => actual.double(x) + 1 };
});

test("calc with a partially mocked double", () => {
  expect(() => calc(-1)).toThrow("negative");
});
