import { describe, expect, it } from "vitest";

describe("failures", () => {
  it("equality diff", () => {
    expect({ name: "deno", tags: ["a", "b"] }).toEqual({
      name: "deno",
      tags: ["a", "c"],
    });
  });

  it("soft assertions", () => {
    expect.soft(1).toBe(2);
    expect.soft("a").toBe("b");
  });

  it("assertion count", () => {
    expect.assertions(2);
    expect(true).toBe(true);
  });

  it("timeout", { timeout: 50 }, async () => {
    await new Promise((r) => setTimeout(r, 1000));
  });

  it("fails() that passes", () => {
    // passing test marked as fails -> fails
  });
  it.fails("marked fails", () => {});
});
