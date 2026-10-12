// Type-checked against the fallback types for `vitest` (it is not installed).
import { describe, expect, it, vi } from "vitest";

describe("typed", () => {
  it.each([[1, 2], [2, 3]])("%i + 1 = %i", (a, b) => {
    expect(a + 1).toBe(b);
  });
  it("spies", () => {
    const fn = vi.fn();
    fn();
    expect(fn).toHaveBeenCalledOnce();
  });
});
