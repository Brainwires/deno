// The `jest` object.
import { hello } from "fake-pkg";
import { describeDep } from "../uses_dep.ts";
import * as dep from "../dep.ts";
import greet from "../greet.ts";

jest.mock("../dep.ts", () => ({
  ...jest.requireActual<typeof import("../dep.ts")>("../dep.ts"),
  value: jest.fn(() => "mocked dep"),
}));

// A second factory loading its original synchronously, which imports the
// first mock: each factory runs once.
jest.mock("../uses_dep.ts", () => ({
  ...jest.requireActual<typeof import("../uses_dep.ts")>("../uses_dep.ts"),
}));

// A factory's result is `module.exports` (Jest's CommonJS interop): here a
// function, which is the default export.
jest.mock("../greet.ts", () => () => "hi");

describe("jest object", () => {
  it("gives factory results CommonJS interop", () => {
    expect(greet()).toBe("hi");
  });

  it("hoists jest.mock above the imports", () => {
    expect(describeDep()).toBe("dep says mocked dep");
    expect(dep.other()).toBe("real other");
  });

  it("requireMock returns the registered mock", () => {
    const mocked = jest.requireMock<typeof import("../dep.ts")>("../dep.ts");
    expect(jest.isMockFunction(mocked.value)).toBe(true);
  });

  it("uses a root __mocks__ file for a package without jest.mock", () => {
    expect(hello()).toBe("mocked fake-pkg");
  });

  it("maps the fake timers API", () => {
    jest.useFakeTimers({ now: 1000, doNotFake: ["Date"] });
    let fired = 0;
    setTimeout(() => fired++, 50);
    setInterval(() => fired++, 100);
    jest.advanceTimersByTime(60);
    expect(fired).toBe(1);
    jest.runOnlyPendingTimers();
    expect(fired).toBe(2);
    expect(jest.getTimerCount()).toBe(1);
    jest.clearAllTimers();
    expect(Date.now()).toBeGreaterThan(1000);
    jest.useRealTimers();
    jest.useFakeTimers();
    jest.setSystemTime(new Date(2020, 0, 1));
    expect(new Date().getFullYear()).toBe(2020);
    expect(jest.getRealSystemTime()).toBeGreaterThan(1600000000000);
    jest.useRealTimers();
  });

  it("fakes process.nextTick and queueMicrotask like Jest", async () => {
    const { setImmediate: macrotask } = await import("node:timers");
    const real = {
      nextTick: process.nextTick,
      queueMicrotask: globalThis.queueMicrotask,
      Date: globalThis.Date,
    };
    jest.useFakeTimers();
    const calls: string[] = [];
    process.nextTick(() => calls.push("tick"));
    queueMicrotask(() => calls.push("microtask"));
    await new Promise((resolve) => macrotask(resolve));
    expect(calls).toEqual([]);
    jest.runAllTicks();
    expect(calls).toEqual(["tick", "microtask"]);
    jest.useRealTimers();
    expect(process.nextTick).toBe(real.nextTick);
    // doNotFake keeps them
    jest.useFakeTimers({ doNotFake: ["nextTick", "queueMicrotask"] });
    expect(process.nextTick).toBe(real.nextTick);
    expect(globalThis.queueMicrotask).toBe(real.queueMicrotask);
    jest.useRealTimers();
    // legacy timers fake the timer functions and nextTick only
    jest.useFakeTimers({ legacyFakeTimers: true });
    expect(process.nextTick).not.toBe(real.nextTick);
    expect(globalThis.queueMicrotask).toBe(real.queueMicrotask);
    expect(globalThis.Date).toBe(real.Date);
    jest.useRealTimers();
  });

  it("replaceProperty restores with restoreAllMocks", () => {
    const config = { mode: "prod" };
    jest.replaceProperty(config, "mode", "test");
    expect(config.mode).toBe("test");
    jest.restoreAllMocks();
    expect(config.mode).toBe("prod");
  });

  it("require() is Jest's: mocks, aliases, fresh modules", () => {
    expect(jest.isMockFunction(require("../dep.ts").value)).toBe(true);
    expect(require("@lib/math").square(4)).toBe(16);
    const first = require("../counter.ts");
    first.increment();
    expect(require("../counter.ts").count).toBe(1);
    jest.resetModules();
    expect(require("../counter.ts").count).toBe(0);
  });

  it("mocked and fn", () => {
    const fn = jest.fn<(n: number) => number>();
    jest.mocked(fn).mockReturnValue(3);
    expect(fn(1)).toBe(3);
    expect(fn).toBeCalledWith(1);
    expect(fn).lastCalledWith(1);
  });
});
