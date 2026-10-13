// Copyright 2018-2026 the Deno authors. MIT license.

// Types for `@jest/globals` under `deno test` and `deno check`, used when the
// package itself is not installed (its own types take precedence). They
// describe the surface `deno test` implements on its Vitest layer (see
// js/jest.js): Jest's test API (a declared parameter of a test or hook is the
// `done` callback), `expect` and the `jest` object.

// deno-lint-ignore-file no-explicit-any camelcase

import type {
  Constructable,
  ExpectStatic,
  FakeTimerInstallOpts,
  MaybeMocked,
  MaybeMockedDeep,
  Mock as VitestMock,
  MockedClass as VitestMockedClass,
  MockedFunction as VitestMockedFunction,
  MockedObject as VitestMockedObject,
  MockInstance as VitestMockInstance,
  Procedure,
  SuiteAPI,
} from "./vitest.d.ts";

/** The callback a test or hook that declares a parameter gets. */
export interface DoneCallback {
  (...args: any[]): any;
  fail(error?: string | { message: string }): any;
}
/** A test or hook function: takes `done`, or returns (a promise of) nothing. */
export type ProvidesCallback =
  | ((done: DoneCallback) => void | undefined)
  | (() => PromiseLike<unknown> | void | undefined);
export type ProvidesHookCallback = ProvidesCallback;

interface JestEachReturn<T extends any[]> {
  (name: string, fn: (...args: T) => any, timeout?: number): void;
}
export interface JestEach {
  <T extends any[] | [any]>(cases: ReadonlyArray<T>): JestEachReturn<T>;
  <T>(cases: ReadonlyArray<T>): JestEachReturn<[T]>;
  (strings: TemplateStringsArray, ...values: any[]): JestEachReturn<[any]>;
}

export interface JestTestAPI {
  (name: string, fn?: ProvidesCallback, timeout?: number): void;
  each: JestEach;
  only: JestTestAPI;
  skip: JestTestAPI;
  todo: (name: string) => void;
  concurrent: JestTestAPI;
  failing: JestTestAPI;
  fails: JestTestAPI;
  skipIf(condition: unknown): JestTestAPI;
  runIf(condition: unknown): JestTestAPI;
}

export type JestHook = (fn: ProvidesHookCallback, timeout?: number) => void;

export declare const describe: SuiteAPI;
export declare const fdescribe: SuiteAPI;
export declare const xdescribe: SuiteAPI;
export declare const it: JestTestAPI;
export declare const test: JestTestAPI;
export declare const fit: JestTestAPI;
export declare const xit: JestTestAPI;
export declare const xtest: JestTestAPI;
export declare const beforeAll: JestHook;
export declare const beforeEach: JestHook;
export declare const afterAll: JestHook;
export declare const afterEach: JestHook;
export declare const expect: ExpectStatic;

/** What `jest.replaceProperty()` returns. */
export interface Replaced<T = unknown> {
  replaceValue(value: T): this;
  restore(): void;
}

/** The `jest` object. */
export interface Jest {
  fn<T extends Procedure = Procedure>(implementation?: T): VitestMock<T>;
  spyOn<T, K extends keyof T>(
    object: T,
    method: K,
    accessType?: "get" | "set",
  ): Required<T>[K] extends Procedure ? VitestMockInstance<Required<T>[K]>
    : VitestMockInstance<any>;
  mocked<T>(item: T, options?: { shallow?: false }): MaybeMockedDeep<T>;
  mocked<T>(item: T, options: { shallow: true }): MaybeMocked<T>;
  isMockFunction(fn: unknown): fn is VitestMockInstance;
  replaceProperty<T extends object, K extends keyof T>(
    object: T,
    key: K,
    value: T[K],
  ): Replaced<T[K]>;
  clearAllMocks(): Jest;
  resetAllMocks(): Jest;
  restoreAllMocks(): Jest;

  // modules
  mock<T = unknown>(
    path: string,
    factory?: () => T,
    options?: { virtual?: boolean },
  ): Jest;
  doMock<T = unknown>(
    path: string,
    factory?: () => T,
    options?: { virtual?: boolean },
  ): Jest;
  unstable_mockModule<T = unknown>(
    path: string,
    factory: () => T | Promise<T>,
    options?: { virtual?: boolean },
  ): Jest;
  unstable_unmockModule(path: string): Jest;
  setMock(path: string, moduleExports: unknown): Jest;
  unmock(path: string): Jest;
  deepUnmock(path: string): Jest;
  dontMock(path: string): Jest;
  requireActual<T = any>(path: string): T;
  requireMock<T = any>(path: string): T;
  createMockFromModule<T = any>(path: string): T;
  genMockFromModule<T = any>(path: string): T;
  resetModules(): Jest;
  isolateModules(fn: () => void): Jest;
  isolateModulesAsync(fn: () => Promise<void>): Promise<void>;
  enableAutomock(): Jest;
  disableAutomock(): Jest;

  // timers
  useFakeTimers(
    config?: FakeTimerInstallOpts & {
      legacyFakeTimers?: boolean;
      doNotFake?: string[];
      advanceTimers?: boolean | number;
      timerLimit?: number;
    },
  ): Jest;
  useRealTimers(): Jest;
  isFakeTimers(): boolean;
  advanceTimersByTime(ms: number): Jest;
  advanceTimersByTimeAsync(ms: number): Promise<Jest>;
  advanceTimersToNextTimer(steps?: number): Jest;
  advanceTimersToNextTimerAsync(steps?: number): Promise<Jest>;
  advanceTimersToNextFrame(): Jest;
  runAllTimers(): Jest;
  runAllTimersAsync(): Promise<Jest>;
  runOnlyPendingTimers(): Jest;
  runOnlyPendingTimersAsync(): Promise<Jest>;
  runAllTicks(): Jest;
  runAllImmediates(): Jest;
  clearAllTimers(): Jest;
  getTimerCount(): number;
  setSystemTime(now?: number | Date): Jest;
  getRealSystemTime(): number;
  now(): number;

  // the run
  setTimeout(timeout: number): Jest;
  retryTimes(retries: number, options?: Record<string, unknown>): Jest;
  getSeed(): number;
  isEnvironmentTornDown(): boolean;
}

export declare const jest: Jest;

/** Jest's mock types, as `jest.Mock<T>` & co. */
export declare namespace jest {
  export type Mock<T extends Procedure = Procedure> = VitestMock<T>;
  export type MockInstance<T extends Procedure = Procedure> =
    VitestMockInstance<T>;
  export type MockedFunction<T extends Procedure> = VitestMockedFunction<T>;
  export type MockedClass<T extends Constructable> = VitestMockedClass<T>;
  export type MockedObject<T> = VitestMockedObject<T>;
  export type Mocked<T> = T extends Procedure ? VitestMockedFunction<T>
    : T extends Constructable ? VitestMockedClass<T>
    : T extends object ? VitestMockedObject<T>
    : T;
  export type Spied<T extends Procedure> = VitestMockInstance<T>;
  export type SpiedFunction<T extends Procedure> = VitestMockInstance<T>;
  export type SpiedClass<T extends Constructable> = VitestMockInstance<
    (...args: ConstructorParameters<T>) => InstanceType<T>
  >;
  export type Replaced<T = unknown> = import("./jest_globals.d.ts").Replaced<
    T
  >;
  export type UnknownFunction = (...args: unknown[]) => unknown;
}

declare const globals: {
  afterAll: typeof afterAll;
  afterEach: typeof afterEach;
  beforeAll: typeof beforeAll;
  beforeEach: typeof beforeEach;
  describe: typeof describe;
  expect: typeof expect;
  it: typeof it;
  jest: typeof jest;
  test: typeof test;
};
export default globals;
