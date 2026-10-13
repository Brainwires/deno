import { expect, test, vi } from "vitest";
import { usesDep } from "./uses_dep.ts";

vi.mock("./dep.ts", () => ({ dep: () => 41 }));

test("a module whose import is mocked", () => {
  expect(usesDep(1)).toBe(42);
});
