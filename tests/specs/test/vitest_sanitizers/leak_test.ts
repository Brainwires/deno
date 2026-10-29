import { describe, expect, test } from "vitest";

describe("leaks", () => {
  test("clears its timer", () => {
    const id = setTimeout(() => {}, 10_000);
    clearTimeout(id);
  });

  test("leaks a timer", () => {
    setTimeout(() => {}, 10_000);
    expect(1).toBe(1);
  });

  test("after the leak", () => {
    expect(2).toBe(2);
  });
});
