import test from "node:test";
import { add } from "./math.js";
test("adds", () => {
  if (add(1, 2) !== 3) throw new Error("no");
});
