// Copyright 2018-2026 the Deno authors. MIT license.

// The `vitest` module as seen by test files under `deno test`.

import { i as index } from "./vendor/vitest_core.js";
import * as chai from "./vendor/chai.js";

export const {
  EvaluatedModules,
  Snapshots,
  TestRunner,
  afterAll,
  afterEach,
  aroundAll,
  aroundEach,
  assert,
  assertType,
  beforeAll,
  beforeEach,
  createExpect,
  describe,
  expect,
  expectTypeOf,
  inject,
  it,
  onTestFailed,
  onTestFinished,
  recordArtifact,
  should,
  suite,
  test,
  vi,
  vitest,
} = index;

export { chai };
