// The built-in `vitest` types catch these errors.
import { expect, it, vi } from "vitest";
import { store } from "./greeting.ts";

vi.mock(import("./greeting.ts"), () => ({ prefix: 1 }));

it("is wrongly typed", () => {
  const add = vi.fn((a: number, b: number) => a + b);
  add.mockReturnValue("three");
  vi.spyOn(store, "save");
  expect(1).toBeGreaterThan("0");
});
