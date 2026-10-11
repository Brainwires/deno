// Copyright 2018-2026 the Deno authors. MIT license.

// The `@jest/globals` module, mapped onto the Vitest implementation.

import { i as index } from "./vendor/vitest_core.js";

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

function unsupported(api) {
  return () => {
    throw new Error(
      `jest.${api}() is not supported yet by \`deno test\`'s Jest compatibility: module mocking is not implemented.`,
    );
  };
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
  mock: unsupported("mock"),
  doMock: unsupported("doMock"),
  unmock: unsupported("unmock"),
  dontMock: unsupported("dontMock"),
  requireActual: unsupported("requireActual"),
  requireMock: unsupported("requireMock"),
  createMockFromModule: unsupported("createMockFromModule"),
  resetModules: unsupported("resetModules"),
  isolateModules: unsupported("isolateModules"),
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
