// Copyright 2018-2026 the Deno authors. MIT license.

// Types for `vitest/globals` under `deno test`, `deno check` and the LSP, used
// when the vitest package itself is not installed: the globals a Vitest config
// with `globals: true` installs (Vitest's `globalApis`), typed like the
// `vitest` module's exports (see vitest.d.ts). Reference them with
// `/// <reference types="vitest/globals" />` or `compilerOptions.types`.

import type * as v from "./vitest.d.ts";

declare global {
  const suite: typeof v.suite;
  const test: typeof v.test;
  const describe: typeof v.describe;
  const it: typeof v.it;
  const chai: typeof v.chai;
  const expect: typeof v.expect;
  const assert: typeof v.assert;
  const expectTypeOf: typeof v.expectTypeOf;
  const assertType: typeof v.assertType;
  const vitest: typeof v.vitest;
  const vi: typeof v.vi;
  const beforeAll: typeof v.beforeAll;
  const afterAll: typeof v.afterAll;
  const beforeEach: typeof v.beforeEach;
  const afterEach: typeof v.afterEach;
  const onTestFinished: typeof v.onTestFinished;
  const onTestFailed: typeof v.onTestFailed;
  const aroundEach: typeof v.aroundEach;
  const aroundAll: typeof v.aroundAll;
}

export {};
