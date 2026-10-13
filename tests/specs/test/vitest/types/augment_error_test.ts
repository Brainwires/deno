// Augmented matchers are checked like the built-in ones.
import { expect, test } from "vitest";
declare module "vitest" {
  interface Assertion<T> {
    toBeWithin(min: number, max: number): void;
  }
}
test("x", () => {
  expect(5).toBeWithin("1", 10);
  expect(5).toBeBar();
});
