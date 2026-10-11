// Copyright 2018-2026 the Deno authors. MIT license.

// Fallback types for `vitest` under `deno test`, used only when the vitest
// package itself (and its types) is not installed. Install vitest as a dev
// dependency for its full types.

// deno-lint-ignore-file no-explicit-any

// Callbacks get `any` parameters from these shapes, rather than implicit
// `any`s that a strict type check rejects.
type Callback = (...args: any[]) => any;
type Each<R> = (cases: any, ...values: any[]) => (
  name: any,
  optionsOrFn?: any,
  fn?: Callback | number,
) => R;
interface TestAPI {
  (name: any, optionsOrFn?: any, fn?: Callback | number): void;
  each: Each<void>;
  for: Each<void>;
  skip: TestAPI;
  only: TestAPI;
  todo: TestAPI;
  concurrent: TestAPI;
  sequential: TestAPI;
  fails: TestAPI;
  skipIf(condition: any): TestAPI;
  runIf(condition: any): TestAPI;
  extend(fixtures: any): TestAPI;
  scoped(fixtures: any): void;
}
interface SuiteAPI {
  (name: any, optionsOrFn?: any, fn?: Callback | number): void;
  each: Each<void>;
  for: Each<void>;
  skip: SuiteAPI;
  only: SuiteAPI;
  todo: SuiteAPI;
  concurrent: SuiteAPI;
  sequential: SuiteAPI;
  shuffle: SuiteAPI;
  skipIf(condition: any): SuiteAPI;
  runIf(condition: any): SuiteAPI;
}
type Hook = (fn: Callback, timeout?: number) => void;

export declare const describe: SuiteAPI;
export declare const suite: SuiteAPI;
export declare const it: TestAPI;
export declare const test: TestAPI;
export declare const beforeAll: Hook;
export declare const afterAll: Hook;
export declare const beforeEach: Hook;
export declare const afterEach: Hook;
export declare const aroundAll: Hook;
export declare const aroundEach: Hook;
export declare const onTestFailed: Hook;
export declare const onTestFinished: Hook;
export declare const expect: any;
export declare const vi: any;
export declare const vitest: any;
export declare const chai: any;
export declare const assert: any;
export declare const should: any;
export declare const expectTypeOf: any;
export declare const assertType: any;
export declare const createExpect: any;
export declare const inject: any;
export declare const recordArtifact: any;
export declare const TestRunner: any;
export declare const Snapshots: any;
export declare const EvaluatedModules: any;

export type Mock<T = any> = any;
export type MockInstance<T = any> = any;
export type MockedFunction<T = any> = any;
export type MockedClass<T = any> = any;
export type Mocked<T> = T;
export type MockedObject<T> = T;
export type MaybeMocked<T> = T;
export type MaybeMockedDeep<T> = T;
export type TestContext = any;
export type TaskContext<T = any> = any;
export type TestFunction = any;
export type Test = any;
export type Suite = any;
export type File = any;
export type Task = any;
export type RunnerTestCase = any;
export type ExpectStatic = any;
export type Assertion<T = any> = any;
export type AsymmetricMatchersContaining = any;
export type Awaitable<T> = T | PromiseLike<T>;
