// `declare module "vitest"` augments the built-in types: custom matchers
// registered with `expect.extend()` are typed.
import { expect, test } from "vitest";

interface CustomMatchers<R = unknown> {
  toBeFoo(): R;
}
declare module "vitest" {
  interface Matchers<T = any> extends CustomMatchers<T> {}
  interface Assertion<T> {
    toBeWithin(min: number, max: number): void;
  }
}

expect.extend({
  toBeFoo(received: unknown) {
    return { pass: received === "foo", message: () => `${received} is not foo` };
  },
  toBeWithin(received: number, min: number, max: number) {
    return { pass: received >= min && received <= max, message: () => "out of range" };
  },
});

test("custom matchers", () => {
  expect("foo").toBeFoo();
  expect(5).toBeWithin(1, 10);
  expect("bar").not.toBeFoo();
});
