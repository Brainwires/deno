import { expect, it, vi } from "vitest";

// src/linked is a symlink to a directory outside the project (made by
// make_link.ts): the runner does not follow it without --allow-read.
it("does not follow a symlink out of the project", async () => {
  await expect(vi.importMock("./linked/secret.ts")).rejects.toThrow(
    // deno-lint-ignore no-explicit-any
    (globalThis as any).Deno.errors.NotCapable,
  );
});
