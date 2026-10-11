import { expect, it, vi } from "vitest";
import * as math from "./math.ts";
import { addTwice } from "./math_user.ts";

vi.mock("./math.ts", () => ({ add: (a: number, b: number) => a + b }));

it("spies on an export of a mocked module", () => {
  expect(addTwice(2)).toBe(4);
  vi.spyOn(math, "add").mockReturnValue(42);
  expect(addTwice(2)).toBe(42);
  expect(math.add).toHaveBeenCalledWith(2, 2);
});
