// Runs without --allow-env (or any permission): Vitest's own runtime reads
// its environment variables (FORCE_TTY, CI, NODE_ENV, ...) without a check.
import process from "node:process";
import { describe, expect, test, vi } from "vitest";
import { plusOne } from "./mod.ts";

vi.mock("./dep.ts", () => ({ dep: () => 2 }));

describe("without permissions", () => {
  test("mocks, matchers and inline snapshots", () => {
    expect(plusOne()).toBe(3);
    expect({ list: [1, { a: "b" }] }).toEqual({ list: [1, { a: "b" }] });
    expect({ a: 1 }).toMatchInlineSnapshot(`
      {
        "a": 1,
      }
    `);
  });

  test("a failed assertion is formatted", () => {
    expect(() => expect({ a: 1 }).toEqual({ a: 2 })).toThrow(
      /expected \{ a: 1 \} to deeply equal \{ a: 2 \}/,
    );
  });

  test("the test's own environment reads stay checked", () => {
    expect(() => process.env.FORCE_TTY).toThrow(/Requires env access/);
  });
});
