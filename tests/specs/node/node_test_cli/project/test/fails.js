const test = require("node:test");
const assert = require("node:assert");
console.log("output of a test file");
test("fails", () => {
  assert.strictEqual(1, 2);
});
test("passes", () => {});
