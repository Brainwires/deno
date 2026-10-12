// Jest's test API semantics on top of Vitest's runner.
const titles: string[] = [];

describe("jest api", () => {
  let hookCalls = 0;
  // A hook's return value (here a spy, a function) is not a teardown.
  beforeEach(() => jest.fn(() => hookCalls++));
  afterAll(() => {
    expect(hookCalls).toBe(0);
  });

  it("supports done callbacks", (done) => {
    setTimeout(() => {
      expect(1).toBe(1);
      done();
    }, 1);
  });

  it("supports done in hooks", () => {
    expect(doneHookRan).toBe(true);
  });

  test.each([[1, "a"], [2, { b: 1 }]])("each %p %p", (n, x) => {
    titles.push(expect.getState().currentTestName!);
    expect(n).toBeGreaterThan(0);
    expect(x).toBeDefined();
  });

  it.each([{ name: "x", n: 1 }])("each $name $n $#", ({ n }) => {
    titles.push(expect.getState().currentTestName!);
    expect(n).toBe(1);
  });

  it("formats each titles like jest-each", () => {
    expect(titles).toEqual([
      'jest api each 1 "a"',
      'jest api each 2 {"b": 1}',
      "jest api each x 1 0",
    ]);
  });

  xit("xit skips", () => {
    throw new Error("must not run");
  });

  test.failing("test.failing passes when the test fails", () => {
    expect(1).toBe(2);
  });
});

describe("retryTimes", () => {
  jest.retryTimes(1);
  let attempts = 0;
  it("retries a failing test", () => {
    attempts++;
    expect(attempts).toBe(2);
  });
});

let doneHookRan = false;
beforeAll((done) => {
  doneHookRan = true;
  done();
});
