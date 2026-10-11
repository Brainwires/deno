import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  onTestFinished,
  test,
} from "vitest";

const events: string[] = [];

beforeAll(() => {
  events.push("beforeAll");
  return () => events.push("beforeAll cleanup");
});
afterAll(() => {
  events.push("afterAll");
  console.log(events.join("\n"));
});

describe("outer", () => {
  beforeEach((ctx) => {
    events.push(`beforeEach ${ctx.task.name}`);
    return () => events.push(`beforeEach cleanup ${ctx.task.name}`);
  });
  afterEach((ctx) => {
    events.push(`afterEach ${ctx.task.name}`);
  });

  it("first", () => {
    onTestFinished(() => events.push("onTestFinished first"));
    expect(1 + 1).toBe(2);
  });

  describe("inner", () => {
    beforeAll(() => events.push("inner beforeAll"));
    afterAll(() => events.push("inner afterAll"));
    test("second", () => {
      events.push("second body");
    });
  });

  it.skip("skipped", () => {
    throw new Error("must not run");
  });
  it.todo("todo");
  it.skipIf(true)("skipIf true", () => {
    throw new Error("must not run");
  });
  it.runIf(true)("runIf true", () => {});
  it.fails("fails as expected", () => {
    expect(1).toBe(2);
  });
});

describe.each([
  { a: 1, b: 2, sum: 3 },
  { a: 2, b: 3, sum: 5 },
])("add($a, $b)", ({ a, b, sum }) => {
  it("sums", () => {
    expect(a + b).toBe(sum);
  });
});

it.each([[1, 1], [2, 4], [3, 9]])("square %i = %i", (n, square) => {
  expect(n * n).toBe(square);
});

test.for([
  ["a", "A"],
  ["b", "B"],
])("upper %s", ([lower, upper]) => {
  expect(lower.toUpperCase()).toBe(upper);
});

it.each`
  a    | b    | expected
  ${1} | ${1} | ${2}
  ${2} | ${3} | ${5}
`("template $a + $b", ({ a, b, expected }) => {
  expect(a + b).toBe(expected);
});

describe.concurrent("concurrent", () => {
  it("one", async () => {
    await new Promise((r) => setTimeout(r, 5));
  });
  it("two", async () => {
    await new Promise((r) => setTimeout(r, 1));
  });
});

let attempts = 0;
it("retries", { retry: 2 }, () => {
  attempts++;
  expect(attempts).toBe(3);
});

it("context skip", (ctx) => {
  ctx.skip();
});
