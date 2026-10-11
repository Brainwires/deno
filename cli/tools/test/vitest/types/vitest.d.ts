// Copyright 2018-2026 the Deno authors. MIT license.

// Fallback types for `vitest` under `deno test`, used only when the vitest
// package itself (and its types) is not installed. Install vitest as a dev
// dependency for its full types.

// deno-lint-ignore-file no-explicit-any

export declare const describe: any;
export declare const suite: any;
export declare const it: any;
export declare const test: any;
export declare const beforeAll: any;
export declare const afterAll: any;
export declare const beforeEach: any;
export declare const afterEach: any;
export declare const aroundAll: any;
export declare const aroundEach: any;
export declare const onTestFailed: any;
export declare const onTestFinished: any;
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
