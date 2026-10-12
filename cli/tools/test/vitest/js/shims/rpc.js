// Copyright 2018-2026 the Deno authors. MIT license.

// Stands in for Vitest's RPC channel to its main process. Under `deno test`
// each test file reports through the Deno test runner instead, so every
// remote call resolves to `undefined`.

const noopRpc = new Proxy({}, {
  get(_target, name) {
    if (name === "then") return undefined;
    return () => Promise.resolve(undefined);
  },
});

function rpc() {
  return noopRpc;
}

/**
 * Vitest tracks evaluated modules for `vi.resetModules()`. Deno owns the
 * module registry, so this map stays empty.
 */
class VitestEvaluatedModules {
  idToModuleMap = new Map();
  getModuleSourceMapById() {
    return null;
  }
}

// Vitest's chunks import these under their minified names.
export { rpc as r, VitestEvaluatedModules as V };
