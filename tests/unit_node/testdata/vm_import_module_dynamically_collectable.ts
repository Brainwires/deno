// Copyright 2018-2026 the Deno authors. MIT license.
// Every Script compiled with an `importModuleDynamically` callback is
// collected with the code: the callback (whose closure holds the Script) does
// not keep it alive. Run by vm_test.ts with --expose-gc.
import { Script } from "node:vm";
import v8 from "node:v8";
import { strictEqual } from "node:assert/strict";

const gc = (globalThis as {
  gc?: (options: { type: string; execution: string }) => Promise<void>;
}).gc;
strictEqual(typeof gc, "function");

// The event loop does not wait for V8's own tasks, the one that runs the
// collection included: a timer keeps it running until the collection is done.
async function collect() {
  const keepAlive = setTimeout(() => {}, 60_000);
  try {
    await gc!({ type: "major", execution: "async" });
  } finally {
    clearTimeout(keepAlive);
  }
}

const count = 1000;
let finalized = 0;
const registry = new FinalizationRegistry(() => {
  finalized++;
});
for (let i = 0; i < count; i++) {
  registry.register(
    new Script(`"${i}";`, { importModuleDynamically() {} } as never),
    i,
  );
}

// A collection that V8 runs from a task, with no JS on the stack. One started
// from JS (`gc()`, or the heap snapshot `queryObjects` takes) scans the native
// stack conservatively for C++ (cppgc) objects, and a stale stack word that
// holds the address of a script's native wrapper keeps that script alive with
// it, for as long as nothing overwrites the word. A few rounds, a task apart,
// as Node.js's `gcUntil` does: an optimizing compile job that runs beside
// the loop holds what it looked at until the main thread installs its code.
// A script that leaks is never collected, whatever the number of rounds.
let alive = -1;
for (let round = 0; round < 10 && alive !== 0; round++) {
  await collect();
  alive = v8.queryObjects(Script, { format: "count" });
  if (alive !== 0) await new Promise((resolve) => setTimeout(resolve, 10));
}
strictEqual(alive, 0);

// The callbacks run in tasks of their own, after the collection: V8 posts one
// per registry that has collected targets, one at a time.
const deadline = Date.now() + 10_000;
while (finalized < count && Date.now() < deadline) {
  await new Promise((resolve) => setTimeout(resolve, 0));
}
strictEqual(finalized, count);

// The registry is used after the waits, which keeps it alive through them: an
// unreachable registry calls back for none of its targets, and optimized code
// lets go of a local as soon as nothing reads it again.
strictEqual(registry instanceof FinalizationRegistry, true);
