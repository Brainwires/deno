import { expect, it, vi } from "vitest";

vi.mock("./some_module.ts", () => ({ value: 1 }));

it("never registered", () => {
  expect(true).toBe(true);
});
