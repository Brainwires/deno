// Copyright 2018-2026 the Deno authors. MIT license.

// Vitest's benchmark fixture is built on tinybench. Benchmarks are run by
// `deno bench`, not by the Vitest compatibility layer of `deno test`.
export class Bench {
  constructor() {
    throw new Error(
      "Vitest benchmarks are not supported by `deno test`; use `deno bench`.",
    );
  }
}
