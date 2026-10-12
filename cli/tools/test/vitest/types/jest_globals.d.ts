// Copyright 2018-2026 the Deno authors. MIT license.

// Fallback types for `@jest/globals` under `deno test`, used only when the
// package itself is not installed.

// deno-lint-ignore-file no-explicit-any

export {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  it,
  test,
} from "./vitest.d.ts";
export declare const expect: any;
export declare const jest: any;
export declare const fdescribe: any;
export declare const fit: any;
export declare const xdescribe: any;
export declare const xit: any;
export declare const xtest: any;
declare const globals: any;
export default globals;
