import { expect, it } from "vitest";
import { run } from "./service.ts";

it("not mocked in another file", () => {
  expect(run()).toBe("real:x|real-db|real");
});
