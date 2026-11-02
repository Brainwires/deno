import { test } from "node:test";
import { add, norm } from "./math.ts";

test("add", () => {
  if (add(1, 2) !== 3) throw new Error("add");
});

test("norm", () => {
  if (norm({ x: 3, y: 4 }) !== 5) throw new Error("norm");
});
