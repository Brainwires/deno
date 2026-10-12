import { expect, test, vi } from "vitest";
import { calc } from "./calc.ts";

vi.mock("./double.ts", () => ({ double: () => 100 }));

test("calc with a mocked double", () => {
  expect(calc(1)).toBe("big");
});
