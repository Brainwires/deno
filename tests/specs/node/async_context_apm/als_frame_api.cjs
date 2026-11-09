// AsyncLocalStorage as Node's AsyncContextFrame backend (its default since
// Node 24) exposes it, which APM agents feature-detect and rely on.
"use strict";
const { AsyncLocalStorage } = require("node:async_hooks");

// 1. run() goes through enterWith(): the probe agents use to detect the
//    AsyncContextFrame backend.
let acf = false;
{
  const probe = new AsyncLocalStorage();
  probe.enterWith = () => {
    acf = true;
  };
  probe.run(1, () => {});
  probe.disable();
}
console.log("run() delegates to enterWith():", acf);

// 2. A wrapped enterWith() sees every store change run() makes, on the way
//    in and out, and run() still restores the store.
{
  const als = new AsyncLocalStorage();
  const seen = [];
  const orig = als.enterWith;
  als.enterWith = function (store) {
    seen.push(store === undefined ? "undefined" : store);
    return orig.call(this, store);
  };
  als.run("outer", () => {
    als.run("inner", () => console.log("inner store:", als.getStore()));
    console.log("outer store:", als.getStore());
  });
  console.log("after run:", als.getStore());
  console.log("enterWith calls:", seen.join(","));
}

// 3. withScope() (Node 25.9), used by context managers' attach().
{
  const als = new AsyncLocalStorage();
  als.enterWith("initial");
  {
    using _scope = als.withScope("scoped");
    console.log("in scope:", als.getStore());
    setTimeout(() => console.log("timer from scope:", als.getStore()), 1);
  }
  console.log("after scope:", als.getStore());
  const scope = als.withScope("manual");
  scope.dispose();
  scope.dispose();
  console.log("after dispose():", als.getStore());
}
