import { describe, expect, it, vi } from "vitest";
import { run } from "./service.ts";
import { viaNested } from "./nested/consumer.ts";
import db, { name, query } from "./db.ts";

// Hoisted above the imports, like the mock below.
const mocks = vi.hoisted(() => ({
  query: vi.fn((sql: string) => `mock:${sql}`),
}));

vi.mock("./db.ts", () => ({
  query: mocks.query,
  name: "mock-db",
  default: { kind: "mock" },
}));

describe("vi.mock with a factory", () => {
  it("replaces the module for the test file", () => {
    expect(query("a")).toBe("mock:a");
    expect(name).toBe("mock-db");
    expect(db.kind).toBe("mock");
  });

  it("replaces the module for every importer", () => {
    expect(run()).toBe("mock:x|mock-db|mock");
    expect(viaNested()).toBe("mock:n");
    expect(mocks.query).toHaveBeenCalledWith("n");
  });

  it("vi.importActual returns the real module", async () => {
    const actual = await vi.importActual<typeof import("./db.ts")>(
      "../mocking/db.ts",
    );
    expect(actual.query("b")).toBe("real:b");
  });
});
