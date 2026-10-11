import { afterEach, describe, expect, it, vi } from "vitest";

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.unstubAllEnvs();
  vi.restoreAllMocks();
});

describe("vi.fn", () => {
  it("records calls and results", () => {
    const fn = vi.fn((a: number, b: number) => a + b);
    expect(fn(1, 2)).toBe(3);
    fn.mockReturnValueOnce(10);
    expect(fn(1, 1)).toBe(10);
    expect(fn).toHaveBeenCalledTimes(2);
    expect(fn).toHaveBeenNthCalledWith(1, 1, 2);
    expect(fn).toHaveBeenLastCalledWith(1, 1);
    expect(fn.mock.results[0]).toEqual({ type: "return", value: 3 });
  });

  it("mockResolvedValue and implementation", async () => {
    const fn = vi.fn().mockResolvedValue("ok");
    await expect(fn()).resolves.toBe("ok");
    fn.mockImplementation(() => {
      throw new Error("boom");
    });
    expect(() => fn()).toThrow("boom");
    fn.mockReset();
    expect(fn()).toBeUndefined();
  });

  it("spyOn and restore", () => {
    const obj = { greet: (name: string) => `hi ${name}` };
    const spy = vi.spyOn(obj, "greet").mockReturnValue("mocked");
    expect(obj.greet("x")).toBe("mocked");
    expect(spy).toHaveBeenCalledWith("x");
    spy.mockRestore();
    expect(obj.greet("x")).toBe("hi x");
    expect(vi.isMockFunction(spy)).toBe(true);
  });
});

describe("fake timers", () => {
  it("advances timers", () => {
    vi.useFakeTimers();
    const fn = vi.fn();
    setTimeout(fn, 100);
    setInterval(fn, 1000);
    vi.advanceTimersByTime(100);
    expect(fn).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(2000);
    expect(fn).toHaveBeenCalledTimes(3);
    expect(vi.getTimerCount()).toBe(1);
    vi.clearAllTimers();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("runs all timers async and mocks the date", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2020-01-01T00:00:00Z"));
    expect(new Date().toISOString()).toBe("2020-01-01T00:00:00.000Z");
    let done = false;
    setTimeout(async () => {
      await Promise.resolve();
      done = true;
    }, 5000);
    await vi.runAllTimersAsync();
    expect(done).toBe(true);
    expect(Date.now()).toBe(Date.parse("2020-01-01T00:00:05Z"));
  });
});

describe("stubs", () => {
  it("stubs globals and env", () => {
    vi.stubGlobal("__answer", 42);
    expect((globalThis as Record<string, unknown>).__answer).toBe(42);
    vi.stubEnv("DENO_VITEST_FLAG", "on");
    expect(process.env.DENO_VITEST_FLAG).toBe("on");
    expect(import.meta.env.DENO_VITEST_FLAG).toBe("on");
  });

  it("restored them", () => {
    expect("__answer" in globalThis).toBe(false);
    expect(process.env.DENO_VITEST_FLAG).toBeUndefined();
  });
});

describe("waiting", () => {
  it("waitFor and waitUntil", async () => {
    let ready = false;
    setTimeout(() => (ready = true), 20);
    await vi.waitUntil(() => ready, { timeout: 1000, interval: 5 });
    let count = 0;
    await vi.waitFor(() => {
      count++;
      if (count < 3) throw new Error("not yet");
    }, { interval: 1 });
    expect(count).toBe(3);
  });

  it("hoisted and mocked", () => {
    const value = vi.hoisted(() => 7);
    expect(value).toBe(7);
    const fn = vi.fn();
    expect(vi.mocked(fn)).toBe(fn);
  });
});

it("import.meta.env defaults", () => {
  expect(import.meta.env.MODE).toBe("test");
  expect(import.meta.env.DEV).toBe(true);
  expect(import.meta.env.PROD).toBe(false);
  expect(import.meta.env.SSR).toBe(true);
  expect(process.env.VITEST).toBe("true");
});
