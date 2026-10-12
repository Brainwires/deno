import { expect, test } from "vitest";
import { double } from "./double.ts";
import { label } from "./label.js";

test("the real double", () => {
  expect(double(3)).toBe(6);
  expect(label(true)).toBe("yes");
});
