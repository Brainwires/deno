import { afterEach, expect, it, vi } from "vitest";

afterEach(() => {
  vi.doUnmock("./db.ts");
  vi.resetModules();
});

it("vi.doMock applies to later imports", async () => {
  vi.doMock("./db.ts", () => ({
    query: () => "do",
    name: "do",
    default: { kind: "do" },
  }));
  const { run } = await import("./service.ts");
  expect(run()).toBe("do|do|do");
});

it("vi.resetModules gives fresh module instances", async () => {
  const first = await import("./counter.ts");
  expect(first.next()).toBe(1);
  expect(first.next()).toBe(2);
  vi.resetModules();
  const second = await import("./counter.ts");
  expect(second).not.toBe(first);
  expect(second.next()).toBe(1);
});

it("vi.doUnmock restores the real module", async () => {
  const { run } = await import("./service.ts");
  expect(run()).toBe("real:x|real-db|real");
});

it("vi.importMock automocks a module", async () => {
  const math = await vi.importMock<typeof import("./math.ts")>("./math.ts");
  expect(vi.isMockFunction(math.add)).toBe(true);
  expect(math.add(1, 2)).toBeUndefined();
});
