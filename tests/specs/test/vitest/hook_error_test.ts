import { afterAll, beforeAll, describe, it } from "vitest";

describe("broken setup", () => {
  beforeAll(() => {
    throw new Error("setup exploded");
  });
  it("a", () => {});
  it("b", () => {});
});

describe("broken teardown", () => {
  afterAll(() => {
    throw new Error("teardown exploded");
  });
  it("c", () => {});
});

it("after", () => {});
