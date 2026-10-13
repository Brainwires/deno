// Type-checked against the built-in types of `vitest` (it is not installed):
// mock factories, mocks, spies, fixtures and matchers are typed, so nothing
// here needs an explicit annotation.
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  type MockInstance,
  test,
  vi,
} from "vitest";
import { greet, prefix, store } from "./greeting.ts";

vi.mock(import("./greeting.ts"), async (importOriginal) => {
  const actual = await importOriginal();
  const prefix = actual.prefix.toUpperCase();
  // `name` is a string: the factory returns the module's exports
  return { ...actual, prefix, greet: (name) => `${prefix}, ${name}` };
});

const mocks = vi.hoisted(() => ({ double: vi.fn((n: number) => n * 2) }));

describe("typed", () => {
  let load: MockInstance<typeof store.load>;
  beforeEach(() => {
    load = vi.spyOn(store, "load");
  });
  afterEach(() => {
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  it("mocks a module with a typed factory", () => {
    expect(prefix).toBe("HELLO");
    expect(greet("Deno")).toBe("HELLO, Deno");
  });

  it("types mocks and spies", async () => {
    const add = vi.fn<(a: number, b: number) => number>();
    add.mockImplementation((a, b) => a + b).mockReturnValueOnce(0);
    expect(add(1, 2)).toBe(0);
    expect(add(1, 2)).toBe(3);
    expect(add).toHaveBeenCalledTimes(2);
    expect(add.mock.calls[0]).toEqual([1, 2]);
    expect(mocks.double(4)).toBe(8);
    load.mockResolvedValueOnce({ id: 1, name: "mocked" });
    await expect(store.load(1)).resolves.toEqual({ id: 1, name: "mocked" });
    expect(load.mock.lastCall?.[0]).toBe(1);
    const size = vi.spyOn(store, "size", "get").mockReturnValue(3);
    expect(store.size).toBe(3);
    expect(size).toHaveBeenCalledOnce();
    vi.mocked(mocks.double).mockReturnValue(0);
    expect(mocks.double(1)).toBe(0);
  });

  it("types fake timers", () => {
    vi.useFakeTimers();
    const tick = vi.fn();
    setTimeout(tick, 100);
    vi.advanceTimersByTime(100);
    expect(tick).toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });

  it.each([[1, 2], [2, 3]])("%i + 1 = %i", (a, b) => {
    expect(a + 1).toBe(b);
  });

  it.each([{ name: "a", length: 1 }])("$name", ({ name, length }) => {
    expect(name).toHaveLength(length);
  });
});

const withUser = test.extend<{ user: { name: string } }>({
  user: async (_context, use) => {
    await use({ name: "fixture" });
  },
});

withUser("gets a typed fixture", ({ user, expect }) => {
  expect(user.name.toUpperCase()).toBe("FIXTURE");
});
