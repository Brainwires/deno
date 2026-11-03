const { test } = require("node:test");
test("passes", ({ attempt }) => {
  if (attempt > 0) throw new Error("ran again after it passed");
});
test("fails until the third attempt", ({ attempt }) => {
  if (attempt < 2) throw new Error(`attempt ${attempt}`);
});
test("parent", async (t) => {
  await t.test("child", () => {});
});
