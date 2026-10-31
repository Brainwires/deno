const { describe, it, test } = require("node:test");

for (const name of ["a", "b", "c", "d", "e"]) {
  test(name, () => {});
}

describe("suite", () => {
  for (const name of ["1", "2", "3"]) {
    it(name, () => {});
  }
});

test("parent", (t) => {
  for (const name of ["x", "y", "z"]) {
    t.test(name, () => {});
  }
});
