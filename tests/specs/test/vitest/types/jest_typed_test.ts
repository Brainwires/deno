// Type-checked against the built-in types of `@jest/globals`.
import { beforeEach, describe, expect, it, jest } from "@jest/globals";
import { store } from "./greeting.ts";

describe("jest typed", () => {
  let load: jest.SpiedFunction<typeof store.load>;
  beforeEach(() => {
    load = jest.spyOn(store, "load");
  });

  it("calls done", (done) => {
    setTimeout(done, 1);
  });

  it("types jest mocks", async () => {
    const add: jest.Mock<(a: number, b: number) => number> = jest.fn(
      (a: number, b: number) => a + b,
    );
    expect(add(1, 2)).toBe(3);
    load.mockResolvedValueOnce({ id: 2, name: "jest" });
    await expect(store.load(2)).resolves.toMatchObject({ name: "jest" });
    jest.restoreAllMocks();
  });
});
