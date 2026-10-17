import assert from "node:assert";
import { describe, it, test } from "node:test";

test("waitFor polls until the condition holds", async (t) => {
  let n = 0;
  const value = await t.waitFor(() => {
    if (++n < 3) throw new Error("not yet");
    return n;
  }, { interval: 1 });
  assert.strictEqual(value, 3);
});

test("waitFor times out with the last failure as cause", async (t) => {
  const cause = new Error("still failing");
  await assert.rejects(
    t.waitFor(() => {
      throw cause;
    }, { interval: 1, timeout: 20 }),
    (err) => err.message === "waitFor() timed out" && err.cause === cause,
  );
});

test("plan validates its options", (t) => {
  assert.throws(() => t.plan(1, null), { code: "ERR_INVALID_ARG_TYPE" });
  assert.throws(() => t.plan(1, { wait: "x" }), {
    code: "ERR_INVALID_ARG_TYPE",
  });
});

describe("t.mock is reset after each test", () => {
  const object = { greet: () => "hello" };
  it("mocks", (t) => {
    t.mock.method(object, "greet", () => "mocked");
    t.mock.timers.enable({ apis: ["Date"], now: 1000 });
    assert.strictEqual(object.greet(), "mocked");
    assert.strictEqual(Date.now(), 1000);
  });
  it("sees the originals again", (t) => {
    assert.strictEqual(object.greet(), "hello");
    assert.ok(Date.now() > 1000);
    // the timers were reset, so they can be enabled again
    t.mock.timers.enable({ apis: ["Date"] });
  });
});
