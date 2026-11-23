// Copyright 2018-2026 the Deno authors. MIT license.
import {
  assert,
  assertArrayEquals,
  assertEquals,
  assertStackTraceEquals,
  test,
} from "checkin:testing";
import {
  asyncPromiseId,
  asyncYield,
  barrierAwait,
  barrierCreate,
  fakeAsync,
} from "checkin:async";
import { asyncThrow } from "checkin:error";

// Test that stack traces from async ops are all sane
test(async function testAsyncThrow() {
  try {
    await asyncThrow("eager");
  } catch (e) {
    assertStackTraceEquals(
      e.stack,
      `TypeError: Error
        at asyncThrow (checkin:error:line:col)
        at testAsyncThrow (test:///unit/ops_async_test.ts:line:col)
      `,
    );
  }
  try {
    await asyncThrow("lazy");
  } catch (e) {
    // The stack may or may not include processTicksAndRejections depending
    // on whether the microtask runs inside op_run_microtasks (no ticks
    // scheduled) or processTicksAndRejections (ticks scheduled).
    assert(e.stack.includes("asyncThrow"));
    assert(e.stack.includes("testAsyncThrow"));
  }
  try {
    await asyncThrow("deferred");
  } catch (e) {
    assert(e.stack.includes("asyncThrow"));
    assert(e.stack.includes("testAsyncThrow"));
  }
});

test(async function testAsyncOp() {
  await asyncYield();
});

// Test a large number of async ops resolving at the same time. This stress-tests both
// large-batch op dispatch and the JS-side promise-tracking implementation.
test(async function testAsyncBarrier() {
  const count = 1e5;
  barrierCreate("barrier", count);
  const promises = [];
  for (let i = 0; i < count; i++) {
    promises.push(barrierAwait("barrier"));
  }
  await Promise.all(promises);
});

test(async function promiseId() {
  const id = await asyncPromiseId();

  assert(typeof id === "number");
  assert(id > 0);
});

// Promise ids are i32s that wrap back to 0 past 2^31 - 1. Test that an op
// that is still pending when the wraparound happens (and whose ring slot has
// been reclaimed in the meantime, moving it to the overflow map) resolves
// correctly. https://github.com/denoland/deno/issues/22759
test(async function testPromiseIdWraparound() {
  const distanceToWrap = 5000;
  Deno.core.__setNextPromiseId(2 ** 31 - distanceToWrap);

  barrierCreate("wraparound_barrier", 2);
  const pending = barrierAwait("wraparound_barrier");

  // Churn through enough ops that `pending` is evicted from the promise ring
  // (RING_SIZE == 4096 ids later) and the id counter wraps past 2^31 - 1.
  for (let i = 0; i < distanceToWrap + 1000; i++) {
    await asyncYield();
  }

  // The counter must have wrapped back to a small non-negative id.
  const id = await asyncPromiseId();
  assert(id >= 0);
  assert(id < distanceToWrap);

  // Releasing the barrier resolves `pending`, which by now lives in the
  // overflow map on the far side of the wraparound.
  await Promise.all([pending, barrierAwait("wraparound_barrier")]);
});

// Ops that complete eagerly return a plain resolved promise with no promise
// id attached, so ref/unref of such a promise must be a silent no-op.
// `hasPromise(undefined)` reads `promiseRing[NaN]`, which would throw on the
// `promise[2]` identity check if the sentinel comparison were strict; the
// loose `!=` against `NO_PROMISE` keeps it a no-op.
test(async function testRefUnrefPromiseWithoutId() {
  const eager = Promise.resolve("eager");
  Deno.core.refOpPromise(eager);
  Deno.core.unrefOpPromise(eager);
  await eager;
});

// An async op that completes on its first poll, or fails to dispatch, hands
// back the runtime's own promise, as a pending op does: a promise hook can
// tell any of them apart from JS's promises (`isCreatingInternalPromise`).
// Whether a blocking op is still pending at that first poll is down to timing,
// so `node:async_hooks` used to report a PROMISE for some `fs.access()` calls.
test(async function testEagerOpPromiseIsInternal() {
  let recording = false;
  const created: boolean[] = [];
  Deno.core.setPromiseHooks((_promise: Promise<unknown>) => {
    if (recording) created.push(Deno.core.isCreatingInternalPromise());
  });
  let resolved: Promise<unknown>;
  let rejected: Promise<unknown>;
  recording = true;
  try {
    resolved = fakeAsync();
    rejected = Deno.core.ops.op_async_throw_error_eager();
  } finally {
    recording = false;
  }
  assert(created.length > 0);
  assertArrayEquals(created, created.map(() => true));
  assertEquals(await resolved, 1);
  let threw = false;
  try {
    await rejected;
  } catch (e) {
    threw = e instanceof TypeError;
  }
  assert(threw);
});

// The primordials' promise helpers are runtime code whatever calls them: the
// promises they make are counted as internal (`isCreatingInternalPromise`),
// not left to the caller's stack frames, which a promise hook reads as core's
// for them.
test(async function testPrimordialPromiseHelpersAreInternal() {
  // deno-lint-ignore no-explicit-any
  const { primordials } = (globalThis as any).__bootstrap;
  const {
    PromisePrototypeCatch,
    SafePromiseAll,
    SafePromiseAllSettled,
    SafePromiseAny,
    SafePromisePrototypeFinally,
    SafePromiseRace,
  } = primordials;
  const inputs = [Promise.resolve(1), Promise.resolve(2)];
  let recording = false;
  const created: boolean[] = [];
  Deno.core.setPromiseHooks((_promise: Promise<unknown>) => {
    if (recording) created.push(Deno.core.isCreatingInternalPromise());
  });
  let results: Promise<unknown>[];
  recording = true;
  try {
    results = [
      PromisePrototypeCatch(inputs[0], () => {}),
      SafePromiseAll(inputs),
      SafePromiseAllSettled(inputs),
      SafePromiseAny(inputs),
      SafePromiseRace(inputs),
      SafePromisePrototypeFinally(inputs[0], () => {}),
    ];
  } finally {
    recording = false;
  }
  assert(created.length >= results.length);
  assertArrayEquals(created, created.map(() => true));
  assertEquals(
    JSON.stringify(await Promise.all(results)),
    JSON.stringify([
      1,
      [1, 2],
      [
        { status: "fulfilled", value: 1 },
        { status: "fulfilled", value: 2 },
      ],
      1,
      1,
      1,
    ]),
  );
  assert(!Deno.core.isCreatingInternalPromise());
});
