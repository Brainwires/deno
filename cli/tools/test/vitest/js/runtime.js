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
import {
  denoGetTestFixtures as getTestFixtures,
  h as getHooks,
  i as getFn,
  s as startTests,
} from "./vendor/runner.js";
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
import { fileUrl, installModuleMocker } from "./mocker.js";

const ORIGINAL_ERROR = Symbol.for("deno.vitest.originalError");
const TEST_SKIP = Symbol.for("Deno.test.skip");
const TEST_LOCATION = Symbol.for("Deno.test.location");
const MODULE_IMPORTED = Symbol.for("deno.vitest.moduleImported");
const MOCKER = Symbol.for("deno.vitest.mocker");
/** The fixtures a test function asks for (set by the vendored runner). */
const USED_FIXTURES = Symbol.for("deno.vitest.usedFixtures");

const { fileURLToPath } = await import("node:url");

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
  /** Why Vitest's runner stopped after collecting the tests, if it threw. */
  runError = undefined;
  runErrorReported = false;

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

  /** Runs a test's function (after its `beforeEach` hooks). */
  async runTask(test) {
    // A mock factory's getters (`get value() { return state.value }`) are
    // read again, for the values the hooks set up.
    globalThis[MOCKER]?.syncGetters();
    globalThis[MOCKER]?.prewarm();
    const fn = getFn(test);
    if (!fn) {
      throw new Error(
        "Test function is not found. Did you add it using `setFn`?",
      );
    }
    await fn();
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
  // the URL `deno test` loaded the module by, not another one
  return fileUrl(path);
}

/**
 * A path as Vitest spells it: forward slashes on Windows too (Vitest compares
 * a task's file with the stack frames it parses, which it normalizes so).
 */
function slash(path) {
  return Deno.build.os === "windows" ? path.replaceAll("\\", "/") : path;
}

function toFilePath(specifier) {
  return slash(
    specifier.startsWith("file:") ? fileURLToPath(specifier) : specifier,
  );
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

/** Whether `deno test` invokes the Deno test registered for `test`. */
function isInvoked(test) {
  return (test.mode === "run" || test.mode === "queued") &&
    test.denoFilteredOut !== true;
}

/** Whether `suite` has hooks that run once around all of its tests. */
function hasSuiteHooks(suite) {
  const hooks = getHooks(suite);
  return !!(hooks?.beforeAll?.length || hooks?.afterAll?.length ||
    hooks?.aroundAll?.length);
}

function isScoped(fixture) {
  return fixture.scope === "file" || fixture.scope === "worker";
}

/** Whether `suite` or a suite around it has per-test hooks. */
function hasEachHooks(suite) {
  for (let current = suite; current;) {
    const hooks = getHooks(current);
    if (
      hooks?.beforeEach?.length || hooks?.afterEach?.length ||
      hooks?.aroundEach?.length
    ) {
      return true;
    }
    if (current === current.file) break;
    current = current.suite ?? current.file;
  }
  return false;
}

/**
 * Whether `test` can set up a fixture that outlives it (file or worker
 * scope): its function asks for one, directly or through another fixture, or
 * one is `auto`. A per-test hook can ask for one too, so under such hooks a
 * test counts whenever its `test` has one.
 */
function hasScopedFixture(test) {
  const suite = test.suite ?? test.file;
  const registrations = getTestFixtures(test.context)?.get(suite);
  if (!registrations?.size) return false;
  const all = [...registrations.values()];
  if (!all.some(isScoped)) return false;
  const used = test[USED_FIXTURES];
  if (used === undefined || hasEachHooks(suite)) return true;
  const seen = new Set();
  const pending = all.filter((fixture) =>
    fixture.auto || used.has(fixture.name)
  );
  while (pending.length) {
    const fixture = pending.pop();
    if (seen.has(fixture)) continue;
    seen.add(fixture);
    if (isScoped(fixture)) return true;
    for (const name of fixture.deps ?? []) {
      const dep = name === fixture.name
        ? fixture.parent
        : registrations.get(name);
      if (dep) pending.push(dep);
    }
  }
  return false;
}

/**
 * The tests whose Deno test can't be checked for leaks.
 *
 * Deno's sanitizers check what a test starts and finishes while its Deno test
 * function runs. For a Vitest test that span is the test with its
 * `beforeEach` / `afterEach` hooks, and also whatever Vitest's runner does
 * before it stops at the next test: the `afterAll` hooks of the suites the
 * test ends and the `beforeAll` hooks of the suites the next test starts
 * (`aroundAll` too). What those hooks open is meant to outlive the test
 * (a server a whole `describe` uses), so a test whose span runs one, or that
 * can set up a file- or worker-scoped fixture (or tear them down, as the last
 * test does), is registered with both sanitizers off. Its leaks are not
 * blamed on later tests (see `record_ignored_leaks` in
 * cli/tools/test/sanitizers.rs). The first test is exempt too when hooks run
 * before it: Vitest's runner starts them while Deno loads the module.
 */
function sanitizerExemptTests(files) {
  const exempt = new Set();
  let current;
  let hooksBeforeFirst = false;
  let scopedFixtures = false;
  const suiteBoundary = (suite) => {
    if (!hasSuiteHooks(suite)) return;
    if (current) exempt.add(current);
    else hooksBeforeFirst = true;
  };
  const visit = (suite) => {
    if (suite.mode === "skip" || suite.mode === "todo") return;
    suiteBoundary(suite);
    for (const task of suite.tasks ?? []) {
      if (task.type === "suite") {
        visit(task);
      } else if (task.type === "test" && isInvoked(task)) {
        if (!current && hooksBeforeFirst) exempt.add(task);
        if (hasScopedFixture(task)) {
          scopedFixtures = true;
          exempt.add(task);
        }
        current = task;
      }
    }
    suiteBoundary(suite);
  };
  for (const file of files) visit(file);
  if (scopedFixtures && current) exempt.add(current);
  return exempt;
}

function registerDenoTests(files, bridge, prefix = "") {
  const exempt = sanitizerExemptTests(files);
  for (const file of files) {
    registerTasks(file.tasks, bridge, prefix, exempt);
  }
}

function registerTasks(tasks, bridge, prefix, exempt) {
  for (const task of tasks) {
    if (task.type === "suite") {
      registerTasks(task.tasks, bridge, prefix, exempt);
      continue;
    }
    if (task.type !== "test") continue;
    registerTest(task, bridge, prefix, exempt.has(task));
  }
}

function registerTest(test, bridge, prefix, sanitizerExempt) {
  const name = prefix + testFullName(test);
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
    // The runner stopped before running the test (or recording why it
    // failed): never report it as passed.
    if (bridge.runError !== undefined && (!result || result.state === "run")) {
      bridge.runErrorReported = true;
      throw bridge.runError;
    }
    if (result?.state === "fail") {
      throw new Error("The test failed without an error");
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
      fileUrl(test.file.filepath)
    }:${test.location.line}:${test.location.column}`;
  }
  // The sanitizers follow `deno test`'s own settings (a `Deno.test.sanitizer()`
  // call, `--sanitize-ops` / `--sanitize-resources`, the `DENO_TEST_SANITIZE_*`
  // variables or deno.json's `test` options), except for the tests above.
  Deno.test({
    name,
    fn,
    ignore: !runnable,
    ...(sanitizerExempt
      ? { sanitizeOps: false, sanitizeResources: false }
      : {}),
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
    const name of [
      "localStorage",
      "sessionStorage",
      "alert",
      "confirm",
      "prompt",
      // canvas/image APIs code detects to decide whether it can draw
      "createImageBitmap",
      "ImageBitmap",
      "ImageData",
      "OffscreenCanvas",
    ]
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

/** A Jest environment name (`jest-environment-jsdom`, ...) as Vitest's. */
function normalizeEnvironment(name) {
  if (name === undefined || name === null) return "node";
  if (name in environments) return name;
  if (/happy-?dom/i.test(name)) return "happy-dom";
  if (/jsdom/i.test(name)) return "jsdom";
  if (/(^|[-/])node$/.test(name)) return "node";
  return name;
}

let started = false;

/**
 * Collects and starts running the Vitest test file `specifier`. Resolves once
 * every test is registered with `Deno.test()`; running continues as Deno
 * invokes the registered tests.
 *
 * @param {string} specifier the test module's URL
 * @param {object} config the resolved Vitest config for the file
 * @param {object} [runnerFs] the runner's own reads (see preload.js); without
 *   them, its reads and imports are checked like the test's
 */
export async function startFile(specifier, config = {}, runnerFs) {
  if (started) return;
  started = true;
  // the setup files (and a Jest project's snapshot serializers) are the
  // runner's own imports
  for (
    const path of [
      ...(config.setupFiles ?? []),
      ...(config.snapshotSerializers ?? []),
    ]
  ) {
    runnerFs?.allowImport(toImportSpecifier(path));
  }
  const filepath = toFilePath(specifier);
  config = { root: slash(Deno.cwd()), ...config };
  const environmentName = normalizeEnvironment(config.environment);
  const jestProject = config.flavor === "jest"
    ? await import("./jest.js")
    : undefined;
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
  if (config.packageBases) {
    globalThis[Symbol.for("deno.vitest.packageBases")] = config.packageBases;
  }
  installModuleMocker(
    vi,
    config.root,
    config.mockRoots,
    config.flavor === "jest",
    runnerFs,
  );
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
      await jestProject?.setupJestEnvironment(
        config,
        environmentName,
        runnerFs,
      );
    } catch (error) {
      throw new Error(
        `Failed to set up the "${environmentName}" test environment: ${
          error?.message ?? error
        }`,
        { cause: error },
      );
    }
  };

  if (jestProject) {
    jestProject.installJestProject(config);
    await jestProject.registerRootMocks(config.mockRoots, runnerFs);
  } else if (config.globals) {
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
    // Filtered first: which tests run decides which are exempt from the
    // sanitizers (see `sanitizerExemptTests`).
    const matches = createNameFilter(config.filter);
    if (matches) {
      for (const file of files) applyNameFilter(file.tasks, matches);
    }
    registerDenoTests(files, bridge, config.testNamePrefix);
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
    // e.g. saving the snapshot file after the last test
    if (bridge.runError !== undefined && !bridge.runErrorReported) {
      bridge.runErrorReported = true;
      throw bridge.runError;
    }
  };
  Deno.test.afterAll(teardown);

  const run = startTests([filepath], runner).then(
    () => bridge.done(),
    (error) => {
      // after collection, the tests that did not run fail with it
      bridge.runError = error;
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
