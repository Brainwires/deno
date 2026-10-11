// Copyright 2018-2026 the Deno authors. MIT license.

// The bridge between Vitest's own runner (vendored from vitest, see
// ./vendor) and `deno test`.
//
// A test file is collected and run by Vitest's runner exactly as `vitest`
// would, so hooks, fixtures, `.each`/`.for`, `.concurrent`, retries, `.fails`,
// `expect` and `vi` keep Vitest's semantics. Every collected test is also
// registered with `Deno.test()` so Deno's reporters, `--filter`, `--fail-fast`
// and summaries see one Deno test per Vitest test. The Deno test function for
// a test is a gate: Vitest's runner waits in `onBeforeRunTask` until Deno
// invokes that test, and the Deno test function waits until Vitest's runner
// has finished with it (and has run the hooks between it and the next test).

// Vitest's chunks export their bindings under minified names.
import { s as startTests } from "./vendor/runner.js";
import {
  c as createExpect,
  g as globalExpect,
  i as vitestIndex,
  T as TestRunner,
  v as vi,
} from "./vendor/vitest_core.js";
import { getNames } from "./vendor/task_utils.js";
import { s as setSafeTimers } from "./vendor/source_map.js";
import { g as globalApis } from "./vendor/constants.js";
import { environments } from "./vendor/environments.js";
import { DenoSnapshotEnvironment } from "./snapshot_environment.js";

const ORIGINAL_ERROR = Symbol.for("deno.vitest.originalError");
const TEST_SKIP = Symbol.for("Deno.test.skip");
const TEST_LOCATION = Symbol.for("Deno.test.location");
const MODULE_IMPORTED = Symbol.for("deno.vitest.moduleImported");

const { fileURLToPath, pathToFileURL } = await import("node:url");

/** Phase 2 work: module mocking needs loader support in the CLI. */
function moduleMockingUnsupported(api) {
  return function unsupported(path) {
    const site = callSite(unsupported);
    const what = path === undefined
      ? `vi.${api}()`
      : `vi.${api}(${JSON.stringify(String(path))})`;
    const error = new Error(
      `${what} is not supported yet by \`deno test\`'s Vitest compatibility: ` +
        `module mocking is not implemented${
          site ? ` (called at ${site})` : ""
        }.`,
    );
    error.name = "VitestCompatError";
    throw error;
  };
}

function callSite(fn) {
  const holder = {};
  Error.captureStackTrace(holder, fn);
  for (const line of String(holder.stack).split("\n").slice(1)) {
    const match = /\(?((?:file|https?|npm|jsr):[^\s()]+:\d+:\d+)\)?\s*$/
      .exec(line);
    if (match) return match[1];
  }
  return undefined;
}

function installModuleMockingStubs() {
  for (const api of ["mock", "doMock", "unmock", "doUnmock", "importMock"]) {
    vi[api] = moduleMockingUnsupported(api);
  }
  vi.resetModules = moduleMockingUnsupported("resetModules");
  vi.importActual = async function importActual(path) {
    const site = callSite(importActual);
    const base = site ? site.replace(/:\d+:\d+$/, "") : undefined;
    const specifier = path.startsWith(".") || path.startsWith("/")
      ? new URL(path, base ?? pathToFileURL(Deno.cwd() + "/")).href
      : path;
    return await import(specifier);
  };
  vi.dynamicImportSettled = async function dynamicImportSettled() {
    for (let i = 0; i < 3; i++) {
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
  };
  // `vi.hoisted()` only hoists the factory above imports; without module
  // mocks nothing depends on that ordering.
  globalThis.__vitest_mocker__ = new Proxy({}, {
    get(_target, name) {
      return moduleMockingUnsupported(String(name));
    },
  });
}

function createDeferred() {
  let resolve;
  let reject;
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function testFullName(test) {
  return getNames(test).slice(1).join(" > ");
}

/**
 * Synchronizes Vitest's runner with the order in which `deno test` invokes
 * the registered tests.
 */
class DenoBridge {
  /** @type {Map<string, { test: any, gate: ReturnType<typeof createDeferred>, released: boolean, skipped: boolean }>} */
  entries = new Map();
  order = [];
  waiting = new Set();
  active = 0;
  finished = false;
  #idleWaiters = [];
  /** Suite-level errors (hooks, collection) not yet surfaced. */
  pendingErrors = [];

  track(test) {
    const entry = {
      test,
      gate: createDeferred(),
      released: false,
      skipped: false,
    };
    this.entries.set(test.id, entry);
    this.order.push(entry);
    return entry;
  }

  /** Called by the runner before it runs `test`. */
  async gate(test) {
    const entry = this.entries.get(test.id);
    if (entry === undefined) return;
    if (!entry.released) {
      this.waiting.add(entry);
      this.#notify();
      await entry.gate.promise;
      this.waiting.delete(entry);
    }
    if (entry.skipped) test.mode = "skip";
  }

  /** Called when the runner is done with a task (ran or skipped it). */
  settled(test) {
    const entry = this.entries.get(test.id);
    if (entry === undefined || entry.settled) return;
    entry.settled = true;
    if (entry.counted) {
      entry.counted = false;
      this.active--;
    }
    this.#notify();
  }

  release(entry) {
    // Tests Deno did not invoke before this one (filtered out) are skipped.
    for (const other of this.order) {
      if (other === entry) break;
      if (!other.released) this.#skip(other);
    }
    if (!entry.released) {
      entry.released = true;
      if (!entry.settled) {
        entry.counted = true;
        this.active++;
      }
      entry.gate.resolve();
    }
  }

  /** Whether some test under `suite` has not been invoked by Deno yet. */
  hasUnreleasedTests(suite) {
    for (const task of suite.tasks ?? []) {
      if (task.type === "suite" && this.hasUnreleasedTests(task)) return true;
      const entry = this.entries.get(task.id);
      if (entry !== undefined && !entry.released) return true;
    }
    return false;
  }

  releaseAll() {
    for (const entry of this.order) {
      if (!entry.released) this.#skip(entry);
    }
  }

  #skip(entry) {
    entry.released = true;
    entry.skipped = true;
    entry.gate.resolve();
  }

  done() {
    this.finished = true;
    this.#notify();
  }

  isIdle() {
    if (this.finished) return true;
    if (this.active > 0) return false;
    for (const entry of this.waiting) {
      if (!entry.released) return true;
    }
    return false;
  }

  untilIdle() {
    if (this.isIdle()) return Promise.resolve();
    const deferred = createDeferred();
    this.#idleWaiters.push(deferred);
    return deferred.promise;
  }

  #notify() {
    if (!this.isIdle()) return;
    const waiters = this.#idleWaiters;
    this.#idleWaiters = [];
    for (const waiter of waiters) waiter.resolve();
  }
}

class DenoTestRunner extends TestRunner {
  #environmentReady;

  constructor(config, bridge, setupEnvironment, onCollected) {
    super(config);
    this.bridge = bridge;
    this.denoSetupEnvironment = setupEnvironment;
    this.denoOnCollected = onCollected;
    this.moduleRunner = { import: (path) => import(toImportSpecifier(path)) };
  }

  trace = (_name, attributes, cb) => (cb ?? attributes)();

  async importFile(filepath, source) {
    this.#environmentReady ??= this.denoSetupEnvironment();
    await this.#environmentReady;
    const promise = import(toImportSpecifier(filepath));
    if (source !== "collect") return await promise;
    return await promise.catch((error) => {
      this.moduleEvaluationFailed = true;
      throw error;
    });
  }

  async onCollected(files) {
    await this.denoOnCollected(files);
  }

  async onBeforeRunTask(test) {
    await super.onBeforeRunTask(test);
    if (test.mode === "run" || test.mode === "queued") {
      await this.bridge.gate(test);
    }
  }

  onDenoTaskUpdate(event, task) {
    if (task.type !== "test") {
      if (
        (event === "suite-finished" || event === "suite-failed-early") &&
        task.result?.state === "fail" && task.result.errors?.length
      ) {
        this.bridge.pendingErrors.push(task);
      }
      return;
    }
    if (event === "test-finished" || event === "test-failed-early") {
      this.bridge.settled(task);
    }
  }
}

function toImportSpecifier(path) {
  if (/^[a-z][a-z0-9+.-]*:/i.test(path) && !/^[a-z]:[\\/]/i.test(path)) {
    return path;
  }
  return pathToFileURL(path).href;
}

function toFilePath(specifier) {
  return specifier.startsWith("file:") ? fileURLToPath(specifier) : specifier;
}

/** Whether the first frame of `stack` is outside the built-in modules. */
function startsInUserCode(stack) {
  const frame = String(stack ?? "").split("\n").find((line) =>
    /^\s+at /.test(line)
  );
  return frame !== undefined && !frame.includes(`deno-vitest:`) &&
    /(file|https?|npm|jsr):/.test(frame);
}

function messageOf(processed) {
  const message = String(processed.message ?? processed);
  return processed.diff && !message.includes(processed.diff)
    ? `${message}\n\n${processed.diff}`
    : message;
}

/**
 * Turns Vitest's recorded errors for a task into one throwable error. The
 * original error is thrown when it was raised in user code, so Deno shows
 * its source location; errors raised by Vitest itself (timeouts, `.fails`,
 * assertion counts) are rethrown with just their message.
 */
function toThrowable(errors, prefix = "") {
  const first = errors[0];
  const message = prefix + errors.map(messageOf).join("\n\n");
  const original = first[ORIGINAL_ERROR];
  if (original instanceof Error && startsInUserCode(original.stack)) {
    try {
      original.message = message;
      return original;
    } catch {
      // frozen or exotic error: fall through
    }
  }
  const error = new Error(message);
  if (first.name && first.name !== "Error") {
    Object.defineProperty(error, "name", {
      value: first.name,
      configurable: true,
      writable: true,
    });
  }
  return error;
}

function skipSignal(note) {
  const error = new Error(note ? `skipped: ${note}` : "skipped");
  error[TEST_SKIP] = true;
  return error;
}

/** Surfaces suite-level failures recorded since the last test. */
function takePendingSuiteError(bridge) {
  if (!bridge.pendingErrors.length) return undefined;
  // A suite whose tests have not run yet (a failed `beforeAll`) reports its
  // error on its own first test instead.
  const suites = [];
  bridge.pendingErrors = bridge.pendingErrors.filter((suite) => {
    if (suite.denoReported) return false;
    if (bridge.hasUnreleasedTests(suite)) return true;
    suites.push(suite);
    return false;
  });
  const errors = [];
  for (const suite of suites) {
    suite.denoReported = true;
    errors.push(...suite.result.errors);
  }
  if (!errors.length) return undefined;
  const names = suites.map((s) => getNames(s).slice(1).join(" > ") || s.name);
  return toThrowable(
    errors,
    `Error in hook or setup of ${names.join(", ")}: `,
  );
}

/** Finds the nearest suite whose failure skipped `test`. */
function failedAncestor(test) {
  let suite = test.suite;
  while (suite) {
    if (suite.result?.state === "fail" && suite.result.errors?.length) {
      return suite;
    }
    suite = suite.suite;
  }
  const file = test.file;
  if (file?.result?.state === "fail" && file.result.errors?.length) return file;
  return undefined;
}

/** Mirrors `deno test --filter`: a substring, or a `/regex/`. */
function createNameFilter(filter) {
  if (!filter) return undefined;
  if (filter.length > 1 && filter.startsWith("/") && filter.endsWith("/")) {
    try {
      const regex = new RegExp(filter.slice(1, -1));
      return (name) => regex.test(name);
    } catch {
      // Deno falls back to a substring match
    }
  }
  return (name) => name.includes(filter);
}

/**
 * Skips the tests `--filter` leaves out before Vitest runs the file, so it
 * also skips the hooks of suites without a selected test (Deno only knows
 * which tests it runs).
 */
function applyNameFilter(tasks, matches) {
  let anyRuns = false;
  for (const task of tasks) {
    if (task.type === "suite") {
      if (applyNameFilter(task.tasks, matches)) {
        anyRuns = true;
      } else if (task.mode === "run" || task.mode === "queued") {
        task.mode = "skip";
      }
    } else if (task.type === "test") {
      if (task.mode !== "run" && task.mode !== "queued") continue;
      if (matches(testFullName(task))) {
        anyRuns = true;
      } else {
        // still registered with Deno, which reports it as filtered out
        task.denoFilteredOut = true;
      }
    }
  }
  return anyRuns;
}

function registerDenoTests(files, bridge) {
  for (const file of files) {
    registerTasks(file.tasks, bridge);
  }
}

function registerTasks(tasks, bridge) {
  for (const task of tasks) {
    if (task.type === "suite") {
      registerTasks(task.tasks, bridge);
      continue;
    }
    if (task.type !== "test") continue;
    registerTest(task, bridge);
  }
}

function registerTest(test, bridge) {
  const name = testFullName(test);
  const runnable = test.mode === "run" || test.mode === "queued" ||
    test.denoFilteredOut === true;
  const entry = runnable ? bridge.track(test) : undefined;
  const fn = async () => {
    bridge.release(entry);
    await bridge.untilIdle();
    const result = test.result;
    const suiteError = takePendingSuiteError(bridge);
    if (result?.state === "fail" && result.errors?.length) {
      throw toThrowable(result.errors);
    }
    if (result?.state === "skip" || test.mode === "skip") {
      const failed = failedAncestor(test);
      if (failed && !failed.denoReported) {
        failed.denoReported = true;
        throw toThrowable(
          failed.result.errors,
          `Error in hook or setup of ${testFullName(failed) || failed.name}: `,
        );
      }
      if (suiteError) throw suiteError;
      throw skipSignal(result?.note);
    }
    if (suiteError) throw suiteError;
  };
  if (test.location) {
    fn[TEST_LOCATION] = `${
      pathToFileURL(test.file.filepath).href
    }:${test.location.line}:${test.location.column}`;
  }
  Deno.test({
    name,
    fn,
    ignore: !runnable,
    sanitizeOps: false,
    sanitizeResources: false,
  });
}

function setEnv(name, value) {
  try {
    process.env[name] = value;
  } catch {
    // no --allow-env: import.meta.env still gets the value
  }
}

/** Mirrors Vitest's `import.meta.env`: a view over `process.env`. */
function createImportMetaEnv() {
  const booleanKeys = ["DEV", "PROD", "SSR"];
  const overlay = Object.create(null);
  const read = (key) => {
    if (key in overlay) return overlay[key];
    try {
      return process.env[key];
    } catch {
      return undefined;
    }
  };
  return new Proxy(overlay, {
    get(_target, key) {
      if (typeof key !== "string") return undefined;
      if (booleanKeys.includes(key)) return !!read(key);
      return read(key);
    },
    set(_target, key, value) {
      if (typeof key !== "string") return true;
      const stored = booleanKeys.includes(key) ? (value ? "1" : "") : value;
      try {
        process.env[key] = stored;
        delete overlay[key];
      } catch {
        overlay[key] = stored;
      }
      return true;
    },
    deleteProperty(_target, key) {
      if (typeof key !== "string") return true;
      delete overlay[key];
      try {
        delete process.env[key];
      } catch {
        // ignore
      }
      return true;
    },
    has(_target, key) {
      return typeof key === "string" && read(key) !== undefined;
    },
    ownKeys() {
      let keys = [];
      try {
        keys = Object.keys(process.env);
      } catch {
        // ignore
      }
      return [...new Set([...keys, ...Object.keys(overlay)])];
    },
    getOwnPropertyDescriptor(_target, key) {
      const value = typeof key === "string" ? read(key) : undefined;
      if (value === undefined) return undefined;
      return { value, writable: true, enumerable: true, configurable: true };
    },
  });
}

function installImportMetaEnv(env) {
  const setProto = globalThis.Deno?.[globalThis.Deno?.internal]
    ?.testSetImportMetaProto;
  if (typeof setProto === "function") {
    setProto({ env });
  }
}

function evalDefine(value) {
  if (typeof value !== "string") return value;
  try {
    return JSON.parse(value);
  } catch {
    try {
      return (0, eval)(`(${value})`);
    } catch {
      return value;
    }
  }
}

function applyEnvAndDefines(config, environmentName, workerState) {
  setEnv("VITEST", "true");
  setEnv("TEST", "true");
  setEnv("VITEST_MODE", "RUN");
  setEnv("VITEST_POOL_ID", "1");
  setEnv("VITEST_WORKER_ID", "1");
  try {
    process.env.NODE_ENV ??= "test";
  } catch {
    // ignore
  }
  const metaEnv = createImportMetaEnv();
  // `vi.stubEnv()` stubs through the worker state's `metaEnv`.
  workerState.metaEnv = metaEnv;
  metaEnv.BASE_URL = config.base ?? "/";
  metaEnv.MODE = config.mode ?? "test";
  metaEnv.DEV = true;
  metaEnv.PROD = false;
  metaEnv.SSR = environmentName === "jsdom" || environmentName === "happy-dom"
    ? false
    : true;
  for (const [key, value] of Object.entries(config.env ?? {})) {
    metaEnv[key] = value;
  }
  for (const [key, value] of Object.entries(config.define ?? {})) {
    if (key.startsWith("import.meta.env.")) {
      metaEnv[key.slice("import.meta.env.".length)] = evalDefine(value);
    } else if (/^[A-Za-z_$][\w$]*$/.test(key)) {
      globalThis[key] = evalDefine(value);
    }
  }
  installImportMetaEnv(metaEnv);
}

function buildRunnerConfig(config) {
  const updateSnapshot = config.update ? "all" : (config.ci ? "none" : "new");
  return {
    root: config.root,
    name: config.name ?? "",
    mode: "test",
    globals: !!config.globals,
    environment: config.environment ?? "node",
    environmentOptions: config.environmentOptions ?? {},
    setupFiles: config.setupFiles ?? [],
    testTimeout: config.testTimeout ?? 5000,
    hookTimeout: config.hookTimeout ?? 10000,
    teardownTimeout: config.teardownTimeout ?? 10000,
    retry: config.retry ?? 0,
    repeats: config.repeats ?? 0,
    maxConcurrency: config.maxConcurrency ?? 5,
    passWithNoTests: true,
    allowOnly: config.allowOnly ?? true,
    testNamePattern: undefined,
    includeTaskLocation: true,
    clearMocks: config.clearMocks ?? false,
    mockReset: config.mockReset ?? false,
    restoreMocks: config.restoreMocks ?? false,
    unstubGlobals: config.unstubGlobals ?? false,
    unstubEnvs: config.unstubEnvs ?? false,
    logHeapUsage: false,
    tags: config.tags ?? [],
    tagsFilter: undefined,
    strictTags: config.strictTags ?? false,
    sequence: {
      shuffle: false,
      concurrent: config.sequence?.concurrent ?? false,
      seed: config.sequence?.seed ?? Date.now(),
      hooks: config.sequence?.hooks ?? "stack",
      setupFiles: config.sequence?.setupFiles ?? "parallel",
    },
    fakeTimers: {
      loopLimit: 10_000,
      shouldClearNativeTimers: true,
      ...(config.fakeTimers ?? {}),
    },
    expect: {
      requireAssertions: config.expect?.requireAssertions ?? false,
      poll: config.expect?.poll ?? {},
    },
    snapshotOptions: {
      updateSnapshot,
      expand: config.expandSnapshotDiff ?? false,
      snapshotFormat: config.snapshotFormat ?? {},
      snapshotEnvironment: new DenoSnapshotEnvironment({
        resolveSnapshotPath: undefined,
      }),
    },
    chaiConfig: config.chaiConfig,
    diff: config.diff,
    _diffOptions: typeof config.diff === "object" ? config.diff : {},
    taskTitleValueFormatTruncate: config.taskTitleValueFormatTruncate ?? 40,
    experimental: {
      viteModuleRunner: false,
      importDurations: { limit: 0 },
    },
    benchmark: { enabled: false },
    defines: {},
    env: config.env ?? {},
  };
}

function createWorkerState(runnerConfig, filepath, environmentName) {
  const cleanups = [];
  return {
    ctx: {
      pool: "deno",
      workerId: 1,
      concurrencyId: 0,
      projectName: runnerConfig.name,
      config: runnerConfig,
    },
    config: runnerConfig,
    environment: {
      name: environmentName,
      viteEnvironment: environmentName === "jsdom" ||
          environmentName === "happy-dom"
        ? "client"
        : "ssr",
    },
    filepath,
    current: undefined,
    providedContext: {},
    evaluatedModules: { idToModuleMap: new Map() },
    resolvingModules: new Set(),
    moduleExecutionInfo: new Map(),
    durations: { environment: 0, prepare: 0, fetch: 0 },
    onCleanup: (fn) => cleanups.push(fn),
    onCancel: () => () => {},
    rpc: {
      resolveSnapshotPath: (path) => path,
    },
    cleanups,
  };
}

/**
 * Vitest's `node` environment has Node's globals. Deno also has a few
 * browser globals Node lacks, which code under test feature-detects
 * (`typeof localStorage`), and Deno's `localStorage` would even persist
 * between runs: hide them.
 */
function hideNonNodeGlobals() {
  for (
    const name of ["localStorage", "sessionStorage", "alert", "confirm", "prompt"]
  ) {
    try {
      Object.defineProperty(globalThis, name, {
        value: undefined,
        configurable: true,
        writable: true,
        enumerable: false,
      });
      delete globalThis[name];
    } catch {
      // not configurable: leave it
    }
  }
}

function captureEventTarget() {
  return {
    add: globalThis.addEventListener,
    remove: globalThis.removeEventListener,
    dispatch: globalThis.dispatchEvent,
    Event: globalThis.Event,
  };
}

/**
 * A DOM environment (jsdom, happy-dom) puts its window's EventTarget methods
 * on the global, but Deno dispatches its own events there too (`load`,
 * `unhandledrejection`, `error`, ...), which a DOM implementation rejects as
 * foreign. Route each event to the implementation that created it, and add
 * listeners to both.
 */
function installHybridEventTarget(deno) {
  const dom = captureEventTarget();
  if (dom.dispatch === deno.dispatch) return;
  const define = (name, value) =>
    Object.defineProperty(globalThis, name, {
      value,
      configurable: true,
      writable: true,
      enumerable: true,
    });
  define("addEventListener", function addEventListener(type, ...args) {
    dom.add.call(this ?? globalThis, type, ...args);
    try {
      deno.add.call(globalThis, type, ...args);
    } catch {
      // a listener or option only the DOM implementation accepts
    }
  });
  define("removeEventListener", function removeEventListener(type, ...args) {
    dom.remove.call(this ?? globalThis, type, ...args);
    try {
      deno.remove.call(globalThis, type, ...args);
    } catch {
      // see addEventListener
    }
  });
  define("dispatchEvent", function dispatchEvent(event) {
    return event instanceof deno.Event && !(event instanceof dom.Event)
      ? deno.dispatch.call(globalThis, event)
      : dom.dispatch.call(this ?? globalThis, event);
  });
}

let started = false;

/**
 * Collects and starts running the Vitest test file `specifier`. Resolves once
 * every test is registered with `Deno.test()`; running continues as Deno
 * invokes the registered tests.
 *
 * @param {string} specifier the test module's URL
 * @param {object} config the resolved Vitest config for the file
 */
export async function startFile(specifier, config = {}) {
  if (started) return;
  started = true;
  const filepath = toFilePath(specifier);
  config = { root: Deno.cwd(), ...config };
  const environmentName = config.environment ?? "node";
  const runnerConfig = buildRunnerConfig(config);
  const workerState = createWorkerState(
    runnerConfig,
    filepath,
    environmentName,
  );
  Object.defineProperty(globalThis, "__vitest_worker__", {
    value: workerState,
    configurable: true,
    writable: true,
    enumerable: false,
  });

  setSafeTimers();
  installModuleMockingStubs();
  applyEnvAndDefines(config, environmentName, workerState);
  globalExpect.setState({ environment: environmentName });

  // The environment is set up when Vitest's runner imports the first setup
  // file (or the test module), so a failure is reported as the file's
  // collection error, once.
  let environmentTeardown;
  const setupEnvironment = async () => {
    const environment = environments[environmentName];
    if (environment === undefined) {
      throw new Error(
        `Unknown Vitest environment "${environmentName}" (supported: node, jsdom, happy-dom).`,
      );
    }
    const denoEventTarget = captureEventTarget();
    try {
      environmentTeardown = await environment.setup(
        globalThis,
        config.environmentOptions ?? {},
      );
      installHybridEventTarget(denoEventTarget);
      if (environmentName === "node") hideNonNodeGlobals();
    } catch (error) {
      throw new Error(
        `Failed to set up the "${environmentName}" test environment: ${
          error?.message ?? error
        }`,
        { cause: error },
      );
    }
  };

  if (config.globals) {
    for (const api of globalApis) {
      globalThis[api] = vitestIndex[api];
    }
  }

  const bridge = new DenoBridge();
  const collected = createDeferred();
  const runner = new DenoTestRunner(runnerConfig, bridge, setupEnvironment, (
    files,
  ) => {
    for (const file of files) {
      if (file.result?.state === "fail" && file.result.errors?.length) {
        // A failed import surfaces again when Deno evaluates the module;
        // other collection errors are reported here.
        file.denoCollectionError = toThrowable(file.result.errors);
      }
    }
    registerDenoTests(files, bridge);
    const matches = createNameFilter(config.filter);
    if (matches) {
      for (const file of files) applyNameFilter(file.tasks, matches);
    }
    collected.resolve(files);
  });

  const teardown = async () => {
    bridge.releaseAll();
    await run;
    for (const fn of workerState.cleanups.splice(0)) await fn();
    vi.restoreAllMocks();
    await environmentTeardown?.teardown?.(globalThis);
    const error = takePendingSuiteError(bridge);
    if (error) throw error;
  };
  Deno.test.afterAll(teardown);

  const run = startTests([filepath], runner).then(
    () => bridge.done(),
    (error) => {
      bridge.done();
      collected.reject(error);
    },
  );

  const files = await collected.promise;
  // Vitest's runner imported the test module; `deno test` must not evaluate
  // it again as its side module (a module graph with top-level await would
  // wait for an evaluation that already happened).
  globalThis[MODULE_IMPORTED] = true;
  for (const file of files) {
    if (!file.denoCollectionError) continue;
    // Reported here, once.
    file.denoReported = true;
    throw file.denoCollectionError;
  }
}

export { createExpect, vi };
