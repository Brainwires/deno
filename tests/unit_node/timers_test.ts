// Copyright 2018-2026 the Deno authors. MIT license.

import { assert, assertRejects, assertThrows, fail } from "@std/assert";
import * as perfHooks from "node:perf_hooks";
import * as timers from "node:timers";
import * as timersPromises from "node:timers/promises";
import process from "node:process";
import { assertEquals } from "@std/assert";
import {
  createHistogram,
  monitorEventLoopDelay,
  performance,
} from "node:perf_hooks";

Deno.test("[node/perf_hooks] performance.timerify()", () => {
  function sayHello() {
    return "hello world";
  }

  const wrapped = performance.timerify(sayHello);
  const result = wrapped();

  if (result !== "hello world") {
    throw new Error(`Expected "hello world", got "${result}"`);
  }
});

Deno.test("[node/perf_hooks] histogram parity regressions", () => {
  assertEquals("Histogram" in perfHooks, false);
  assertEquals("RecordableHistogram" in perfHooks, false);

  const h = createHistogram();
  assertEquals(h.percentiles, new Map([[100, 0]]));

  h.record(1);
  h.record(2);
  assertEquals(
    h.percentiles,
    new Map([[0, 1], [50, 1], [75, 2], [100, 2]]),
  );
  assertThrows(
    () => h.record(9223372036854775808n),
    RangeError,
    'The value of "val" is out of range',
  );

  const low = createHistogram({ lowest: 1, highest: 10 });
  const high = createHistogram({ lowest: 1, highest: 100000 });
  high.record(1000);
  low.add(high);
  assertEquals(low.count, 1);
  assertEquals(low.exceeds, 0);
  assertEquals(low.max, 0);
  assertEquals(low.percentile(100), 0);
  assertEquals(low.percentiles, new Map([[100, 0]]));

  const eld = monitorEventLoopDelay();
  assert(
    eld instanceof Object.getPrototypeOf(Object.getPrototypeOf(h))
      .constructor,
  );
  assertEquals(eld.percentiles, new Map([[100, 0]]));
});

Deno.test("[node/perf_hooks] timerify requires a real RecordableHistogram", () => {
  assertThrows(
    () =>
      performance.timerify(() => {}, {
        histogram: { record() {} } as never,
      }),
    TypeError,
    '"options.histogram" property must be an instance of RecordableHistogram',
  );
});

Deno.test("[node/timers setTimeout]", () => {
  {
    const { clearTimeout, setTimeout } = timers;
    const id = setTimeout(() => {});
    clearTimeout(id);
  }

  {
    const id = timers.setTimeout(() => {});
    timers.clearTimeout(id);
  }
});

Deno.test("[node/timers setInterval]", () => {
  {
    const { clearInterval, setInterval } = timers;
    const id = setInterval(() => {});
    clearInterval(id);
  }

  {
    const id = timers.setInterval(() => {});
    timers.clearInterval(id);
  }
});

Deno.test("[node/timers setImmediate]", async () => {
  {
    const { clearImmediate, setImmediate } = timers;
    const imm = setImmediate(() => {});
    clearImmediate(imm);
  }

  {
    const imm = timers.setImmediate(() => {});
    timers.clearImmediate(imm);
  }

  {
    const deffered = Promise.withResolvers<void>();
    const imm = timers.setImmediate(
      (a, b) => {
        assert(a === 1);
        assert(b === 2);
        deffered.resolve();
      },
      1,
      2,
    );
    await deffered;
    timers.clearImmediate(imm);
  }
});

Deno.test("[node/timers/promises setTimeout]", () => {
  const { setTimeout } = timersPromises;
  const p = setTimeout(0);

  assert(p instanceof Promise);
  return p;
});

Deno.test("[node/timers/promises scheduler.wait]", async () => {
  const { scheduler } = timersPromises;
  let resolved = false;
  timers.setTimeout(() => (resolved = true), 20);
  const p = scheduler.wait(20);

  assert(p instanceof Promise);
  await p;
  assert(resolved);
});

Deno.test("[node/timers/promises scheduler.yield]", async () => {
  const { scheduler } = timersPromises;
  let resolved = false;
  timers.setImmediate(() => resolved = true);

  const p = scheduler.yield();
  assert(p instanceof Promise);
  await p;

  assert(resolved);
});

// Regression test for https://github.com/denoland/deno/issues/17981
Deno.test("[node/timers refresh cancelled timer]", () => {
  const { setTimeout, clearTimeout } = timers;
  const p = setTimeout(() => {
    fail();
  }, 1);
  clearTimeout(p);
  p.refresh();
});

Deno.test("[node/timers] clearTimeout with number", () => {
  const timer = +timers.setTimeout(() => fail(), 10);
  timers.clearTimeout(timer);
});

Deno.test("[node/timers] clearInterval with number", () => {
  const timer = +timers.setInterval(() => fail(), 10);
  timers.clearInterval(timer);
});

Deno.test("[node/timers] getActiveResourcesInfo counts refed timers", () => {
  const countTimeouts = () =>
    process.getActiveResourcesInfo().filter((type) => type === "Timeout")
      .length;
  const before = countTimeouts();
  const timeout = timers.setTimeout(() => fail(), 1000);
  const unrefed = timers.setTimeout(() => fail(), 1000).unref();
  const interval = timers.setInterval(() => fail(), 1000);
  try {
    assertEquals(countTimeouts(), before + 2);
    // A numeric id reaches the Node.js timer, and the count follows.
    Deno.unrefTimer(+interval);
    assertEquals(countTimeouts(), before + 1);
    Deno.refTimer(+unrefed);
    assertEquals(countTimeouts(), before + 2);
    timers.clearTimeout(+timeout);
    assertEquals(countTimeouts(), before + 1);
  } finally {
    timers.clearTimeout(timeout);
    timers.clearTimeout(unrefed);
    timers.clearInterval(interval);
  }
  assertEquals(countTimeouts(), before);
});

Deno.test("[node/timers setImmediate returns Immediate object]", () => {
  const { clearImmediate, setImmediate } = timers;

  const imm = setImmediate(() => {});
  imm.unref();
  imm.ref();
  imm.hasRef();
  clearImmediate(imm);
});

Deno.test({
  name: "setInterval yields correct values at expected intervals",
  async fn() {
    // Test configuration
    const CONFIG = {
      expectedValue: 42,
      intervalMs: 100,
      iterations: 3,
      tolerancePercent: Deno.env.get("CI") != null ? 75 : 50,
    };

    const { setInterval } = timersPromises;
    const results: Array<{ value: number; timestamp: number }> = [];
    const startTime = Date.now();

    const iterator = setInterval(CONFIG.intervalMs, CONFIG.expectedValue);

    for await (const value of iterator) {
      results.push({
        value,
        timestamp: Date.now(),
      });
      if (results.length === CONFIG.iterations) {
        break;
      }
    }

    const values = results.map((r) => r.value);
    assertEquals(
      values,
      Array(CONFIG.iterations).fill(CONFIG.expectedValue),
      `Each iteration should yield ${CONFIG.expectedValue}`,
    );

    const intervals = results.slice(1).map((result, index) => ({
      interval: result.timestamp - results[index].timestamp,
      iterationNumber: index + 1,
    }));

    const toleranceMs = (CONFIG.tolerancePercent / 100) * CONFIG.intervalMs;
    const expectedRange = {
      min: CONFIG.intervalMs - toleranceMs,
      max: CONFIG.intervalMs + toleranceMs,
    };

    intervals.forEach(({ interval, iterationNumber }) => {
      const isWithinTolerance = interval >= expectedRange.min &&
        interval <= expectedRange.max;

      assertEquals(
        isWithinTolerance,
        true,
        `Iteration ${iterationNumber}: Interval ${interval}ms should be within ` +
          `${expectedRange.min}ms and ${expectedRange.max}ms ` +
          `(${CONFIG.tolerancePercent}% tolerance of ${CONFIG.intervalMs}ms)`,
      );
    });

    const totalDuration = results[results.length - 1].timestamp - startTime;
    const expectedDuration = CONFIG.intervalMs * CONFIG.iterations;
    const isDurationReasonable =
      totalDuration >= (expectedDuration - toleranceMs) &&
      totalDuration <= (expectedDuration + toleranceMs);

    assertEquals(
      isDurationReasonable,
      true,
      `Total duration ${totalDuration}ms should be close to ${expectedDuration}ms ` +
        `(within ${toleranceMs}ms tolerance)`,
    );

    const timestamps = results.map((r) => r.timestamp);
    const areTimestampsOrdered = timestamps.every((timestamp, i) =>
      i === 0 || timestamp > timestamps[i - 1]
    );

    assertEquals(
      areTimestampsOrdered,
      true,
      "Timestamps should be strictly increasing",
    );
  },
});

Deno.test({
  name: "setInterval with AbortSignal stops after expected duration",
  async fn() {
    const INTERVAL_MS = 500;
    const TOTAL_DURATION_MS = 3000;
    const TOLERANCE_MS = 500;
    const DELTA_TOLERANCE_MS = Deno.env.get("CI") != null ? 100 : 50;

    const abortController = new AbortController();
    const { setInterval } = timersPromises;

    // Set up abort after specified duration
    const abortTimeout = timers.setTimeout(() => {
      abortController.abort();
    }, TOTAL_DURATION_MS);

    // Track iterations and timing
    const startTime = Date.now();
    const iterations: number[] = [];

    try {
      for await (
        const _timestamp of setInterval(INTERVAL_MS, undefined, {
          signal: abortController.signal,
        })
      ) {
        iterations.push(Date.now() - startTime);
      }
    } catch (error) {
      if (error instanceof Error && error.name !== "AbortError") {
        throw error;
      }
    } finally {
      timers.clearTimeout(abortTimeout);
    }

    // Validate timing
    const totalDuration = iterations[iterations.length - 1];
    const isWithinTolerance =
      totalDuration >= (TOTAL_DURATION_MS - TOLERANCE_MS) &&
      totalDuration <= (TOTAL_DURATION_MS + TOLERANCE_MS);

    assertEquals(
      isWithinTolerance,
      true,
      `Total duration ${totalDuration}ms should be within ±${TOLERANCE_MS}ms of ${TOTAL_DURATION_MS}ms`,
    );

    // Validate interval consistency
    const intervalDeltas = iterations.slice(1).map((time, i) =>
      time - iterations[i]
    );

    intervalDeltas.forEach((delta, i) => {
      const isIntervalValid = delta >= (INTERVAL_MS - DELTA_TOLERANCE_MS) &&
        delta <= (INTERVAL_MS + DELTA_TOLERANCE_MS);
      assertEquals(
        isIntervalValid,
        true,
        `Interval ${
          i + 1
        } duration (${delta}ms) should be within ±${DELTA_TOLERANCE_MS}ms of ${INTERVAL_MS}ms`,
      );
    });
  },
});

Deno.test({
  name: "[timers/promises] setTimeout aborted by AbortSignal",
  async fn() {
    const timerPromise = new Promise((resolve, reject) => {
      const abortController = new AbortController();
      const timer = timersPromises.setTimeout(1000, "foo", {
        signal: abortController.signal,
      });
      abortController.abort();
      timer.then(resolve).catch(reject);
    });

    await assertRejects(
      () => timerPromise,
      Error,
      "The operation was aborted",
    );
  },
});

// A chain of immediates used to starve the reactor: the executor polled its
// I/O driver once in 61 loop iterations, so socket readiness waited for up to
// 61 immediates (an echo for 122). libuv polls for I/O and runs the due
// timers on every iteration.
function runImmediateChain(burnMs: number) {
  const state = { ticks: 0, done: false };
  const chain = () => {
    state.ticks++;
    const end = performance.now() + burnMs;
    while (performance.now() < end);
    if (!state.done) timers.setImmediate(chain);
  };
  timers.setImmediate(chain);
  return state;
}

Deno.test({
  name: "[node/timers] socket I/O progresses between consecutive immediates",
  async fn() {
    const net = await import("node:net");
    const server = net.createServer((socket) => {
      socket.on("data", (data) => socket.write(data));
    });
    await new Promise<void>((resolve) =>
      server.listen(0, "127.0.0.1", resolve)
    );
    const { port } = server.address() as { port: number };
    const client = net.connect(port, "127.0.0.1");
    client.setNoDelay(true);
    await new Promise<void>((resolve) => client.once("connect", resolve));

    const chain = runImmediateChain(5);
    const tickLatencies: number[] = [];
    try {
      for (let i = 0; i < 5; i++) {
        const sentAt = chain.ticks;
        const echoed = new Promise<void>((resolve) =>
          client.once("data", () => resolve())
        );
        client.write("x");
        await echoed;
        tickLatencies.push(chain.ticks - sentAt);
      }
    } finally {
      chain.done = true;
      client.destroy();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    }
    // One round trip is two hops through the reactor: a few immediates. When
    // the reactor was polled once in 61 iterations it took 61 to 122.
    const worst = Math.max(...tickLatencies);
    assert(
      worst <= 16,
      `an echo took ${tickLatencies} immediates; it should take a few`,
    );
  },
});

Deno.test({
  name: "[node/timers] a due timer runs between consecutive immediates",
  async fn() {
    const chain = runImmediateChain(5);
    const tickLatencies: number[] = [];
    try {
      for (let i = 0; i < 5; i++) {
        const setAt = chain.ticks;
        await new Promise<void>((resolve) => timers.setTimeout(resolve, 1));
        tickLatencies.push(chain.ticks - setAt);
      }
    } finally {
      chain.done = true;
    }
    // 1 ms is due after the first 5 ms immediate. (Timers were not starved
    // as I/O was; this pins it.)
    const worst = Math.max(...tickLatencies);
    assert(
      worst <= 8,
      `a 1 ms timer took ${tickLatencies} immediates; it should take one or two`,
    );
  },
});
