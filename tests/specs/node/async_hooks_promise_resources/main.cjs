const async_hooks = require("node:async_hooks");

const events = [];
const ids = new Map();
const name = (id) => ids.get(id) ?? `#${id}`;
const hook = async_hooks.createHook({
  init(id, type, triggerId) {
    if (type !== "PROMISE") return;
    ids.set(id, `p${ids.size + 1}`);
    events.push(`init ${name(id)} ${type} trigger=${name(triggerId)}`);
  },
  before(id) {
    events.push(`before ${name(id)}`);
  },
  after(id) {
    events.push(`after ${name(id)}`);
  },
  promiseResolve(id) {
    events.push(`resolve ${name(id)}`);
  },
}).enable();

console.log("main executionAsyncId", async_hooks.executionAsyncId());
const p1 = Promise.resolve(42);
p1.then(() => {
  events.push(
    `in then: executionAsyncId is ${name(async_hooks.executionAsyncId())}`,
  );
}).then(() => {
  hook.disable();
  console.log(events.join("\n"));
});
