const assert = require("node:assert");
const { beforeEach, describe, it, test } = require("node:test");

beforeEach((t) => console.log(`beforeEach ${t.name}`));

describe("suite", () => {
  it("passes", () => {});
  it("is skipped", { skip: true }, () => {});
});

test("fails", () => {
  assert.strictEqual(1, 2);
});

test("is todo", { todo: true });
