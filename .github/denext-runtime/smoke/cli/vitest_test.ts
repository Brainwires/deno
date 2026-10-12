// Copyright 2018-2026 the Deno authors. MIT license.
// A Vitest suite as written for Vitest, run unchanged by this fork's
// `deno test` (DENEXT.md item 23). The CLI smoke (cli_smoke.sh) runs it.
import { describe, expect, it, vi } from "vitest";

describe("denext deno CLI", () => {
  it("runs vitest-style tests", () => {
    const double = vi.fn((n: number) => n * 2);
    expect(double(21)).toBe(42);
    expect(double).toHaveBeenCalledWith(21);
  });

  it("awaits async tests", async () => {
    await expect(Promise.resolve("ok")).resolves.toBe("ok");
  });
});
