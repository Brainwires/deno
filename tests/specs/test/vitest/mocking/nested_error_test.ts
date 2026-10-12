import { it, vi } from "vitest";

it("mocks inside a test", () => {
  vi.mock("./db.ts");
});
