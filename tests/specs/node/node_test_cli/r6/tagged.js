const { describe, it, test } = require("node:test");
describe("db suite", { tags: ["db"] }, () => {
  it("db only", () => {});
  it("db plus integration", { tags: ["Integration"] }, () => {});
});
test("untagged", () => {});
test("flaky", { tags: ["flaky"] }, (t) => {
  t.test("inherits", () => {});
});
