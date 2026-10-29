import { afterEach, describe, expect, test, vi } from "vitest";

// Vitest's fake clock is not the test's timer: none of these is a leak.
describe("fake timers", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  test("leaves fake timers queued", () => {
    vi.useFakeTimers();
    setTimeout(() => {}, 10_000);
    setInterval(() => {}, 10);
    vi.advanceTimersByTime(50);
    expect(vi.getTimerCount()).toBe(2);
  });

  test("advances asynchronously", async () => {
    vi.useFakeTimers();
    const done = new Promise((resolve) => setTimeout(resolve, 1_000));
    await vi.advanceTimersByTimeAsync(1_000);
    await done;
  });

  test("ticks on its own (nextTimerAsync)", async () => {
    vi.useFakeTimers();
    vi.setTimerTickMode("nextTimerAsync");
    await new Promise((resolve) => setTimeout(resolve, 5_000));
  });

  test("ticks on its own (interval)", async () => {
    vi.useFakeTimers();
    vi.setTimerTickMode("interval", 5);
    await new Promise((resolve) => setTimeout(resolve, 10));
  });
});

// Vitest keeps the clock installed until `vi.useRealTimers()`.
describe("a clock left installed across tests", () => {
  test("installs a clock that advances with real time", () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });

  test("still uses it", async () => {
    const start = Date.now();
    await new Promise((resolve) => setTimeout(resolve, 30));
    expect(Date.now()).toBeGreaterThan(start);
  });

  test("uninstalls it", () => {
    vi.useRealTimers();
  });
});
