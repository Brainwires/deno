import { describe, expect, it, jest } from "@jest/globals";

describe("@jest/globals", () => {
  it("maps jest onto vi", () => {
    const fn = jest.fn((n: number) => n + 1);
    expect(fn(1)).toBe(2);
    expect(fn).toHaveBeenCalledTimes(1);
    jest.useFakeTimers();
    let fired = false;
    setTimeout(() => (fired = true), 10);
    jest.advanceTimersByTime(10);
    expect(fired).toBe(true);
    jest.useRealTimers();
  });

  it("jest.requireMock automocks a module synchronously", () => {
    const math = jest.requireMock<typeof import("./mocking/math.ts")>(
      "./mocking/math.ts",
    );
    expect(jest.isMockFunction(math.add)).toBe(true);
    expect(math.add(1, 2)).toBeUndefined();
    expect(math.PI).toBe(3.14);
  });
});
