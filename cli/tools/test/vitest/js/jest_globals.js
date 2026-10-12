// Copyright 2018-2026 the Deno authors. MIT license.

// The `@jest/globals` module, mapped onto the Vitest implementation (see
// jest.js).

import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  jest,
  test,
} from "./jest.js";

export {
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

export const fdescribe = describe.only;
export const fit = it.only;
export const xdescribe = describe.skip;
export const xit = it.skip;
export const xtest = test.skip;

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
