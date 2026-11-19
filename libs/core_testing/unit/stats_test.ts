// Copyright 2018-2026 the Deno authors. MIT license.
import {
  barrierAwait,
  barrierCreate,
  LeakType,
  StatsFactory,
} from "checkin:async";
import { TestObjectWrap } from "checkin:object";
import {
  assert,
  assertEquals,
  assertStackTraceEquals,
  test,
} from "checkin:testing";

const { op_pipe_create } = Deno.core.ops;

test(async function testStatsOps() {
  using statsBefore = StatsFactory.capture();
  assert(statsBefore.dump().empty);

  barrierCreate("barrier", 3);
  const promise1 = barrierAwait("barrier");
  assertEquals(1, StatsFactory.capture().dump().count(LeakType.AsyncOp));
  const promise2 = barrierAwait("barrier");
  assertEquals(2, StatsFactory.capture().dump().count(LeakType.AsyncOp));
  // No traces here at all, even though we have ops
  assertEquals(
    0,
    StatsFactory.capture().dump().countWithTraces(LeakType.AsyncOp),
  );
  using statsMiddle = StatsFactory.capture();
  const diffMiddle = StatsFactory.diff(statsBefore, statsMiddle);
  assertEquals(0, diffMiddle.disappeared.count(LeakType.AsyncOp));
  assertEquals(2, diffMiddle.appeared.count(LeakType.AsyncOp));
  // No traces here at all, even though we have ops
  assertEquals(0, diffMiddle.appeared.countWithTraces(LeakType.AsyncOp));

  await Promise.all([promise1, promise2, barrierAwait("barrier")]);

  using statsAfter = StatsFactory.capture();
  const diff = StatsFactory.diff(statsBefore, statsAfter);
  assert(diff.empty);
});

// denext: a pending async method of a native class is reported under its own
// name. Its op id used to be the index of its class among the classes, which
// as an op id is another, earlier op (here one of `DOMPointReadOnly`'s).
test(async function testStatsOpsNameAnAsyncMethod() {
  // deno-lint-ignore no-explicit-any
  const opNames = (data: any[]) =>
    data.map((item) => item[LeakType.AsyncOp]?.[2]).sort().join(",");
  using statsBefore = StatsFactory.capture();
  const promise = new TestObjectWrap().withAsyncFn(50);
  using statsMiddle = StatsFactory.capture();
  const appeared = StatsFactory.diff(statsBefore, statsMiddle).appeared;
  assertEquals(1, appeared.count(LeakType.AsyncOp));
  assertEquals("withAsyncFn", opNames(appeared.rawData));
  assertEquals("withAsyncFn", opNames(statsMiddle.dump().rawData));
  // a regular op beside it keeps its own name
  barrierCreate("named", 2);
  const barrier = barrierAwait("named");
  using statsBoth = StatsFactory.capture();
  assertEquals(
    "op_async_barrier_await,withAsyncFn",
    opNames(statsBoth.dump().rawData),
  );
  await Promise.all([promise, barrier, barrierAwait("named")]);
  using statsAfter = StatsFactory.capture();
  assert(StatsFactory.diff(statsBefore, statsAfter).empty);
});

test(function testStatsResources() {
  using statsBefore = StatsFactory.capture();

  const [p1, p2] = op_pipe_create();
  using statsMiddle = StatsFactory.capture();
  const diffMiddle = StatsFactory.diff(statsBefore, statsMiddle);
  assertEquals(0, diffMiddle.disappeared.count(LeakType.Resource));
  assertEquals(2, diffMiddle.appeared.count(LeakType.Resource));
  Deno.core.close(p1);
  Deno.core.close(p2);

  using statsAfter = StatsFactory.capture();
  const diff = StatsFactory.diff(statsBefore, statsAfter);
  assert(diff.empty);
});

test(function testTimers() {
  // Timers are JS-managed but tracked by Rust via op_timer_track/op_timer_untrack
  // for leak sanitizer support.
  using statsBefore = StatsFactory.capture();

  const timeout = setTimeout(() => null, 1000);
  const interval = setInterval(() => null, 1000);

  using statsMiddle = StatsFactory.capture();
  const diffMiddle = StatsFactory.diff(statsBefore, statsMiddle);
  assertEquals(1, diffMiddle.appeared.count(LeakType.Timer));
  assertEquals(1, diffMiddle.appeared.count(LeakType.Interval));

  clearTimeout(timeout);
  clearInterval(interval);

  using statsAfter = StatsFactory.capture();
  const diff = StatsFactory.diff(statsBefore, statsAfter);
  assert(diff.empty);
});

async function enableTracingForTest(f: () => Promise<void> | void) {
  const oldTracingState = Deno.core.isLeakTracingEnabled();
  Deno.core.setLeakTracingEnabled(true);
  try {
    await f();
  } finally {
    Deno.core.setLeakTracingEnabled(oldTracingState);
  }
}

test(async function testAsyncLeakTrace() {
  await enableTracingForTest(async () => {
    barrierCreate("barrier", 2);
    const tracesBefore = Deno.core.getAllLeakTraces();
    using statsBefore = StatsFactory.capture();
    const p1 = barrierAwait("barrier");
    const tracesAfter = Deno.core.getAllLeakTraces();
    using statsAfter = StatsFactory.capture();
    const diff = StatsFactory.diff(statsBefore, statsAfter);
    // We don't test the contents, just that we have a trace here
    assertEquals(diff.appeared.countWithTraces(LeakType.AsyncOp), 1);

    assertEquals(tracesAfter.size, tracesBefore.size + 1);
    assertStackTraceEquals(
      Deno.core.getLeakTraceForPromise(p1)!,
      `
      at op_async_barrier_await (ext:core/00_infra.js:line:col)
      at barrierAwait (checkin:async:line:col)
      at test:///unit/stats_test.ts:line:col
      at enableTracingForTest (test:///unit/stats_test.ts:line:col)
      at testAsyncLeakTrace (test:///unit/stats_test.ts:line:col)
    `,
    );
    const p2 = barrierAwait("barrier");
    await Promise.all([p1, p2]);
  });
});

test(async function testTimeoutLeakTrace() {
  // Timers are tracked by Rust and have leak traces when tracing is enabled.
  await enableTracingForTest(() => {
    const tracesBefore = Deno.core.getAllLeakTraces();
    using statsBefore = StatsFactory.capture();
    const t1 = setTimeout(() => {}, 100_000);
    const tracesAfter = Deno.core.getAllLeakTraces();
    using statsAfter = StatsFactory.capture();
    const diff = StatsFactory.diff(statsBefore, statsAfter);
    assertEquals(diff.appeared.countWithTraces(LeakType.Timer), 1);

    assertEquals(tracesAfter.size, tracesBefore.size + 1);
    clearTimeout(t1);
    const tracesFinal = Deno.core.getAllLeakTraces();
    assertEquals(tracesFinal.size, tracesBefore.size);
  });
});

test(async function testIntervalLeakTrace() {
  // Intervals are tracked by Rust and have leak traces when tracing is enabled.
  await enableTracingForTest(() => {
    const tracesBefore = Deno.core.getAllLeakTraces();
    using statsBefore = StatsFactory.capture();
    const t1 = setInterval(() => {}, 100_000);
    const tracesAfter = Deno.core.getAllLeakTraces();
    using statsAfter = StatsFactory.capture();
    const diff = StatsFactory.diff(statsBefore, statsAfter);
    assertEquals(diff.appeared.countWithTraces(LeakType.Interval), 1);

    assertEquals(tracesAfter.size, tracesBefore.size + 1);
    clearInterval(t1);
    const tracesFinal = Deno.core.getAllLeakTraces();
    assertEquals(tracesFinal.size, tracesBefore.size);
  });
});
