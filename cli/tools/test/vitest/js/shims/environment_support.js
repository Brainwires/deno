// Copyright 2018-2026 the Deno authors. MIT license.

// Helpers that let Vitest's vendored test environments (jsdom, happy-dom)
// run from a `deno-vitest:` module.

/** The base `createRequire()` resolves the project's own packages from. */
export function denoVitestRootRequireBase() {
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
