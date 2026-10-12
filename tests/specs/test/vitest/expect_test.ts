import { assert, chai, describe, expect, it } from "vitest";

describe("matchers", () => {
  it("equality and truthiness", () => {
    expect(1).toBe(1);
    expect({ a: [1, { b: 2 }] }).toEqual({ a: [1, { b: 2 }] });
    expect({ a: 1, b: undefined }).toEqual({ a: 1 });
    expect({ a: 1, b: undefined }).not.toStrictEqual({ a: 1 });
    expect(null).toBeNull();
    expect(undefined).toBeUndefined();
    expect(0).toBeFalsy();
    expect("x").toBeTruthy();
    expect(NaN).toBeNaN();
    expect(0.1 + 0.2).toBeCloseTo(0.3);
    expect(5).toBeGreaterThan(4);
    expect(5).toBeLessThanOrEqual(5);
    expect("hello world").toMatch(/world/);
    expect("hello").toContain("ell");
    expect([1, 2, 3]).toContain(2);
    expect([{ a: 1 }]).toContainEqual({ a: 1 });
    expect([1, 2]).toHaveLength(2);
    expect({ a: { b: 1 } }).toHaveProperty("a.b", 1);
    expect({ a: 1, b: 2 }).toMatchObject({ a: 1 });
    expect(new Date(0)).toBeInstanceOf(Date);
    expect("s").toBeTypeOf("string");
    expect(3).toSatisfy((n: number) => n % 2 === 1);
    expect(2).toBeOneOf([1, 2]);
  });

  it("asymmetric matchers", () => {
    expect({ id: 1, name: "x", extra: true }).toEqual(
      expect.objectContaining({ id: expect.any(Number), name: "x" }),
    );
    expect([1, 2, 3]).toEqual(expect.arrayContaining([3, 1]));
    expect("deno test").toEqual(expect.stringContaining("test"));
    expect("deno test").toEqual(expect.stringMatching(/^deno/));
    expect({ a: 1 }).toEqual({ a: expect.anything() });
    expect(1.0001).toEqual(expect.closeTo(1, 3));
    expect({ x: 1 }).toEqual(expect.not.objectContaining({ y: 1 }));
  });

  it("errors and promises", async () => {
    const thrower = () => {
      throw new TypeError("bad input");
    };
    expect(thrower).toThrow();
    expect(thrower).toThrow("bad");
    expect(thrower).toThrow(/input/);
    expect(thrower).toThrowError(TypeError);
    await expect(Promise.resolve(1)).resolves.toBe(1);
    await expect(Promise.reject(new Error("no"))).rejects.toThrow("no");
  });

  it("extend", () => {
    expect.extend({
      toBeEven(received: number) {
        return {
          pass: received % 2 === 0,
          message: () => `expected ${received} to be even`,
        };
      },
    });
    // deno-lint-ignore no-explicit-any
    (expect(4) as any).toBeEven();
    // deno-lint-ignore no-explicit-any
    (expect(3) as any).not.toBeEven();
  });

  it("assertion counting", () => {
    expect.assertions(1);
    expect.hasAssertions();
    expect(1).toBe(1);
  });

  it("poll", async () => {
    let n = 0;
    await expect.poll(() => ++n, { interval: 1 }).toBe(3);
  });

  it("chai style and assert", () => {
    expect(1).to.equal(1);
    expect([1, 2]).to.have.lengthOf(2);
    assert.equal(1, 1);
    assert.deepEqual({ a: 1 }, { a: 1 });
    chai.expect(true).to.be.true;
  });
});
