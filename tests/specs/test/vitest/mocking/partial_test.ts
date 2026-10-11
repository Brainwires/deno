import { expect, it, vi } from "vitest";
import { run } from "./service.ts";

vi.mock("./db.ts", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./db.ts")>();
  return { ...actual, name: "partial" };
});

it("a factory can extend the original module", () => {
  expect(run()).toBe("real:x|partial|real");
});
