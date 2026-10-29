const { test } = require("node:test");

test("parent", (t) => {
  // Not awaited: the parent still waits for it, as in Node.
  t.test("child", async () => {
    await new Promise((resolve) => setTimeout(resolve, 10));
    console.log("child ran");
  });
});
