// Copyright 2018-2026 the Deno authors. MIT license.

// denext fork (Brainwires/deno): 50 more than upstream's 44, the
// `Deno.desktop` ops the fork adds to NOT_IMPORTED_OPS in 99_main.js (launch
// targets and events, scheme registration, passkeys, the rich clipboard, drag
// out, file dialogs, shortcuts, launch at login, DevTools, menus, scheduled
// notifications, screens, window capabilities, quit / close, auth sessions,
// runOnMainThread, Deno.desktop.updater, and op_desktop_alert_async, which
// the uncaught-error handler needs). The desktop JS that cli/rt evaluates
// after bootstrap reaches them through `core.ops`, so they survive
// removeImportedOps() like upstream's own desktop ops.
const EXPECTED_OP_COUNT = 94;
// The main scope minus WORKER_EXCLUDED_OPS in 99_main.js: upstream strips the
// two text-clipboard ops from workers, and the fork strips every new
// main-scope-only desktop op (47) and the native classes that need the
// desktop backend (BrowserWindow, Dock, Tray, Notification), leaving
// upstream's 20 minus those 4, plus op_desktop_screens,
// op_desktop_window_capabilities (both read-only) and op_desktop_alert_async.
const EXPECTED_WORKER_OP_COUNT = 19;

function getExposedOpNames(): string[] {
  // @ts-ignore TS doesn't allow to index with symbol
  const core = Deno[Deno.internal].core;
  return Object.keys(core.ops);
}

Deno.test(function checkExposedOps() {
  const opNames = getExposedOpNames();

  if (opNames.length !== EXPECTED_OP_COUNT) {
    throw new Error(
      `Expected ${EXPECTED_OP_COUNT} ops, but got ${opNames.length}:\n${
        opNames.join("\n")
      }`,
    );
  }

  if (!opNames.includes("op_desktop_verify_ed25519")) {
    throw new Error("Desktop update signature verification op is not exposed");
  }
});
Deno.test(function internalCoreOnlyHidesExtensionLoaders() {
  // @ts-ignore TS doesn't allow to index with symbol
  const core = Deno[Deno.internal].core;

  for (const name of ["createLazyLoader", "loadExtScript"]) {
    if (name in core) {
      throw new Error(`${name} should not be exposed`);
    }
  }

  for (const name of ["close", "read", "readAll"]) {
    if (typeof core[name] !== "function") {
      throw new Error(`${name} should remain exposed`);
    }
  }
});

Deno.test(async function workerDoesNotExposeImportedOps() {
  const mainOpNames = getExposedOpNames();
  const worker = new Worker(
    `data:application/javascript,${
      encodeURIComponent(`
        // @ts-ignore TS doesn't allow to index with symbol
        const core = Deno[Deno.internal].core;
        postMessage(Object.keys(core.ops));
      `)
    }`,
    { type: "module" },
  );
  let actualOpNames: string[];
  try {
    actualOpNames = await new Promise((resolve, reject) => {
      worker.onmessage = (event) => resolve(event.data);
      worker.onerror = (event) => reject(event.error);
    });
  } finally {
    worker.terminate();
  }
  if (
    actualOpNames.length !== EXPECTED_WORKER_OP_COUNT ||
    actualOpNames.some((opName) => !mainOpNames.includes(opName))
  ) {
    throw new Error(
      `Unexpected worker ops:\n${actualOpNames.join("\n")}`,
    );
  }
});
