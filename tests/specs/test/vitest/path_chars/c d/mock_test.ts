import { expect, test, vi } from "vitest";
import { calc } from "./calc.ts";

// the test lives under a path with a space: the mock is the module
// the loader resolves `./double.ts` to
vi.mock("./double.ts", () => ({ double: () => 100 }));

test("a mock under a path with a space", async () => {
  expect(calc(1)).toBe(101);
  const actual = await vi.importActual<typeof import("./double.ts")>(
    "./double.ts",
  );
  expect(actual.double(2)).toBe(4);
});
