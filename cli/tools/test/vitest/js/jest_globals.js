// Copyright 2018-2026 the Deno authors. MIT license.

// The `@jest/globals` module, mapped onto the Vitest implementation.

import { i as index } from "./vendor/vitest_core.js";
import { callerUrl } from "./mocker.js";

const { vi } = index;

export const {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  test,
} = index;

const { createRequire } = await import("node:module");
const { fileURLToPath } = await import("node:url");

function mocker() {
  return globalThis[Symbol.for("deno.vitest.mocker")];
}

/** Jest's synchronous `require` of a module, for `jest.requireActual()`. */
function requireActual(path, importer) {
  const resolved = globalThis.Deno[globalThis.Deno.internal].vitestMocks
    .resolve(path, importer);
  const require = createRequire(importer);
  return require(
    resolved?.startsWith("file:") ? fileURLToPath(resolved) : resolved ?? path,
  );
}

export const jest = {
  fn: (...args) => vi.fn(...args),
  spyOn: (...args) => vi.spyOn(...args),
  mocked: (value) => value,
  isMockFunction: (value) => vi.isMockFunction(value),
  clearAllMocks: () => (vi.clearAllMocks(), jest),
  resetAllMocks: () => (vi.resetAllMocks(), jest),
  restoreAllMocks: () => (vi.restoreAllMocks(), jest),
  useFakeTimers: (config) => (vi.useFakeTimers(config), jest),
  useRealTimers: () => (vi.useRealTimers(), jest),
  advanceTimersByTime: (ms) => (vi.advanceTimersByTime(ms), jest),
  advanceTimersByTimeAsync: async (ms) => (
    await vi.advanceTimersByTimeAsync(ms), jest
  ),
  advanceTimersToNextTimer: (
    steps,
  ) => (vi.advanceTimersToNextTimer(steps), jest),
  runAllTimers: () => (vi.runAllTimers(), jest),
  runAllTimersAsync: async () => (await vi.runAllTimersAsync(), jest),
  runOnlyPendingTimers: () => (vi.runOnlyPendingTimers(), jest),
  runOnlyPendingTimersAsync: async () => (
    await vi.runOnlyPendingTimersAsync(), jest
  ),
  runAllTicks: () => (vi.runAllTicks(), jest),
  clearAllTimers: () => (vi.clearAllTimers(), jest),
  getTimerCount: () => vi.getTimerCount(),
  setSystemTime: (time) => (vi.setSystemTime(time), jest),
  getRealSystemTime: () => vi.getRealSystemTime(),
  now: () => Date.now(),
  setTimeout: (testTimeout) => (vi.setConfig({ testTimeout }), jest),
  mock(path, factory, _options) {
    mocker().queueMock(path, callerUrl(jest.mock), factory);
    return jest;
  },
  doMock(path, factory, _options) {
    mocker().queueMock(path, callerUrl(jest.doMock), factory);
    return jest;
  },
  unmock(path) {
    mocker().queueUnmock(path, callerUrl(jest.unmock));
    return jest;
  },
  dontMock(path) {
    mocker().queueUnmock(path, callerUrl(jest.dontMock));
    return jest;
  },
  requireActual(path) {
    return requireActual(path, callerUrl(jest.requireActual));
  },
  requireMock(path) {
    throw new Error(
      `jest.requireMock(${
        JSON.stringify(path)
      }) is not supported by \`deno test\`: ES modules load asynchronously, use \`await vi.importMock()\` instead.`,
    );
  },
  createMockFromModule(path) {
    return mocker().mockObject(
      requireActual(path, callerUrl(jest.createMockFromModule)),
    );
  },
  resetModules() {
    mocker().resetModules();
    return jest;
  },
  isolateModules(fn) {
    mocker().resetModules();
    try {
      fn();
    } finally {
      mocker().resetModules();
    }
    return jest;
  },
};

export default {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  jest,
  test,
};
