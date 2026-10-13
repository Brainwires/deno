// `globals: true`: the test API is global, typed by `vitest/globals`.
/// <reference types="vitest/globals" />
import type { Calls } from "./calls.ts";

describe("globals", () => {
  let calls: Calls = 0;
  beforeEach(() => {
    calls++;
  });

  it("needs no import", () => {
    const add = vi.fn((a: number, b: number) => a + b);
    expect(add(1, 2)).toBe(3);
    expect(add).toHaveBeenCalledWith(1, 2);
    expect(calls).toBe(1);
  });
});
