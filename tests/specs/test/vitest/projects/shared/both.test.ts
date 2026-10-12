import { expect, it } from "vitest";

// Included by both projects: Vitest runs it once in each.
it("runs in each project", (ctx) => {
  expect(["alpha", "beta"]).toContain(ctx.task.file.projectName);
});
