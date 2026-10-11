import { expect, it, vi } from "vitest";
import { run } from "./service.ts";

vi.mock("./db.ts", () => ({ query: () => "a", name: "a", default: {} }));

it("mocked in this file", () => {
  expect(run()).toBe("a|a|undefined");
});
