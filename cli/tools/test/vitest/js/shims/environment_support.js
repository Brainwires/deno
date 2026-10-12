// Copyright 2018-2026 the Deno authors. MIT license.

// Helpers that let Vitest's vendored test environments (jsdom, happy-dom)
// run from a `deno-vitest:` module.

/**
 * The base `createRequire()` resolves the project's own packages from (for
 * jsdom's internals: where jsdom resolves from, which for a Jest project
 * can be its environment package, see jest_config.js).
 */
export function denoVitestRootRequireBase() {
  const jsdomBase = globalThis[Symbol.for("deno.vitest.packageBases")]?.jsdom;
  if (jsdomBase) return jsdomBase;
  const root = globalThis.__vitest_worker__?.config?.root;
  return root ? root.replace(/\/?$/, "/") : Deno.cwd() + "/";
}

/**
 * An `error` event nobody listens to on the DOM window is an unhandled error
 * in the test file; report it the way Deno reports any uncaught exception.
 */
export function denoVitestReportUnhandledError(error) {
  queueMicrotask(() => {
    throw error;
  });
}
