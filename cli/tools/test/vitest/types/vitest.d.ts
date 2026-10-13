// Copyright 2018-2026 the Deno authors. MIT license.

// Types for `vitest` under `deno test` and `deno check`, used when the vitest
// package itself (and its types) is not installed: an installed vitest's own
// types take precedence. They describe the surface `deno test` implements
// (the vendored Vitest runtime, see cli/tools/test/vitest), so a test file
// type-checks without the package: `vi.mock()` factories, `vi.fn()` mocks,
// `expect()` matchers, the test API and its modifiers, hooks and fixtures.

// deno-lint-ignore-file no-explicit-any ban-types no-empty-interface

// --- helpers ----------------------------------------------------------------

export type Awaitable<T> = T | PromiseLike<T>;
export type Procedure = (...args: any[]) => any;
export type Constructable = new (...args: any[]) => any;
type Methods<T> = keyof {
  [K in keyof T as T[K] extends Procedure ? K : never]: T[K];
};
type Properties<T> = keyof {
  [K in keyof T as T[K] extends Procedure ? never : K]: T[K];
};
type Classes<T> = keyof {
  [K in keyof T as T[K] extends Constructable ? K : never]: T[K];
};
type Promisify<O> = {
  [K in keyof O]: O[K] extends (...args: infer A) => infer R
    ? Promisify<O[K]> & ((...args: A) => Promise<R>)
    : O[K];
};

// --- mocks ------------------------------------------------------------------

export interface MockResultReturn<T> {
  type: "return";
  value: T;
}
export interface MockResultThrow {
  type: "throw";
  value: any;
}
export interface MockResultIncomplete {
  type: "incomplete";
  value: undefined;
}
export type MockResult<T> =
  | MockResultReturn<T>
  | MockResultThrow
  | MockResultIncomplete;
export interface MockSettledResultFulfilled<T> {
  type: "fulfilled";
  value: T;
}
export interface MockSettledResultRejected {
  type: "rejected";
  value: any;
}
export interface MockSettledResultIncomplete {
  type: "incomplete";
  value: undefined;
}
export type MockSettledResult<T> =
  | MockSettledResultFulfilled<T>
  | MockSettledResultRejected
  | MockSettledResultIncomplete;

export interface MockContext<T extends Procedure = Procedure> {
  /** The arguments of each call. */
  calls: Parameters<T>[];
  /** The `this` of each call made with `new`. */
  instances: ReturnType<T>[];
  /** The `this` of each call. */
  contexts: ThisParameterType<T>[];
  /** The order of each call among all mocks' calls. */
  invocationCallOrder: number[];
  /** What each call returned or threw. */
  results: MockResult<ReturnType<T>>[];
  /** What the promise each call returned resolved or rejected with. */
  settledResults: MockSettledResult<Awaited<ReturnType<T>>>[];
  /** The arguments of the last call. */
  lastCall: Parameters<T> | undefined;
}

export interface MockInstance<T extends Procedure = Procedure> {
  /** The mock's name (`vi.fn()` by default). */
  getMockName(): string;
  mockName(name: string): this;
  /** The calls, results and instances recorded so far. */
  mock: MockContext<T>;
  /** Clears the recorded calls. */
  mockClear(): this;
  /** Clears the recorded calls and resets the implementation. */
  mockReset(): this;
  /** Restores the original implementation (of a `vi.spyOn()` spy). */
  mockRestore(): void;
  getMockImplementation(): T | undefined;
  mockImplementation(fn: T): this;
  mockImplementationOnce(fn: T): this;
  withImplementation(fn: T, callback: () => Promise<unknown>): Promise<this>;
  withImplementation(fn: T, callback: () => unknown): this;
  mockReturnThis(): this;
  mockReturnValue(value: ReturnType<T>): this;
  mockReturnValueOnce(value: ReturnType<T>): this;
  mockResolvedValue(value: Awaited<ReturnType<T>>): this;
  mockResolvedValueOnce(value: Awaited<ReturnType<T>>): this;
  mockRejectedValue(error: unknown): this;
  mockRejectedValueOnce(error: unknown): this;
}

/** A mock function, as `vi.fn()` returns. */
export interface Mock<T extends Procedure = Procedure> extends MockInstance<T> {
  (...args: Parameters<T>): ReturnType<T>;
  new (...args: Parameters<T>): ReturnType<T>;
}

export type MockedFunction<T extends Procedure> =
  & Mock<T>
  & {
    [K in keyof T]: T[K];
  };
export type MockedFunctionDeep<T extends Procedure> =
  & Mock<T>
  & MockedObjectDeep<T>;
export type MockedClass<T extends Constructable> =
  & MockInstance<(...args: ConstructorParameters<T>) => InstanceType<T>>
  & { prototype: T extends { prototype: any } ? Mocked<T["prototype"]> : never }
  & T;
export type MockedObject<T> =
  & {
    [K in Methods<T>]: T[K] extends Procedure ? MockedFunction<T[K]> : T[K];
  }
  & { [K in Properties<T>]: T[K] };
export type MockedObjectDeep<T> =
  & {
    [K in Methods<T>]: T[K] extends Procedure ? MockedFunctionDeep<T[K]>
      : T[K];
  }
  & { [K in Properties<T>]: MaybeMockedDeep<T[K]> };
export type MaybeMocked<T> = T extends Procedure ? MockedFunction<T>
  : T extends object ? MockedObject<T>
  : T;
export type MaybeMockedDeep<T> = T extends Procedure ? MockedFunctionDeep<T>
  : T extends object ? MockedObjectDeep<T>
  : T;
export type MaybePartiallyMocked<T> = MaybeMocked<T>;
export type MaybePartiallyMockedDeep<T> = MaybeMockedDeep<T>;
export type Mocked<T> =
  & {
    [P in keyof T]: T[P] extends Procedure ? MockInstance<T[P]>
      : T[P] extends Constructable ? MockedClass<T[P]>
      : T[P];
  }
  & T;

/** A `vi.mock()` factory: returns (some of) the module's exports. */
export type MockFactoryWithHelper<M = unknown> = (
  importOriginal: <T extends M = M>() => Promise<T>,
) => Awaitable<Partial<M>>;
export type MockFactory = () => any;
export interface MockOptions {
  /** Spy on the module's exports instead of mocking them. */
  spy?: boolean;
}

/** The `vi.when(spy)` chain. */
export interface WhenChain<T extends Procedure> {
  calledWith(...args: Parameters<T> | any[]): WhenCalledWith<T>;
}
export interface WhenOptions {
  times?: number;
}
export interface WhenCalledWith<T extends Procedure> extends WhenChain<T> {
  thenReturn(value: ReturnType<T>, options?: WhenOptions): WhenCalledWith<T>;
  thenReturnOnce(value: ReturnType<T>): WhenCalledWith<T>;
  thenResolve(
    value: Awaited<ReturnType<T>>,
    options?: WhenOptions,
  ): WhenCalledWith<T>;
  thenResolveOnce(value: Awaited<ReturnType<T>>): WhenCalledWith<T>;
  thenThrow(error: unknown, options?: WhenOptions): WhenCalledWith<T>;
  thenThrowOnce(error: unknown): WhenCalledWith<T>;
  thenReject(error: unknown, options?: WhenOptions): WhenCalledWith<T>;
  thenRejectOnce(error: unknown): WhenCalledWith<T>;
}

export interface FakeTimerInstallOpts {
  now?: number | Date;
  toFake?: string[];
  loopLimit?: number;
  shouldAdvanceTime?: boolean;
  advanceTimeDelta?: number;
  shouldClearNativeTimers?: boolean;
  [key: string]: unknown;
}

export interface RuntimeOptions {
  testTimeout?: number;
  hookTimeout?: number;
  retry?: number;
  fakeTimers?: FakeTimerInstallOpts;
  maxConcurrency?: number;
  [key: string]: unknown;
}

export interface WaitForOptions {
  timeout?: number;
  interval?: number;
}

/** The `vi` object. */
export interface VitestUtils {
  // mocks
  fn<T extends Procedure = Procedure>(implementation?: T): Mock<T>;
  spyOn<T, S extends Properties<Required<T>>>(
    object: T,
    property: S,
    accessType: "get",
  ): MockInstance<() => Required<T>[S]>;
  spyOn<T, S extends Properties<Required<T>>>(
    object: T,
    property: S,
    accessType: "set",
  ): MockInstance<(value: Required<T>[S]) => void>;
  spyOn<T, M extends Classes<Required<T>> | Methods<Required<T>>>(
    object: T,
    method: M,
  ): Required<T>[M] extends
    | (new (...args: infer A) => infer R)
    | ((...args: infer A) => infer R) ? MockInstance<(...args: A) => R>
    : never;
  isMockFunction(fn: unknown): fn is MockInstance;
  mocked<T>(item: T, deep?: false): MaybeMocked<T>;
  mocked<T>(item: T, deep: true): MaybeMockedDeep<T>;
  mocked<T>(
    item: T,
    options: { partial?: boolean; deep?: false },
  ): MaybeMocked<T>;
  mocked<T>(
    item: T,
    options: { partial?: boolean; deep: true },
  ): MaybeMockedDeep<T>;
  mockObject<T>(value: T, options?: MockOptions): MaybeMockedDeep<T>;
  when<T extends Procedure>(
    spy: MockInstance<T>,
    options?: Record<string, unknown>,
  ): WhenChain<T>;
  isWhenChain(value: unknown): boolean;
  clearAllMocks(): VitestUtils;
  resetAllMocks(): VitestUtils;
  restoreAllMocks(): VitestUtils;

  // modules
  /** Mocks the module `path` (hoisted above the imports). */
  mock(path: string, factory?: MockFactoryWithHelper | MockOptions): void;
  mock<T>(
    module: Promise<T>,
    factory?: MockFactoryWithHelper<T> | MockOptions,
  ): void;
  unmock(path: string | Promise<unknown>): void;
  /** Mocks the module `path` for the imports that follow (not hoisted). */
  doMock(
    path: string,
    factory?: MockFactoryWithHelper | MockOptions,
  ): Disposable;
  doMock<T>(
    module: Promise<T>,
    factory?: MockFactoryWithHelper<T> | MockOptions,
  ): Disposable;
  doUnmock(path: string | Promise<unknown>): void;
  hoisted<T>(factory: () => T): T;
  importActual<T = Record<string, any>>(path: string): Promise<T>;
  importMock<T = Record<string, any>>(
    path: string,
  ): Promise<MaybeMockedDeep<T>>;
  resetModules(): VitestUtils;
  dynamicImportSettled(): Promise<void>;

  // timers
  useFakeTimers(config?: FakeTimerInstallOpts): VitestUtils;
  useRealTimers(): VitestUtils;
  isFakeTimers(): boolean;
  runOnlyPendingTimers(): VitestUtils;
  runOnlyPendingTimersAsync(): Promise<VitestUtils>;
  runAllTimers(): VitestUtils;
  runAllTimersAsync(): Promise<VitestUtils>;
  runAllTicks(): VitestUtils;
  advanceTimersByTime(ms: number): VitestUtils;
  advanceTimersByTimeAsync(ms: number): Promise<VitestUtils>;
  advanceTimersToNextTimer(steps?: number): VitestUtils;
  advanceTimersToNextTimerAsync(steps?: number): Promise<VitestUtils>;
  advanceTimersToNextFrame(): VitestUtils;
  getTimerCount(): number;
  clearAllTimers(): VitestUtils;
  setSystemTime(time: number | string | Date): VitestUtils;
  getMockedSystemTime(): Date | null;
  getRealSystemTime(): number;
  setTimerTickMode(mode: string, interval?: number): VitestUtils;

  // globals and env
  stubGlobal(name: string | symbol | number, value: unknown): VitestUtils;
  unstubAllGlobals(): VitestUtils;
  stubEnv<T extends string>(
    name: T,
    value: T extends "PROD" | "DEV" | "SSR" ? boolean : string | undefined,
  ): VitestUtils;
  unstubAllEnvs(): VitestUtils;

  // config and helpers
  setConfig(config: RuntimeOptions): void;
  resetConfig(): void;
  waitFor<T>(
    callback: () => T | Promise<T>,
    options?: number | WaitForOptions,
  ): Promise<T>;
  waitUntil<T>(
    callback: () => T | Promise<T>,
    options?: number | WaitForOptions,
  ): Promise<Exclude<T, false | 0 | "" | null | undefined>>;
  defineHelper<F extends Procedure>(fn: F): F;
}

// --- expect -----------------------------------------------------------------

export interface AsymmetricMatchersContaining {
  stringContaining(expected: string): any;
  objectContaining<T = any>(expected: T): any;
  arrayContaining<T = unknown>(expected: ReadonlyArray<T>): any;
  stringMatching(expected: string | RegExp): any;
  closeTo(expected: number, precision?: number): any;
  schemaMatching(schema: unknown): any;
}

export interface MatcherState {
  assertionCalls: number;
  currentTestName: string | undefined;
  expand?: boolean;
  isNot: boolean;
  promise: string;
  testPath?: string;
  equals(
    a: unknown,
    b: unknown,
    customTesters?: Tester[],
    strict?: boolean,
  ): boolean;
  utils: any;
  [key: string]: any;
}
export interface ExpectationResult {
  pass: boolean;
  message: () => string;
  actual?: unknown;
  expected?: unknown;
}
export type RawMatcherFn = (
  this: MatcherState,
  received: any,
  ...expected: any[]
) => ExpectationResult | Promise<ExpectationResult>;
export type Tester = (
  this: any,
  a: any,
  b: any,
  customTesters: Tester[],
) => boolean | undefined;
export interface ExpectPollOptions {
  interval?: number;
  timeout?: number;
  message?: string;
}

export interface ExpectStatic extends AsymmetricMatchersContaining {
  <T>(actual: T, message?: string): Assertion<T>;
  soft<T>(actual: T, message?: string): Assertion<T>;
  poll<T>(
    actual: () => T,
    options?: ExpectPollOptions,
  ): Promisify<Assertion<Awaited<T>>>;
  unreachable(message?: string): never;
  extend(matchers: Record<string, RawMatcherFn>): void;
  addEqualityTesters(testers: Tester[]): void;
  addSnapshotSerializer(plugin: unknown): void;
  assertions(expected: number): void;
  hasAssertions(): void;
  anything(): any;
  any(constructor: unknown): any;
  getState(): MatcherState;
  setState(state: Partial<MatcherState>): void;
  not: AsymmetricMatchersContaining;
  assert: any;
}

/** Jest's matchers (and Vitest's own). */
export interface JestAssertion<T = any> {
  toEqual<E>(expected: E): void;
  toStrictEqual<E>(expected: E): void;
  toBe<E>(expected: E): void;
  toMatch(expected: string | RegExp): void;
  toMatchObject<E extends object | any[]>(expected: E): void;
  toContain<E>(item: E): void;
  toContainEqual<E>(item: E): void;
  toBeTruthy(): void;
  toBeFalsy(): void;
  toBeGreaterThan(expected: number | bigint): void;
  toBeGreaterThanOrEqual(expected: number | bigint): void;
  toBeLessThan(expected: number | bigint): void;
  toBeLessThanOrEqual(expected: number | bigint): void;
  toBeNaN(): void;
  toBeUndefined(): void;
  toBeNull(): void;
  toBeNullable(): void;
  toBeDefined(): void;
  toBeInstanceOf<E>(expected: E): void;
  toBeTypeOf(
    expected:
      | "bigint"
      | "boolean"
      | "function"
      | "number"
      | "object"
      | "string"
      | "symbol"
      | "undefined",
  ): void;
  toBeOneOf<E>(expected: ReadonlyArray<E> | Set<E>): void;
  toSatisfy<E>(matcher: (value: E) => boolean, message?: string): void;
  toHaveLength(length: number): void;
  toHaveProperty<E>(
    property: string | ReadonlyArray<string | number>,
    value?: E,
  ): void;
  toBeCloseTo(number: number, numDigits?: number): void;
  toThrow(expected?: string | Constructable | RegExp | Error): void;
  toThrowError(expected?: string | Constructable | RegExp | Error): void;

  // mock calls
  toHaveBeenCalled(): void;
  toBeCalled(): void;
  toHaveBeenCalledOnce(): void;
  toHaveBeenCalledTimes(times: number): void;
  toBeCalledTimes(times: number): void;
  toHaveBeenCalledWith<E extends any[]>(...args: E): void;
  toBeCalledWith<E extends any[]>(...args: E): void;
  toHaveBeenCalledExactlyOnceWith<E extends any[]>(...args: E): void;
  toHaveBeenNthCalledWith<E extends any[]>(n: number, ...args: E): void;
  toHaveBeenLastCalledWith<E extends any[]>(...args: E): void;
  toHaveBeenCalledBefore(
    mock: MockInstance<any>,
    failIfNoFirstInvocation?: boolean,
  ): void;
  toHaveBeenCalledAfter(
    mock: MockInstance<any>,
    failIfNoFirstInvocation?: boolean,
  ): void;
  toHaveReturned(): void;
  toReturn(): void;
  toHaveReturnedTimes(times: number): void;
  toReturnTimes(times: number): void;
  toHaveReturnedWith<E>(value: E): void;
  toReturnWith<E>(value: E): void;
  toHaveLastReturnedWith<E>(value: E): void;
  toHaveNthReturnedWith<E>(n: number, value: E): void;
  toHaveResolved(): void;
  toHaveResolvedTimes(times: number): void;
  toHaveResolvedWith<E>(value: E): void;
  toHaveLastResolvedWith<E>(value: E): void;
  toHaveNthResolvedWith<E>(n: number, value: E): void;
  /** Every `vi.when()` behavior of the mock has been used up. */
  toHaveBeenExhausted(): void;

  // snapshots
  toMatchSnapshot<U extends { [P in keyof T]: any }>(
    properties: Partial<U>,
    hint?: string,
  ): void;
  toMatchSnapshot(hint?: string): void;
  toMatchInlineSnapshot<U extends { [P in keyof T]: any }>(
    properties: Partial<U>,
    snapshot?: string,
    hint?: string,
  ): void;
  toMatchInlineSnapshot(snapshot?: string, hint?: string): void;
  toThrowErrorMatchingSnapshot(hint?: string): void;
  toThrowErrorMatchingInlineSnapshot(snapshot?: string, hint?: string): void;
  toMatchFileSnapshot(filepath: string, hint?: string): Promise<void>;
}

/** A callable chai chain (`.a("string")`, `.include(x)`, `.length(2)`). */
export interface ChaiCallable<T> extends Assertion<T> {
  (...args: any[]): Assertion<T>;
}

type NumberLike = number | bigint | Date;

/** Chai's BDD assertions, which Vitest's `expect` also has. */
export interface ChaiAssertion<T = any> {
  // language chains
  to: Assertion<T>;
  be: Assertion<T>;
  been: Assertion<T>;
  is: Assertion<T>;
  that: Assertion<T>;
  which: Assertion<T>;
  and: Assertion<T>;
  has: Assertion<T>;
  have: Assertion<T>;
  with: Assertion<T>;
  at: Assertion<T>;
  of: Assertion<T>;
  same: Assertion<T>;
  but: Assertion<T>;
  does: Assertion<T>;
  still: Assertion<T>;
  also: Assertion<T>;
  // flags
  deep: Assertion<T>;
  nested: Assertion<T>;
  own: Assertion<T>;
  ordered: Assertion<T>;
  any: Assertion<T>;
  all: Assertion<T>;
  itself: Assertion<T>;
  // property assertions
  ok: Assertion<T>;
  true: Assertion<T>;
  false: Assertion<T>;
  null: Assertion<T>;
  undefined: Assertion<T>;
  NaN: Assertion<T>;
  exist: Assertion<T>;
  empty: Assertion<T>;
  arguments: Assertion<T>;
  Arguments: Assertion<T>;
  extensible: Assertion<T>;
  sealed: Assertion<T>;
  frozen: Assertion<T>;
  finite: Assertion<T>;
  // sinon-chai style mock assertions
  called: Assertion<T>;
  calledOnce: Assertion<T>;
  calledTwice: Assertion<T>;
  calledThrice: Assertion<T>;
  // callable chains
  a: ChaiCallable<T>;
  an: ChaiCallable<T>;
  include: ChaiCallable<T>;
  includes: ChaiCallable<T>;
  contain: ChaiCallable<T>;
  contains: ChaiCallable<T>;
  length: ChaiCallable<T>;
  lengthOf: ChaiCallable<T>;
  // methods
  equal(value: unknown, message?: string): Assertion<T>;
  equals(value: unknown, message?: string): Assertion<T>;
  eq(value: unknown, message?: string): Assertion<T>;
  eql(value: unknown, message?: string): Assertion<T>;
  eqls(value: unknown, message?: string): Assertion<T>;
  above(value: NumberLike, message?: string): Assertion<T>;
  gt(value: NumberLike, message?: string): Assertion<T>;
  greaterThan(value: NumberLike, message?: string): Assertion<T>;
  least(value: NumberLike, message?: string): Assertion<T>;
  gte(value: NumberLike, message?: string): Assertion<T>;
  greaterThanOrEqual(value: NumberLike, message?: string): Assertion<T>;
  below(value: NumberLike, message?: string): Assertion<T>;
  lt(value: NumberLike, message?: string): Assertion<T>;
  lessThan(value: NumberLike, message?: string): Assertion<T>;
  most(value: NumberLike, message?: string): Assertion<T>;
  lte(value: NumberLike, message?: string): Assertion<T>;
  lessThanOrEqual(value: NumberLike, message?: string): Assertion<T>;
  within(start: NumberLike, finish: NumberLike, message?: string): Assertion<
    T
  >;
  instanceof(constructor: unknown, message?: string): Assertion<T>;
  instanceOf(constructor: unknown, message?: string): Assertion<T>;
  property(name: string | symbol, ...rest: unknown[]): Assertion<T>;
  ownProperty(name: string | symbol, ...rest: unknown[]): Assertion<T>;
  haveOwnProperty(name: string | symbol, ...rest: unknown[]): Assertion<T>;
  ownPropertyDescriptor(
    name: string | symbol,
    ...rest: unknown[]
  ): Assertion<T>;
  match(regexp: RegExp, message?: string): Assertion<T>;
  matches(regexp: RegExp, message?: string): Assertion<T>;
  string(value: string, message?: string): Assertion<T>;
  keys(...keys: any[]): Assertion<T>;
  key(...keys: any[]): Assertion<T>;
  throw(...args: any[]): Assertion<T>;
  throws(...args: any[]): Assertion<T>;
  Throw(...args: any[]): Assertion<T>;
  respondTo(method: string, message?: string): Assertion<T>;
  respondsTo(method: string, message?: string): Assertion<T>;
  satisfy(matcher: (value: any) => boolean, message?: string): Assertion<T>;
  satisfies(matcher: (value: any) => boolean, message?: string): Assertion<T>;
  closeTo(expected: number, delta: number, message?: string): Assertion<T>;
  approximately(
    expected: number,
    delta: number,
    message?: string,
  ): Assertion<T>;
  members(set: ReadonlyArray<unknown>, message?: string): Assertion<T>;
  oneOf(list: ReadonlyArray<unknown>, message?: string): Assertion<T>;
  change(...args: any[]): Assertion<T>;
  changes(...args: any[]): Assertion<T>;
  increase(...args: any[]): Assertion<T>;
  increases(...args: any[]): Assertion<T>;
  decrease(...args: any[]): Assertion<T>;
  decreases(...args: any[]): Assertion<T>;
  by(delta: number, message?: string): Assertion<T>;
  fail(message?: string): never;
  callCount(times: number): Assertion<T>;
  calledWith(...args: any[]): Assertion<T>;
  calledOnceWith(...args: any[]): Assertion<T>;
  lastCalledWith(...args: any[]): Assertion<T>;
  nthCalledWith(n: number, ...args: any[]): Assertion<T>;
  returned(): Assertion<T>;
  returnedWith(value: unknown): Assertion<T>;
  returnedTimes(times: number): Assertion<T>;
  lastReturnedWith(value: unknown): Assertion<T>;
  nthReturnedWith(n: number, value: unknown): Assertion<T>;
  calledBefore(mock: MockInstance<any>): Assertion<T>;
  calledAfter(mock: MockInstance<any>): Assertion<T>;
}

/**
 * Custom matchers, for a project's `expect.extend()` ones:
 * `declare module "vitest" { interface Matchers<T = any> { toBeFoo(): void } }`
 * (or augment `Assertion` / `AsymmetricMatchersContaining` directly).
 */
export interface Matchers<T = any> {}

/** What `expect(value)` returns. */
export interface Assertion<T = any>
  extends ChaiAssertion<T>, JestAssertion<T>, Matchers<T> {
  not: Assertion<T>;
  resolves: Promisify<Assertion<T>>;
  rejects: Promisify<Assertion<T>>;
}

// --- the test API -----------------------------------------------------------

/** A test or suite of the run (a task of Vitest's runner). */
export interface Task {
  id: string;
  name: string;
  fullName?: string;
  mode: "run" | "skip" | "only" | "todo" | "queued";
  type: "test" | "suite";
  meta: Record<string, any>;
  result?: { state: string; errors?: unknown[]; [key: string]: any };
  suite?: Suite;
  file?: File;
  [key: string]: any;
}
export interface Test extends Task {
  type: "test";
  context: TestContext;
}
export interface Suite extends Task {
  type: "suite";
  tasks: Task[];
}
export interface File extends Suite {
  filepath: string;
}
export type RunnerTestCase = Test;
export type RunnerTestSuite = Suite;
export type RunnerTestFile = File;

/** The context every test function gets. */
export interface TestContext {
  /** The running test. */
  readonly task: Readonly<Test>;
  /** `expect` bound to this test (for concurrent tests' snapshots). */
  expect: ExpectStatic;
  /** Aborted when the test times out or the run is cancelled. */
  signal: AbortSignal;
  /** Skips the test (when `condition` is true). */
  skip(note?: string): never;
  skip(condition: boolean, note?: string): void;
  onTestFailed(fn: OnTestFailedHandler, timeout?: number): void;
  onTestFinished(fn: OnTestFinishedHandler, timeout?: number): void;
  annotate(
    message: string,
    type?: string,
    attachment?: unknown,
  ): Promise<unknown>;
}
export type TaskContext<T = Test> = TestContext & { task: Readonly<T> };
export type ExtendedContext<T> = TestContext & T;

export type OnTestFailedHandler = (context: TestContext) => Awaitable<void>;
export type OnTestFinishedHandler = (context: TestContext) => Awaitable<void>;

export interface TestOptions {
  timeout?: number;
  retry?: number;
  repeats?: number;
  concurrent?: boolean;
  skip?: boolean;
  only?: boolean;
  todo?: boolean;
  fails?: boolean;
  [key: string]: unknown;
}

export type TestFunction<ExtraContext = object> = (
  context: TestContext & ExtraContext,
) => Awaitable<unknown>;
export type SuiteFactory = (test: TestAPI) => Awaitable<void>;

interface TestRegister<ExtraContext> {
  (
    name: string | Function,
    fn?: TestFunction<ExtraContext>,
    options?: number | TestOptions,
  ): void;
  (
    name: string | Function,
    options?: TestOptions,
    fn?: TestFunction<ExtraContext>,
  ): void;
}
interface EachFunctionReturn<T extends any[]> {
  (
    name: string | Function,
    fn: (...args: T) => Awaitable<unknown>,
    options?: number | TestOptions,
  ): void;
  (
    name: string | Function,
    options: TestOptions,
    fn: (...args: T) => Awaitable<unknown>,
  ): void;
}
type ExtractEachCallbackArgs<T extends ReadonlyArray<any>> = {
  1: [T[0]];
  2: [T[0], T[1]];
  3: [T[0], T[1], T[2]];
  4: [T[0], T[1], T[2], T[3]];
  5: [T[0], T[1], T[2], T[3], T[4]];
  6: [T[0], T[1], T[2], T[3], T[4], T[5]];
  7: [T[0], T[1], T[2], T[3], T[4], T[5], T[6]];
  8: [T[0], T[1], T[2], T[3], T[4], T[5], T[6], T[7]];
  9: [T[0], T[1], T[2], T[3], T[4], T[5], T[6], T[7], T[8]];
  10: [T[0], T[1], T[2], T[3], T[4], T[5], T[6], T[7], T[8], T[9]];
  fallback: Array<T extends ReadonlyArray<infer U> ? U : any>;
}[
  T extends Readonly<[any]> ? 1
    : T extends Readonly<[any, any]> ? 2
    : T extends Readonly<[any, any, any]> ? 3
    : T extends Readonly<[any, any, any, any]> ? 4
    : T extends Readonly<[any, any, any, any, any]> ? 5
    : T extends Readonly<[any, any, any, any, any, any]> ? 6
    : T extends Readonly<[any, any, any, any, any, any, any]> ? 7
    : T extends Readonly<[any, any, any, any, any, any, any, any]> ? 8
    : T extends Readonly<[any, any, any, any, any, any, any, any, any]> ? 9
    : T extends Readonly<[any, any, any, any, any, any, any, any, any, any]>
      ? 10
    : "fallback"
];
export interface TestEachFunction {
  <T extends any[] | [any]>(cases: ReadonlyArray<T>): EachFunctionReturn<T>;
  <T extends ReadonlyArray<any>>(
    cases: ReadonlyArray<T>,
  ): EachFunctionReturn<ExtractEachCallbackArgs<T>>;
  <T>(cases: ReadonlyArray<T>): EachFunctionReturn<T[]>;
  (...args: [TemplateStringsArray, ...any]): EachFunctionReturn<any[]>;
}
export interface TestForFunctionReturn<Arg, ExtraContext> {
  (
    name: string | Function,
    fn: (arg: Arg, context: TestContext & ExtraContext) => Awaitable<unknown>,
  ): void;
  (
    name: string | Function,
    options: TestOptions,
    fn: (arg: Arg, context: TestContext & ExtraContext) => Awaitable<unknown>,
  ): void;
}
export interface TestForFunction<ExtraContext> {
  <T>(cases: ReadonlyArray<T>): TestForFunctionReturn<T, ExtraContext>;
  (
    strings: TemplateStringsArray,
    ...values: any[]
  ): TestForFunctionReturn<any, ExtraContext>;
}

export interface FixtureOptions {
  auto?: boolean;
  scope?: "test" | "file" | "worker";
  injected?: boolean;
  [key: string]: unknown;
}
export type FixtureFn<V, Context> = (
  context: TestContext & Context,
  use: (value: V) => Promise<void>,
) => Promise<void>;
export type Fixture<V, Context> =
  | V
  | FixtureFn<V, Context>
  | [V | FixtureFn<V, Context>, FixtureOptions];
export type Fixtures<T extends Record<string, any>, ExtraContext = object> = {
  [K in keyof T]: Fixture<T[K], ExtraContext & T>;
};
export interface FixtureHelpers {
  onCleanup(fn: () => Awaitable<void>): void;
}
type FixtureBuilder<V, Context> = (
  context: TestContext & Context,
  helpers: FixtureHelpers,
) => Awaitable<V>;

export interface TestAPI<ExtraContext = object>
  extends TestRegister<ExtraContext> {
  each: TestEachFunction;
  for: TestForFunction<ExtraContext>;
  skip: TestAPI<ExtraContext>;
  only: TestAPI<ExtraContext>;
  todo: TestAPI<ExtraContext>;
  concurrent: TestAPI<ExtraContext>;
  fails: TestAPI<ExtraContext>;
  skipIf(condition: unknown): TestAPI<ExtraContext>;
  runIf(condition: unknown): TestAPI<ExtraContext>;
  /** A test API whose tests get more fixtures in their context. */
  extend<K extends string, V>(
    name: K,
    fn: FixtureBuilder<V, ExtraContext>,
  ): TestAPI<ExtraContext & { [P in K]: V }>;
  extend<K extends string, V>(
    name: K,
    options: FixtureOptions,
    fn: FixtureBuilder<V, ExtraContext>,
  ): TestAPI<ExtraContext & { [P in K]: V }>;
  extend<T extends Record<string, any> = object>(
    fixtures: Fixtures<T, ExtraContext>,
  ): TestAPI<ExtraContext & T>;
  override(
    fixtures: Partial<Fixtures<Record<string, any>, ExtraContext>>,
  ): TestAPI<ExtraContext>;
  scoped(
    fixtures: Partial<Fixtures<Record<string, any>, ExtraContext>>,
  ): TestAPI<ExtraContext>;
  describe: SuiteAPI;
  suite: SuiteAPI;
  beforeEach(fn: BeforeEachListener<ExtraContext>, timeout?: number): void;
  afterEach(fn: AfterEachListener<ExtraContext>, timeout?: number): void;
  beforeAll(fn: BeforeAllListener, timeout?: number): void;
  afterAll(fn: AfterAllListener, timeout?: number): void;
  aroundEach(fn: AroundEachListener<ExtraContext>, timeout?: number): void;
  aroundAll(fn: AroundAllListener, timeout?: number): void;
}

interface SuiteRegister {
  (
    name: string | Function,
    fn?: SuiteFactory,
    options?: number | TestOptions,
  ): void;
  (name: string | Function, options: TestOptions, fn?: SuiteFactory): void;
}
interface SuiteEachReturn<T extends any[]> {
  (
    name: string | Function,
    fn: (...args: T) => Awaitable<void>,
    options?: number | TestOptions,
  ): void;
  (
    name: string | Function,
    options: TestOptions,
    fn: (...args: T) => Awaitable<void>,
  ): void;
}
export interface SuiteEachFunction {
  <T extends any[] | [any]>(cases: ReadonlyArray<T>): SuiteEachReturn<T>;
  <T extends ReadonlyArray<any>>(
    cases: ReadonlyArray<T>,
  ): SuiteEachReturn<ExtractEachCallbackArgs<T>>;
  <T>(cases: ReadonlyArray<T>): SuiteEachReturn<T[]>;
  (...args: [TemplateStringsArray, ...any]): SuiteEachReturn<any[]>;
}
export interface SuiteForFunction {
  <T>(cases: ReadonlyArray<T>): (
    name: string | Function,
    fn: (arg: T) => Awaitable<void>,
  ) => void;
}
export interface SuiteAPI extends SuiteRegister {
  each: SuiteEachFunction;
  for: SuiteForFunction;
  skip: SuiteAPI;
  only: SuiteAPI;
  todo: SuiteAPI;
  concurrent: SuiteAPI;
  shuffle: SuiteAPI;
  skipIf(condition: unknown): SuiteAPI;
  runIf(condition: unknown): SuiteAPI;
}

// --- hooks ------------------------------------------------------------------

export type BeforeAllListener = (
  suite: Readonly<Suite | File>,
) => Awaitable<unknown>;
export type AfterAllListener = (
  suite: Readonly<Suite | File>,
) => Awaitable<unknown>;
export type BeforeEachListener<ExtraContext = object> = (
  context: TestContext & ExtraContext,
  suite: Readonly<Suite>,
) => Awaitable<unknown>;
export type AfterEachListener<ExtraContext = object> = (
  context: TestContext & ExtraContext,
  suite: Readonly<Suite>,
) => Awaitable<unknown>;
export type AroundEachListener<ExtraContext = object> = (
  runTest: () => Promise<void>,
  context: TestContext & ExtraContext,
) => Awaitable<unknown>;
export type AroundAllListener = (
  runSuite: () => Promise<void>,
  suite: Readonly<Suite | File>,
) => Awaitable<unknown>;

// --- the module's exports ---------------------------------------------------

export declare const describe: SuiteAPI;
export declare const suite: SuiteAPI;
export declare const it: TestAPI;
export declare const test: TestAPI;
export declare function beforeAll(
  fn: BeforeAllListener,
  timeout?: number,
): void;
export declare function afterAll(fn: AfterAllListener, timeout?: number): void;
export declare function beforeEach<ExtraContext = object>(
  fn: BeforeEachListener<ExtraContext>,
  timeout?: number,
): void;
export declare function afterEach<ExtraContext = object>(
  fn: AfterEachListener<ExtraContext>,
  timeout?: number,
): void;
export declare function aroundEach<ExtraContext = object>(
  fn: AroundEachListener<ExtraContext>,
  timeout?: number,
): void;
export declare function aroundAll(
  fn: AroundAllListener,
  timeout?: number,
): void;
export declare function onTestFailed(
  fn: OnTestFailedHandler,
  timeout?: number,
): void;
export declare function onTestFinished(
  fn: OnTestFinishedHandler,
  timeout?: number,
): void;
export declare const expect: ExpectStatic;
export declare const vi: VitestUtils;
export declare const vitest: VitestUtils;
export declare function createExpect(test?: unknown): ExpectStatic;
export declare function assertType<T>(value: T): void;
export declare function inject<T = any>(key: string): T;
// Typed loosely: chai's own API, and Vitest's type-level testing.
export declare const chai: any;
export declare const assert: any;
export declare const should: any;
export declare const expectTypeOf: any;
export declare const recordArtifact: any;
export declare const TestRunner: any;
export declare const Snapshots: any;
export declare const EvaluatedModules: any;
export type ExpectTypeOf<_T = any> = any;
