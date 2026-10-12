import { expect, it } from "vitest";

it("alpha inherits the root config", (ctx) => {
  expect(ctx.task.file.projectName ?? "").toBe("alpha");
  expect(ctx.task.timeout).toBe(1234);
});
