// Copyright 2018-2026 the Deno authors. MIT license.

(function () {
"use strict";
const { core, internals, primordials } = __bootstrap;
const {
  ArrayIsArray,
  ArrayPrototypeIncludes,
  ArrayPrototypeJoin,
  ArrayPrototypeLastIndexOf,
  ArrayPrototypeMap,
  ArrayPrototypePop,
  ArrayPrototypePush,
  ArrayPrototypeSlice,
  ArrayPrototypeSort,
  ArrayPrototypeSplice,
  Date,
  DateNow,
  DatePrototypeGetTime,
  DatePrototypeToISOString,
  DatePrototypeToString,
  Error,
  ErrorPrototype,
  FunctionPrototypeCall,
  JSONStringify,
  MapPrototypeClear,
  MapPrototypeDelete,
  MapPrototypeGet,
  MapPrototypeHas,
  MapPrototypeSet,
  MathFloor,
  MathImul,
  MathMax,
  MathRandom,
  NumberIsFinite,
  NumberIsInteger,
  NumberPrototypeToFixed,
  ObjectDefineProperty,
  ObjectEntries,
  ObjectKeys,
  ObjectPrototypeHasOwnProperty,
  ObjectGetOwnPropertyDescriptor,
  ObjectGetPrototypeOf,
  ObjectPrototypeIsPrototypeOf,
  Promise,
  PromisePrototypeThen,
  PromiseResolve,
  PromiseWithResolvers,
  Proxy,
  ReflectApply,
  ReflectConstruct,
  ReflectGet,
  ReflectHas,
  RegExpPrototypeExec,
  RegExpPrototypeTest,
  SafeArrayIterator,
  SafeMap,
  SafeMapIterator,
  SafePromiseRace,
  SafeRegExp,
  SafeSet,
  SafeSetIterator,
  SetPrototypeAdd,
  SetPrototypeDelete,
  SetPrototypeGetSize,
  SetPrototypeHas,
  String,
  StringPrototypeEndsWith,
  StringPrototypeIncludes,
  StringPrototypeIndexOf,
  StringPrototypeMatch,
  StringPrototypeRepeat,
  StringPrototypeReplace,
  StringPrototypeReplaceAll,
  StringPrototypeSlice,
  StringPrototypeSplit,
  StringPrototypeToLowerCase,
  StringPrototypeTrim,
  StringPrototypeStartsWith,
  Symbol,
  SymbolDispose,
  SymbolFor,
  SymbolToPrimitive,
  TypeError,
  queueMicrotask,
} = primordials;

// The genuine timer functions, captured once at runtime so that an enabled mock
// clock (node:test's mock.timers) cannot stop a test's timeout from firing in
// real time. We cannot read these at module-init time: this polyfill is baked
// into the startup snapshot, where the module body runs before the timer APIs
// exist on `globalThis`. Instead we capture lazily on the first `test()` /
// `suite()` registration (see installErrorHandlers), which always runs after
// the runtime is booted and before any test body can call mock.timers.enable().
let realSetTimeout = null;
let realClearTimeout = null;
function ensureRealTimers() {
  if (realSetTimeout === null) {
    realSetTimeout = globalThis.setTimeout;
    realClearTimeout = globalThis.clearTimeout;
  }
}

let errorHandlersInstalled = false;

let activeNodeTests = 0;

let pendingCallbackReject = null;

// Stack of failure "sinks" for the tests whose bodies are currently executing.
// A test pushes a sink for the duration of its body (across await boundaries)
// and pops it when the body settles. An unhandled rejection or uncaught
// exception that fires while a test body is running is attributed to the
// innermost still-running test, matching Node's behavior of failing the
// currently-active test (see https://github.com/denoland/deno/issues/34818).
const activeTestSinks = [];

function pushTestSink(sink) {
  ArrayPrototypePush(activeTestSinks, sink);
}

function popTestSink(sink) {
  const idx = ArrayPrototypeLastIndexOf(activeTestSinks, sink);
  if (idx !== -1) {
    ArrayPrototypeSplice(activeTestSinks, idx, 1);
  }
}

// The innermost test whose body is still running, or null when no test body is
// currently on the stack (e.g. between registration and execution).
function currentTestSink() {
  for (let i = activeTestSinks.length - 1; i >= 0; i--) {
    const sink = activeTestSinks[i];
    if (!sink.settled) return sink;
  }
  return null;
}

function buildTimeoutError(timeout) {
  // Node fails a timed-out test with a `test timed out after Nms` cause and an
  // ERR_TEST_FAILURE wrapper. We surface the same message and tag the error so
  // reporters that look at code/failureType behave like Node.
  const err = new Error(`test timed out after ${timeout}ms`);
  err.code = "ERR_TEST_FAILURE";
  err.failureType = "testTimeoutFailure";
  return err;
}

function buildAbortError(signal) {
  // When the test is aborted via a caller-supplied signal, Node fails the test
  // with the signal's reason (or a generic abort error when none was given).
  if (signal && signal.reason !== undefined) {
    return signal.reason;
  }
  const err = new Error("The test was aborted");
  err.code = "ABORT_ERR";
  err.failureType = "testAborted";
  return err;
}

function sanitizeThrowValue(err) {
  if (err === null || err === undefined || typeof err !== "object") {
    return err;
  }
  if (ObjectPrototypeIsPrototypeOf(ErrorPrototype, err)) {
    return err;
  }
  const inspectSymbol = SymbolFor("nodejs.util.inspect.custom");
  if (typeof err[inspectSymbol] !== "function") {
    return err;
  }
  try {
    Deno.inspect(err);
    return err;
  } catch {
    return new Error(
      "test threw a non-Error object with a throwing custom inspect",
    );
  }
}

function installErrorHandlers() {
  // Capture the genuine timers now, before any test body runs and before any
  // mock clock can replace the globals.
  ensureRealTimers();
  if (errorHandlersInstalled) return;
  errorHandlersInstalled = true;

  globalThis.addEventListener("unhandledrejection", (event) => {
    // Attribute the rejection to the currently-running test so it fails, the
    // way Node does. This is gated on an active sink rather than on
    // `activeNodeTests` so it also works in TAP mode, where tests are run by
    // our own runner and the counter is not used.
    const sink = currentTestSink();
    if (sink !== null) {
      event.preventDefault();
      sink.fail(event.reason);
      return;
    }
    // Preserve the prior behavior of swallowing rejections while Deno.test
    // backed node tests are pending but no body is actively running.
    if (activeNodeTests > 0) {
      event.preventDefault();
    }
  });

  globalThis.addEventListener("error", (event) => {
    // Uncaught exceptions are not routed through the test sink: a synchronous
    // throw in a test body is already caught by the body wrapper, and Node does
    // not treat every uncaught `error` event (for example warnings surfaced as
    // errors) as a test failure. We keep the original behavior of swallowing
    // the error while node tests are pending and forwarding to a pending
    // callback-style `done` reject.
    if (activeNodeTests > 0) {
      event.preventDefault();
    }
    if (pendingCallbackReject !== null) {
      pendingCallbackReject(event.error ?? new Error("uncaught error"));
      pendingCallbackReject = null;
    }
  });
}
const {
  validateBoolean,
  validateFunction,
  validateInteger,
  validateNumber,
  validateObject,
  validateString,
  validateStringArray,
} = core.loadExtScript("ext:deno_node/internal/validators.mjs");
const nodeErrors = core.loadExtScript("ext:deno_node/internal/errors.ts");
const {
  ERR_INVALID_ARG_TYPE,
  ERR_INVALID_ARG_VALUE,
} = nodeErrors.codes;
// `ERR_INVALID_STATE` is a hand-written class exported at the top level of the
// errors module; unlike the generated codes it is not registered on `.codes`.
const { ERR_INVALID_STATE } = nodeErrors;
const { emitExperimentalWarning, kEmptyObject } = core.loadExtScript(
  "ext:deno_node/internal/util.mjs",
);
const { default: assert } = core.loadExtScript("ext:deno_node/assert.ts");
const {
  tapEscape,
  tapIndent,
} = core.loadExtScript("ext:deno_node/internal/test/reporters.ts");

// `t.assert`, as Node's `lib/internal/test_runner/assert.js`: the methods
// copied from `node:assert`, plus `fileSnapshot`, plus whatever
// `require("node:test").assert.register(name, fn)` adds or overrides. `ok`
// isn't in the map; a context supplies its own so the stack starts at the
// caller (as Node's `innerOk` hack).
const methodsToCopy = [
  "deepEqual",
  "deepStrictEqual",
  "doesNotMatch",
  "doesNotReject",
  "doesNotThrow",
  "equal",
  "fail",
  "ifError",
  "match",
  "notDeepEqual",
  "notDeepStrictEqual",
  "notEqual",
  "notStrictEqual",
  "partialDeepStrictEqual",
  "rejects",
  "strictEqual",
  "throws",
];

let assertionMap;
function getAssertionMap() {
  if (assertionMap === undefined) {
    assertionMap = new SafeMap();
    for (let i = 0; i < methodsToCopy.length; i++) {
      MapPrototypeSet(assertionMap, methodsToCopy[i], assert[methodsToCopy[i]]);
    }
    MapPrototypeSet(assertionMap, "snapshot", snapshotAssertion);
    MapPrototypeSet(assertionMap, "fileSnapshot", fileSnapshot);
  }
  return assertionMap;
}

function registerAssertion(name, fn) {
  validateString(name, "name");
  validateFunction(fn, "fn");
  MapPrototypeSet(getAssertionMap(), name, fn);
}

let innerOk;
// A context's `t.assert`: every registered assertion, called with the
// context as `this` and counted against its plan (`getPlan()`).
function createContextAssert(context, getPlan) {
  const assertObject = { __proto__: null };
  for (
    const { 0: name, 1: method } of new SafeMapIterator(getAssertionMap())
  ) {
    assertObject[name] = (...args) => {
      getPlan()?.increment();
      return ReflectApply(method, context, args);
    };
  }
  if (!MapPrototypeHas(getAssertionMap(), "ok")) {
    innerOk ??= core.loadExtScript("ext:deno_node/internal/assert/utils.ts")
      .innerOk;
    const ok = function ok(...args) {
      getPlan()?.increment();
      innerOk(ok, ...new SafeArrayIterator(args));
    };
    assertObject.ok = ok;
  }
  return assertObject;
}

// Lazy access so `node:fs` and `node:path` polyfills are pulled in only on the
// first `fileSnapshot()` call, mirroring the other lazy loaders in this file
// and avoiding circular init during snapshot build.
let _fsForSnapshot = null;
function getFsForSnapshot() {
  if (_fsForSnapshot === null) {
    _fsForSnapshot = core.loadExtScript("ext:deno_node/fs.ts");
  }
  return _fsForSnapshot;
}
let _pathForSnapshot = null;
function getPathForSnapshot() {
  if (_pathForSnapshot === null) {
    _pathForSnapshot = core.loadExtScript("ext:deno_node/path/mod.ts");
  }
  return _pathForSnapshot;
}

// Resolve update-snapshot mode lazily so the env / Rust op is read at most
// once. We accept either Deno's own `--update-snapshots` flag (when running
// under `deno test`) or Node's `--test-update-snapshots` propagated via
// NODE_OPTIONS, so the polyfill behaves the same way the rest of the Node
// compat surface does for reporter detection above.
//
// The deno_test extension's ops are registered at runtime - after this
// polyfill's snapshot is built - so they are not on the captured `core.ops`.
// They are however reachable via `Deno[Deno.internal].core.ops`, which the
// test runner exposes for cli/js code; we look them up lazily through there.
let _fileSnapshotUpdateMode = undefined;
function isFileSnapshotUpdateMode() {
  if (_fileSnapshotUpdateMode !== undefined) return _fileSnapshotUpdateMode;
  const denoInternal = globalThis.Deno?.[globalThis.Deno.internal];
  const op = denoInternal?.core?.ops?.op_test_snapshot_in_update_mode;
  if (typeof op === "function") {
    try {
      if (op()) {
        _fileSnapshotUpdateMode = true;
        return true;
      }
    } catch { /* op not wired up; not running under `deno test` */ }
  }
  let nodeOptions = "";
  try {
    nodeOptions = globalThis.Deno?.env?.get("NODE_OPTIONS") || "";
  } catch { /* permission denied */ }
  if (
    nodeOptions &&
    RegExpPrototypeTest(
      new SafeRegExp(/(?:^|\s)--test-update-snapshots(?:\s|=|$)/),
      nodeOptions,
    )
  ) {
    _fileSnapshotUpdateMode = true;
    return true;
  }
  _fileSnapshotUpdateMode = false;
  return false;
}

// Default serializer pipeline: matches Node's `t.assert.fileSnapshot` default
// (`JSON.stringify(value, null, 2)`).
function defaultFileSnapshotSerializer(value) {
  return JSONStringify(value, null, 2);
}

// `t.assert.fileSnapshot(value, path[, options])`.
//
// Without `--test-update-snapshots`, serializes `value` and compares against
// the contents of the file at `path` using `assert.strictEqual`. With the
// flag, writes the serialized value to `path` (creating parent directories
// as needed). Both file paths are CWD-relative, matching Node.
function fileSnapshot(actual, path, options) {
  validateString(path, "path");
  if (options === undefined) {
    options = { __proto__: null };
  } else {
    validateObject(options, "options");
  }
  let serializers;
  if (options.serializers === undefined) {
    serializers = [defaultFileSnapshotSerializer];
  } else {
    if (!ArrayIsArray(options.serializers)) {
      throw new ERR_INVALID_ARG_TYPE(
        "options.serializers",
        "Array",
        options.serializers,
      );
    }
    serializers = options.serializers;
    for (let i = 0; i < serializers.length; i++) {
      if (typeof serializers[i] !== "function") {
        throw new ERR_INVALID_ARG_TYPE(
          `options.serializers[${i}]`,
          "function",
          serializers[i],
        );
      }
    }
  }
  let value = actual;
  try {
    for (let i = 0; i < serializers.length; i++) {
      value = serializers[i](value);
    }
  } catch (err) {
    const e = new ERR_INVALID_STATE(
      "The provided serializers did not generate a string.",
    );
    e.cause = err;
    e.input = actual;
    throw e;
  }
  if (typeof value !== "string") {
    const e = new ERR_INVALID_STATE(
      "The provided serializers did not generate a string.",
    );
    e.input = actual;
    throw e;
  }

  const fs = getFsForSnapshot();
  if (isFileSnapshotUpdateMode()) {
    try {
      const parent = getPathForSnapshot().dirname(path);
      fs.mkdirSync(parent, { __proto__: null, recursive: true });
      fs.writeFileSync(path, value, "utf8");
    } catch (err) {
      const e = new ERR_INVALID_STATE(
        `Cannot write snapshot file '${path}'.`,
      );
      e.cause = err;
      e.filename = path;
      throw e;
    }
    return;
  }

  let expected;
  try {
    expected = fs.readFileSync(path, "utf8");
  } catch (err) {
    const isMissing = err && err.code === "ENOENT";
    const message = isMissing
      ? `Cannot read snapshot file '${path}'. Missing snapshots can be ` +
        "generated by rerunning the command with the --test-update-snapshots " +
        "flag."
      : `Cannot read snapshot file '${path}'.`;
    const e = new ERR_INVALID_STATE(message);
    e.cause = err;
    e.filename = path;
    throw e;
  }
  assert.strictEqual(value, expected);
}

// `t.assert.snapshot(value[, options])`, as Node 26.5.1's
// `lib/internal/test_runner/snapshot.js`: each test's snapshots live in
// `<test file>.snapshot` (or `snapshot.setResolveSnapshotPath()`'s file), keyed
// `${t.fullName} ${n}`, as template literals assigned to `exports`. With
// `--test-update-snapshots` the file is written instead of compared.
const kMissingSnapshotTip = "Missing snapshots can be generated by rerunning " +
  "the command with the --test-update-snapshots flag.";
let snapshotSerializers = [defaultFileSnapshotSerializer];
let resolveSnapshotPathFn = defaultResolveSnapshotPath;
const snapshotFiles = new SafeMap();

function defaultResolveSnapshotPath(testPath) {
  if (typeof testPath !== "string") {
    return testPath;
  }
  return `${testPath}.snapshot`;
}

function validateFunctionArray(fns, name) {
  if (!ArrayIsArray(fns)) {
    throw new ERR_INVALID_ARG_TYPE(name, "Array", fns);
  }
  for (let i = 0; i < fns.length; i++) {
    validateFunction(fns[i], `${name}[${i}]`);
  }
}

function setDefaultSnapshotSerializers(serializers) {
  validateFunctionArray(serializers, "serializers");
  snapshotSerializers = ArrayPrototypeSlice(serializers);
}

function setResolveSnapshotPath(fn) {
  validateFunction(fn, "fn");
  resolveSnapshotPathFn = fn;
}

function snapshotTemplateEscape(str) {
  let result = String(str);
  result = StringPrototypeReplaceAll(result, "\\", "\\\\");
  result = StringPrototypeReplaceAll(result, "`", "\\`");
  result = StringPrototypeReplaceAll(result, "${", "\\${");
  return result;
}

function escapeSnapshotKey(str) {
  let result = StringPrototypeSlice(JSONStringify(str, null, 2), 1, -1);
  result = StringPrototypeReplaceAll(result, "`", "\\`");
  result = StringPrototypeReplaceAll(result, "${", "\\${");
  return result;
}

function snapshotStateError(message, filename, cause) {
  const err = new ERR_INVALID_STATE(message);
  if (cause !== undefined) err.cause = cause;
  err.filename = filename;
  return err;
}

function getSnapshotFile(entryFile) {
  let file = MapPrototypeGet(snapshotFiles, entryFile);
  if (file === undefined) {
    const resolved = resolveSnapshotPathFn(entryFile);
    if (typeof resolved !== "string") {
      throw snapshotStateError("Invalid snapshot filename.", resolved);
    }
    file = {
      path: resolved,
      snapshots: { __proto__: null },
      nameCounts: new SafeMap(),
      loaded: false,
    };
    MapPrototypeSet(snapshotFiles, entryFile, file);
  }
  return file;
}

function readSnapshotFile(file) {
  if (file.loaded) return;
  try {
    const source = getFsForSnapshot().readFileSync(file.path, "utf8");
    const vm = core.loadExtScript("ext:deno_node/vm.js");
    const context = { __proto__: null, exports: { __proto__: null } };
    vm.createContext(context);
    vm.runInContext(source, context);
    if (context.exports === null || typeof context.exports !== "object") {
      throw new ERR_INVALID_STATE(`Malformed snapshot file '${file.path}'.`);
    }
    const keys = ObjectKeys(context.exports);
    for (let i = 0; i < keys.length; i++) {
      file.snapshots[keys[i]] = snapshotTemplateEscape(
        context.exports[keys[i]],
      );
    }
    file.loaded = true;
  } catch (err) {
    let msg = `Cannot read snapshot file '${file.path}.'`;
    if (err?.code === "ENOENT") {
      msg += ` ${kMissingSnapshotTip}`;
    }
    throw snapshotStateError(msg, file.path, err);
  }
}

function writeSnapshotFile(file) {
  try {
    const keys = ArrayPrototypeSort(ObjectKeys(file.snapshots));
    const out = ArrayPrototypeJoin(
      ArrayPrototypeMap(
        keys,
        (key) => `exports[\`${key}\`] = \`${file.snapshots[key]}\`;\n`,
      ),
      "\n",
    );
    const fs = getFsForSnapshot();
    fs.mkdirSync(getPathForSnapshot().dirname(file.path), {
      __proto__: null,
      recursive: true,
    });
    fs.writeFileSync(file.path, out, "utf8");
  } catch (err) {
    throw snapshotStateError(
      `Cannot write snapshot file '${file.path}.'`,
      file.path,
      err,
    );
  }
}

function snapshotAssertion(actual, options = kEmptyObject) {
  validateObject(options, "options");
  const { serializers = snapshotSerializers } = options;
  validateFunctionArray(serializers, "options.serializers");
  const filePath = this?.filePath ?? getEntryFilePath();
  const fullName = this?.fullName;
  const file = getSnapshotFile(filePath);
  let value = actual;
  try {
    for (let i = 0; i < serializers.length; i++) {
      value = serializers[i](value);
    }
  } catch (err) {
    const e = new ERR_INVALID_STATE(
      "The provided serializers did not generate a string.",
    );
    e.input = actual;
    e.cause = err;
    throw e;
  }
  value = `\n${snapshotTemplateEscape(value)}\n`;
  const count = MapPrototypeGet(file.nameCounts, fullName) ?? 1;
  MapPrototypeSet(file.nameCounts, fullName, count + 1);
  const id = `${fullName} ${count}`;
  if (isFileSnapshotUpdateMode()) {
    // Node writes every file once the run ends; written on each update here,
    // which leaves the same file.
    file.loaded = true;
    file.snapshots[escapeSnapshotKey(id)] = value;
    writeSnapshotFile(file);
    return;
  }
  readSnapshotFile(file);
  if (!ReflectHas(file.snapshots, id)) {
    const err = new ERR_INVALID_STATE(
      `Snapshot '${id}' not found in '${file.path}.' ${kMissingSnapshotTip}`,
    );
    err.snapshot = id;
    err.filename = file.path;
    throw err;
  }
  assert.strictEqual(value, file.snapshots[id]);
}

// Lazy access to other node polyfills; loading these eagerly at module
// init causes circular initialization issues during snapshotting.
let _Readable = null;
function getReadable() {
  if (_Readable === null) {
    _Readable = core.loadExtScript(
      "ext:deno_node/internal/streams/readable.js",
    ).Readable;
  }
  return _Readable;
}
let _fsWatch = null;
function getFsWatch() {
  if (_fsWatch === null) {
    _fsWatch = core.loadExtScript("ext:deno_node/fs.ts").watch;
  }
  return _fsWatch;
}
const lazyProcess = core.createLazyLoader("node:process");

// node:test `run()` implementation.
//
// Returns a `TestsStream`-compatible Readable that emits structured events
// describing the test run lifecycle. We currently support the watch-mode
// event stream (`test:watch:drained`, `test:watch:restarted`) which is the
// minimum required for the Node.js `test-runner/test-run-watch-*` fixtures
// that drive watch behavior through the programmatic API. Actual test file
// discovery / execution remains TODO and is gated behind separate work; the
// stream emits a single empty run cycle, then either ends (watch:false) or
// waits for filesystem changes to trigger restarts (watch:true).
//
// See test-runner/test-run-watch-*.mjs in the Node compat suite for the
// behavior this implements.
function run(options) {
  options = options ?? {};

  // Experimental `testTagFilters`: a bare string is normalized to a
  // single-element array; every element must be a string. Registering a
  // non-empty filter emits the one-shot experimental warning.
  let testTagFilters = options.testTagFilters;
  if (testTagFilters !== undefined) {
    if (typeof testTagFilters === "string") {
      testTagFilters = [testTagFilters];
    }
    if (!ArrayIsArray(testTagFilters)) {
      throw new ERR_INVALID_ARG_TYPE(
        "options.testTagFilters",
        ["string", "string[]"],
        options.testTagFilters,
      );
    }
    for (let i = 0; i < testTagFilters.length; i++) {
      if (typeof testTagFilters[i] !== "string") {
        throw new ERR_INVALID_ARG_TYPE(
          `options.testTagFilters[${i}]`,
          "string",
          testTagFilters[i],
        );
      }
    }
    if (testTagFilters.length > 0 && !tagsWarningEmitted) {
      tagsWarningEmitted = true;
      emitExperimentalWarning("Test tags");
    }
  }

  const watch = options.watch === true;
  const signal = options.signal;
  let cwd = options.cwd;
  if (cwd === undefined) {
    cwd = lazyProcess().default.cwd();
  }

  const Readable = getReadable();
  const stream = new Readable({
    __proto__: null,
    objectMode: true,
    // We push events imperatively; the consumer just needs a no-op `_read`.
    read() {},
  });

  let watcher = null;
  let finished = false;
  let pendingRestartTimer = null;

  function finish() {
    if (finished) return;
    finished = true;
    if (pendingRestartTimer !== null) {
      clearTimeout(pendingRestartTimer);
      pendingRestartTimer = null;
    }
    if (watcher !== null) {
      try {
        watcher.close();
      } catch { /* ignore */ }
      watcher = null;
    }
    // deno-lint-ignore deno-internal/prefer-primordials -- stream is a Node Readable, not an Array
    stream.push(null);
  }

  function emit(type) {
    if (finished) return;
    const data = { __proto__: null };
    // Node's TestsStream emits each lifecycle entry both as a data chunk
    // (consumed via async iteration / `'data'` listeners) and as a named
    // event so callers can attach `.on('test:watch:drained', ...)` directly.
    // deno-lint-ignore deno-internal/prefer-primordials -- stream is a Node Readable, not an Array
    stream.push({ __proto__: null, type, data });
    stream.emit(type, data);
  }

  function drained() {
    emit("test:watch:drained");
  }

  function scheduleRestart() {
    if (finished) return;
    // Debounce bursts of fs events so a single user-visible change produces
    // exactly one restart cycle (Node's watcher coalesces likewise).
    if (pendingRestartTimer !== null) {
      clearTimeout(pendingRestartTimer);
    }
    pendingRestartTimer = setTimeout(() => {
      pendingRestartTimer = null;
      if (finished) return;
      emit("test:watch:restarted");
      drained();
    }, 50);
  }

  if (signal) {
    if (signal.aborted) {
      // Resolve the initial drained on next tick to keep callers that
      // `await once(stream, 'test:watch:drained')` working.
      queueMicrotask(() => {
        drained();
        finish();
      });
      return stream;
    }
    signal.addEventListener("abort", finish, { once: true });
  }

  // Emit the initial "drained" event after the current microtask completes
  // so that consumers attaching `.on('data')` synchronously after `run(...)`
  // returns still observe the event.
  queueMicrotask(() => {
    drained();
    if (!watch) {
      finish();
      return;
    }
    try {
      const fsWatch = getFsWatch();
      watcher = fsWatch(cwd, { recursive: true }, () => {
        scheduleRestart();
      });
      watcher.on("error", () => {
        finish();
      });
    } catch {
      // If we can't watch (e.g. cwd doesn't exist), end the stream gracefully.
      finish();
    }
  });

  return stream;
}

function noop() {}

const skippedSymbol = Symbol("skipped");

// Detect Node.js-compatible `--test-reporter=...` selection so the polyfill
// can emit reporter output matching Node's snapshot fixtures. Deno's CLI does
// not consume `--test-reporter`; instead, our `child_process` polyfill (via
// node_shim) propagates the value through NODE_OPTIONS when one Deno process
// spawns another using Node-style flags. Reading the env var here keeps the
// detection self-contained and avoids new Rust plumbing.
function detectNodeTestReporter() {
  const env = globalThis.Deno?.env;
  if (!env) return null;
  let value = null;
  try {
    value = env.get("NODE_TEST_REPORTER");
  } catch { /* permission denied */ }
  if (value) return value;
  let nodeOptions = "";
  try {
    nodeOptions = env.get("NODE_OPTIONS") || "";
  } catch { /* permission denied */ }
  if (!nodeOptions) return null;
  // Match the first `--test-reporter` occurrence; support both `=value` and
  // space-separated forms. We intentionally do not handle multiple reporters
  // (Node lets you stack reporters with destinations); the snapshot tests use
  // a single reporter and that is what we target.
  const match = StringPrototypeMatch(
    nodeOptions,
    new SafeRegExp(/--test-reporter(?:=|\s+)(\S+)/),
  );
  return match ? match[1] : null;
}

// Resolve lazily: testing.ts ships inside Deno's startup snapshot, so any env
// lookups performed at module-evaluation time observe the build environment,
// not the running process. Memoize on the first call so we still only read the
// env once.
let nodeTestReporterCache;
function getNodeTestReporter() {
  if (nodeTestReporterCache !== undefined) return nodeTestReporterCache;
  nodeTestReporterCache = detectNodeTestReporter();
  return nodeTestReporterCache;
}
// Whether this process runs under `deno test` (whose `Deno.test()` the
// default path registers with); elsewhere `Deno.test()` does nothing.
function isDenoTestRun() {
  const ops = globalThis.Deno?.[globalThis.Deno.internal]?.core?.ops;
  return typeof ops?.op_register_test === "function";
}

// The reporter of the run-mode runner, or null for `deno test`'s own: as
// Node, `spec` unless `--test-reporter` names `tap` or `dot`. Under `deno
// test` only `tap` takes the run-mode runner (Node's snapshot fixtures).
function getReporterKind() {
  const reporter = getNodeTestReporter();
  if (reporter === "tap") return "tap";
  if (isDenoTestRun()) return null;
  return reporter === "dot" || reporter === "junit" ? reporter : "spec";
}

function isReporterMode() {
  return getReporterKind() !== null;
}

function getTapSuiteALS() {
  if (tapSuiteALS !== null) return tapSuiteALS;
  const mod = core.loadExtScript("ext:deno_node/async_hooks.ts");
  const ALS = mod.AsyncLocalStorage;
  tapSuiteALS = new ALS();
  return tapSuiteALS;
}

function getTapCurrentSuite() {
  if (tapSuiteALS !== null) {
    const fromAls = tapSuiteALS.getStore();
    if (fromAls !== undefined) return fromAls;
  }
  return tapCurrentSuiteSync;
}

// Parse `--test-skip-pattern` from NODE_OPTIONS so the TAP-mode polyfill can
// filter tests Node-style. A bare string is interpreted as a regex source;
// `/.../flags` is a regex literal.
function parsePatternFlag(flag) {
  const env = globalThis.Deno?.env;
  if (!env) return null;
  let nodeOptions = "";
  try {
    nodeOptions = env.get("NODE_OPTIONS") || "";
  } catch { /* permission denied */ }
  if (!nodeOptions) return null;
  const out = [];
  const re = new SafeRegExp(`${flag}(?:=|\\s+)(\\S+)`, "g");
  let m;
  while ((m = RegExpPrototypeExec(re, nodeOptions)) !== null) {
    const value = m[1];
    let pattern;
    const litMatch = StringPrototypeMatch(
      value,
      new SafeRegExp(/^\/(.*)\/([a-z]*)$/),
    );
    if (litMatch) {
      try {
        pattern = new SafeRegExp(litMatch[1], litMatch[2]);
      } catch {
        continue;
      }
    } else {
      try {
        pattern = new SafeRegExp(value);
      } catch {
        continue;
      }
    }
    ArrayPrototypePush(out, pattern);
  }
  return out.length > 0 ? out : null;
}

let testSkipPatternCache;
function getTestSkipPatterns() {
  if (testSkipPatternCache !== undefined) return testSkipPatternCache;
  testSkipPatternCache = parsePatternFlag("--test-skip-pattern");
  return testSkipPatternCache;
}

let testOnlyFlagCache;
function isTestOnlyFlagSet() {
  if (testOnlyFlagCache !== undefined) return testOnlyFlagCache;
  const env = globalThis.Deno?.env;
  if (!env) {
    testOnlyFlagCache = false;
    return false;
  }
  let nodeOptions = "";
  try {
    nodeOptions = env.get("NODE_OPTIONS") || "";
  } catch { /* permission denied */ }
  testOnlyFlagCache = RegExpPrototypeTest(
    new SafeRegExp(/(^|\s)--test-only(\s|=|$)/),
    nodeOptions,
  );
  return testOnlyFlagCache;
}

// `--test-randomize` / `--test-random-seed=<uint32>` (Node 26,
// `lib/internal/test_runner/utils.js` parseCommandLine): a seed turns
// randomization on; `--test-randomize` alone draws one. Null when off. Read
// from NODE_OPTIONS, where the node shim forwards both flags.
let testRandomSeedCache;
function getTestRandomSeed() {
  if (testRandomSeedCache !== undefined) return testRandomSeedCache;
  let nodeOptions = "";
  try {
    nodeOptions = globalThis.Deno?.env?.get("NODE_OPTIONS") || "";
  } catch { /* permission denied */ }
  const seed = RegExpPrototypeExec(
    new SafeRegExp(/(?:^|\s)--test-random-seed(?:=|\s+)(\d+)(?:\s|$)/),
    nodeOptions,
  );
  if (seed !== null && NumberIsInteger(+seed[1]) && +seed[1] <= 0xFFFF_FFFF) {
    testRandomSeedCache = +seed[1];
  } else if (
    RegExpPrototypeTest(
      new SafeRegExp(/(?:^|\s)--test-randomize(?:\s|=true|$)/),
      nodeOptions,
    )
  ) {
    // Node's `createRandomSeed()`.
    testRandomSeedCache = MathFloor(MathRandom() * (0xFFFF_FFFF + 1));
  } else {
    testRandomSeedCache = null;
  }
  return testRandomSeedCache;
}

// Node's `createSeededGenerator()`: Mulberry32 from a uint32 seed. Every test,
// suite and the root draw from their own generator with the run's seed, so a
// seed gives the same order as in Node.
function createSeededGenerator(seed) {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6D2B79F5) | 0;
    let value = MathImul(state ^ state >>> 15, 1 | state);
    value ^= value + MathImul(value ^ value >>> 7, 61 | value);
    return ((value ^ value >>> 14) >>> 0) / 4_294_967_296;
  };
}

// The generator a parent dequeues its subtests with, or null when the run is
// not randomized.
function createSubtestQueueRandom() {
  const seed = getTestRandomSeed();
  return seed === null ? null : createSeededGenerator(seed);
}

// Node's `dequeuePendingSubtest()` over `list[start..]`: with a generator and
// two or more pending, a uniformly random one is moved to `start` (the one to
// run next); otherwise the next in declaration order stays there.
function dequeueSubtest(list, start, random) {
  const pending = list.length - start;
  if (random === null || pending < 2) return;
  const index = start + MathFloor(random() * pending);
  if (index === start) return;
  const [entry] = ArrayPrototypeSplice(list, index, 1);
  ArrayPrototypeSplice(list, start, 0, entry);
}

const TEST_ONLY_WARNING =
  "# 'only' and 'runOnly' require the --test-only command-line option.";

function matchesAnyPattern(name, patterns) {
  for (const p of new SafeArrayIterator(patterns)) {
    if (RegExpPrototypeTest(p, name)) return true;
  }
  return false;
}

// Returns true if the given test/suite name should be excluded from the run.
function shouldSkipByPattern(name) {
  const skip = getTestSkipPatterns();
  if (skip && matchesAnyPattern(String(name), skip)) return true;
  return false;
}

// Top-level queue of test/suite entries collected synchronously while the
// script body evaluates. Children of describe() blocks live under their parent
// entry's `children` array, populated during the synchronous descend through
// the describe body.
const tapTopEntries = [];
let tapRunScheduled = false;
// AsyncLocalStorage tracking the currently-active describe() so that test()
// and describe() calls made inside an async describe body - even after
// `await` boundaries - still register against the surrounding suite. The
// fallback variable handles synchronous nesting before ALS is loaded.
let tapCurrentSuiteSync = null;
let tapSuiteALS = null;
const tapStats = {
  tests: 0,
  suites: 0,
  pass: 0,
  fail: 0,
  cancelled: 0,
  skipped: 0,
  todo: 0,
};

function tapWrite(line) {
  // deno-lint-ignore no-console
  console.log(line);
}

function tapDirective(options) {
  if (options.skip) {
    const msg = options.skip === true ? "" : tapEscape(String(options.skip));
    return msg ? ` # SKIP ${msg}` : " # SKIP";
  }
  if (options.todo) {
    const msg = options.todo === true ? "" : tapEscape(String(options.todo));
    return msg ? ` # TODO ${msg}` : " # TODO";
  }
  return "";
}

// The run's clock (Node's `duration_ms` values are fractional milliseconds).
function nowMs() {
  return globalThis.performance.now();
}

// Node 26.5.1's `ERR_TEST_FAILURE` failure types (lib/internal/test_runner/
// test.js) the reporters print.
const kTestCodeFailure = "testCodeFailure";
const kHookFailure = "hookFailed";
const kSubtestsFailed = "subtestsFailed";

// A test's error as Node reports it: an `ERR_TEST_FAILURE` whose `cause` is
// what the test threw.
function testFailure(cause, failureType) {
  const message = failureType === kSubtestsFailed
    ? cause
    : (cause?.message ?? String(cause));
  const err = new Error(message);
  err.code = "ERR_TEST_FAILURE";
  err.failureType = failureType;
  if (failureType !== kSubtestsFailed) err.cause = cause;
  return err;
}

function subtestsFailedError(count) {
  const err = testFailure(
    `${count} subtest${count > 1 ? "s" : ""} failed`,
    kSubtestsFailed,
  );
  // Node reports no stack for it.
  ObjectDefineProperty(err, "stack", { __proto__: null, value: undefined });
  return err;
}

// Where `test()` / `describe()` was called, for the reporters' `location`
// (Node's `loc`): the first stack frame outside the runtime. Code without a
// file (`eval`) has none, as in Node.
function captureTestLocation() {
  const stack = new Error().stack;
  if (typeof stack !== "string") return undefined;
  const lines = StringPrototypeSplit(stack, "\n");
  for (let i = 1; i < lines.length; i++) {
    const line = lines[i];
    const m = RegExpPrototypeExec(
      new SafeRegExp(/\(?([^\s()]+):(\d+):(\d+)\)?$/),
      line,
    );
    if (m === null) continue;
    let file = m[1];
    if (
      StringPrototypeStartsWith(file, "ext:") ||
      StringPrototypeStartsWith(file, "node:")
    ) {
      continue;
    }
    if (StringPrototypeStartsWith(file, "<")) return undefined;
    if (StringPrototypeStartsWith(file, "file://")) {
      try {
        file = lazyFileURLToPath()(file);
      } catch { /* keep the URL */ }
    }
    return { file, line: m[2], column: m[3] };
  }
  return undefined;
}

let _fileURLToPath = null;
function lazyFileURLToPath() {
  _fileURLToPath ??= core.loadExtScript("ext:deno_node/url.ts").fileURLToPath;
  return _fileURLToPath;
}

const kLineBreak = new SafeRegExp(/\r?\n/);

// Node's `jsToYaml` (reporter/tap.js) for one test's details.
function tapYamlValue(indent, name, value, seen) {
  if (value === undefined) return "";
  const prefix = `${indent}  ${name}:`;
  if (value === null) return `${prefix} ~\n`;
  if (typeof value !== "object") {
    if (typeof value !== "string") {
      return `${prefix} ${lazyInspect()(value, { breakLength: Infinity })}\n`;
    }
    const lines = StringPrototypeSplit(value, kLineBreak);
    if (lines.length === 1) {
      return `${prefix} ${lazyInspect()(value, { breakLength: Infinity })}\n`;
    }
    let str = `${prefix} |-\n`;
    for (let i = 0; i < lines.length; i++) {
      str += `${indent}    ${lines[i]}\n`;
    }
    return str;
  }
  SetPrototypeAdd(seen, value);
  const isErrorObj = ObjectPrototypeIsPrototypeOf(ErrorPrototype, value);
  let propsIndent = indent;
  let result = "";
  if (name !== null) {
    result += `${prefix}\n`;
    propsIndent += "  ";
  }
  const keys = ObjectKeys(value);
  for (let i = 0; i < keys.length; i++) {
    const key = keys[i];
    const v = value[key];
    if (isErrorObj && (key === "cause" || key === "code")) continue;
    if (SetPrototypeHas(seen, v)) {
      result += `${propsIndent}  ${key}: <Circular>\n`;
      continue;
    }
    result += tapYamlValue(propsIndent, key, v, seen);
  }
  if (isErrorObj) {
    const { cause, code, failureType } = value;
    let errMsg = value.message ?? "<unknown error>";
    let errName = value.name;
    let errStack = value.stack;
    let errCode = code;
    let errExpected = value.expected;
    let errActual = value.actual;
    let errOperator = value.operator;
    let errIsAssertion = isAssertionLike(value);
    if (
      code === "ERR_TEST_FAILURE" &&
      (failureType === kTestCodeFailure || failureType === kHookFailure)
    ) {
      errStack = cause?.stack ?? errStack;
      errCode = cause?.code ?? errCode;
      errName = cause?.name ?? errName;
      errMsg = cause?.message ?? (cause === undefined ? errMsg : String(cause));
      if (isAssertionLike(cause)) {
        errExpected = cause.expected;
        errActual = cause.actual;
        errOperator = cause.operator ?? errOperator;
        errIsAssertion = true;
      }
    }
    result += tapYamlValue(indent, "error", errMsg, seen);
    if (errCode) result += tapYamlValue(indent, "code", errCode, seen);
    if (errName && errName !== "Error") {
      result += tapYamlValue(indent, "name", errName, seen);
    }
    if (errIsAssertion) {
      result += tapYamlValue(indent, "expected", errExpected, copySet(seen));
      result += tapYamlValue(indent, "actual", errActual, copySet(seen));
      if (errOperator) {
        result += tapYamlValue(indent, "operator", errOperator, seen);
      }
    }
    if (typeof errStack === "string") {
      const frames = [];
      const lines = StringPrototypeSplit(errStack, kLineBreak);
      for (let i = 0; i < lines.length; i++) {
        const frame = lines[i];
        const processed = StringPrototypeReplace(
          frame,
          new SafeRegExp(/^\s+at\s+/),
          "",
        );
        if (processed.length > 0 && processed.length !== frame.length) {
          ArrayPrototypePush(frames, processed);
        }
      }
      if (frames.length > 0) {
        const delimiter = `\n${indent}    `;
        result += `${indent}  stack: |-${delimiter}`;
        result += `${ArrayPrototypeJoin(frames, delimiter)}\n`;
      }
    }
  }
  return result;
}

function copySet(set) {
  const copy = new SafeSet();
  for (const value of new SafeSetIterator(set)) SetPrototypeAdd(copy, value);
  return copy;
}

function isAssertionLike(value) {
  return value !== null && typeof value === "object" &&
    ReflectHas(value, "expected") && ReflectHas(value, "actual");
}

let _inspect = null;
function lazyInspect() {
  _inspect ??= core.loadExtScript("ext:deno_node/internal/util/inspect.mjs")
    .inspect;
  return _inspect;
}

// The reporters of the run-mode runner (Node's `tap`, `spec` and `dot`, from
// lib/internal/test_runner/reporter/), fed the same events Node's are:
// `start` (test:start), `end` (test:pass / test:fail), `plan`, `diagnostic`
// and, at the end, `summary`.
const kSymbols = {
  __proto__: null,
  fail: "\u2716 ",
  pass: "\u2714 ",
  info: "\u2139 ",
  arrow: "\u25B6 ",
  skip: "\uFE63 ",
  warn: "\u26A0 ",
};

function reporterColors() {
  let enabled = false;
  try {
    const env = globalThis.Deno?.env;
    const force = env?.get("FORCE_COLOR");
    if (force !== undefined && force !== "" && force !== "0") {
      enabled = true;
    } else if (env?.get("NO_COLOR") === undefined) {
      enabled = globalThis.Deno?.stdout?.isTerminal?.() === true;
    }
  } catch { /* no env access: no colors */ }
  const c = (code) => enabled ? `\x1b[${code}m` : "";
  return {
    red: c(31),
    green: c(32),
    yellow: c(33),
    blue: c(34),
    gray: c(90),
    white: c(39),
    reset: c(0),
  };
}

function specIndent(nesting) {
  return StringPrototypeRepeat("  ", nesting);
}

class TapReporter {
  header() {
    tapWrite("TAP version 13");
  }

  start(entry, depth) {
    tapWrite(`${tapIndent(depth)}# Subtest: ${tapEscape(entry.name)}`);
  }

  plan(depth, count) {
    tapWrite(`${tapIndent(depth)}1..${count}`);
  }

  end(entry, depth, n, result) {
    const indent = tapIndent(depth);
    tapWrite(
      `${indent}${result.failed ? "not ok" : "ok"} ${n} - ${
        tapEscape(entry.name)
      }${tapDirective(entry.options)}`,
    );
    let details = `${indent}  ---\n`;
    details += tapYamlValue(indent, "duration_ms", result.durationMs);
    details += tapYamlValue(indent, "type", result.isSuite ? "suite" : "test");
    if (result.failed && entry.loc !== undefined) {
      details += tapYamlValue(
        indent,
        "location",
        `${entry.loc.file}:${entry.loc.line}:${entry.loc.column}`,
      );
    }
    if (result.error !== undefined) {
      details += tapYamlValue(indent, null, result.error, new SafeSet());
    }
    details += `${indent}  ...`;
    tapWrite(details);
  }

  diagnostic(depth, message) {
    tapWrite(`${tapIndent(depth)}# ${tapEscape(message)}`);
  }

  summary(n, stats, durationMs) {
    tapWrite(`1..${n}`);
    for (const line of new SafeArrayIterator(summaryLines(stats, durationMs))) {
      tapWrite(`# ${line}`);
    }
  }
}

function summaryLines(stats, durationMs) {
  return [
    `tests ${stats.tests}`,
    `suites ${stats.suites}`,
    `pass ${stats.pass}`,
    `fail ${stats.fail}`,
    `cancelled ${stats.cancelled}`,
    `skipped ${stats.skipped}`,
    `todo ${stats.todo}`,
    `duration_ms ${durationMs}`,
  ];
}

// Node's `formatTestReport` (reporter/utils.js).
function formatTestReport(colors, failed, entry, result, showError, indent) {
  let color = failed ? colors.red : colors.green;
  let symbol = failed ? kSymbols.fail : kSymbols.pass;
  const { skip, todo } = entry.options;
  const duration = result.durationMs
    ? ` ${colors.gray}(${result.durationMs}ms)${colors.white}`
    : "";
  let title = `${entry.name}${duration}`;
  if (skip) {
    title += ` # ${typeof skip === "string" && skip.length ? skip : "SKIP"}`;
    color = colors.gray;
    symbol = kSymbols.skip;
  } else if (todo) {
    title += ` # ${typeof todo === "string" && todo.length ? todo : "TODO"}`;
    if (failed) {
      color = colors.yellow;
      symbol = kSymbols.warn;
    }
  } else if (entry.options.expectFailure) {
    title += " # EXPECTED FAILURE";
  }
  let err = "";
  if (showError && result.error !== undefined) {
    const error = result.error;
    const shown = error?.code === "ERR_TEST_FAILURE" &&
        error.failureType !== kSubtestsFailed
      ? error.cause
      : error;
    const text = ArrayPrototypeJoin(
      StringPrototypeSplit(
        lazyInspect()(shown, { breakLength: Infinity }),
        kLineBreak,
      ),
      `\n${indent}  `,
    );
    err = `\n${indent}  ${text}\n`;
  }
  return `${indent}${color}${symbol}${title}${colors.white}${err}`;
}

// The spec and dot reporters write through a buffer flushed on the next
// macrotask, as Node's reporter stream reaches stdout after the test code
// that ran in the same turn.
class BufferedReporter {
  #chunks = [];
  #flushScheduled = false;

  write(text) {
    ArrayPrototypePush(this.#chunks, text);
    if (!this.#flushScheduled) {
      this.#flushScheduled = true;
      ensureRealTimers();
      realSetTimeout(() => this.flush(), 0);
    }
  }

  flush() {
    this.#flushScheduled = false;
    if (this.#chunks.length === 0) return;
    const text = ArrayPrototypeJoin(this.#chunks, "");
    this.#chunks = [];
    lazyProcess().default.stdout.write(text);
  }
}

class SpecReporter extends BufferedReporter {
  #colors = reporterColors();
  // Tests started but not yet printed (the arrow line once a subtest reports).
  #stack = [];
  #failed = [];

  header() {}

  start(entry, depth) {
    ArrayPrototypePush(this.#stack, { entry, depth, printed: false });
  }

  plan() {}

  end(entry, depth, _n, result) {
    let prefix = "";
    ArrayPrototypePop(this.#stack);
    for (let i = 0; i < this.#stack.length; i++) {
      const parent = this.#stack[i];
      if (parent.printed) continue;
      parent.printed = true;
      prefix += `${
        specIndent(parent.depth)
      }${kSymbols.arrow}${parent.entry.name}\n`;
    }
    if (result.failed && result.error?.failureType !== kSubtestsFailed) {
      ArrayPrototypePush(this.#failed, { entry, result });
    }
    this.write(
      `${prefix}${
        formatTestReport(
          this.#colors,
          result.failed,
          entry,
          result,
          false,
          specIndent(depth),
        )
      }\n`,
    );
  }

  diagnostic(depth, message) {
    const c = this.#colors;
    this.write(
      `${c.blue}${specIndent(depth)}${kSymbols.info}${message}${c.white}\n`,
    );
  }

  summary(_n, stats, durationMs) {
    for (const line of new SafeArrayIterator(summaryLines(stats, durationMs))) {
      this.diagnostic(0, line);
    }
    this.write(formatFailedTests(this.#colors, this.#failed));
    this.flush();
  }
}

// The failing tests the spec reporter lists after the run.
function formatFailedTests(colors, failed) {
  if (failed.length === 0) return "";
  const results = [
    `\n${colors.red}${kSymbols.fail}failing tests:${colors.white}\n`,
  ];
  let cwd = "";
  try {
    cwd = lazyProcess().default.cwd();
  } catch { /* no cwd: absolute paths */ }
  for (let i = 0; i < failed.length; i++) {
    const { entry, result } = failed[i];
    if (entry.loc !== undefined) {
      const rel = cwd
        ? getPathForSnapshot().relative(cwd, entry.loc.file)
        : entry.loc.file;
      ArrayPrototypePush(
        results,
        `test at ${rel}:${entry.loc.line}:${entry.loc.column}`,
      );
    }
    ArrayPrototypePush(
      results,
      formatTestReport(colors, true, entry, result, true, ""),
    );
  }
  return ArrayPrototypeJoin(results, "\n");
}

// Node's `dot` reporter: a `.` or `X` per test, wrapped at the terminal's
// width (at least 20), then the failed tests.
class DotReporter extends BufferedReporter {
  #colors = reporterColors();
  #failed = [];
  #count = 0;
  #columns = dotLineLength();

  header() {}
  start() {}
  plan() {}
  diagnostic() {}

  end(entry, _depth, _n, result) {
    const c = this.#colors;
    if (result.failed) {
      this.write(`${c.red}X${c.reset}`);
      ArrayPrototypePush(this.#failed, { entry, result });
    } else {
      this.write(`${c.green}.${c.reset}`);
    }
    if (++this.#count === this.#columns) {
      this.write("\n");
      // Read again in case the terminal was resized.
      this.#columns = dotLineLength();
      this.#count = 0;
    }
  }

  summary() {
    const c = this.#colors;
    this.write("\n");
    if (this.#failed.length > 0) {
      this.write(`\n${c.red}Failed tests:${c.white}\n\n`);
      for (const { entry, result } of new SafeArrayIterator(this.#failed)) {
        this.write(formatTestReport(c, true, entry, result, true, ""));
      }
    }
    this.flush();
  }
}

// Node's `junit` reporter (`lib/internal/test_runner/reporter/junit.js`): the
// run as one `<testsuites>` document, written once the run is done. A test
// with subtests is a `<testsuite>`, any other a `<testcase>`; diagnostics,
// the summary's included, are XML comments.
function junitEscapeContent(s = "") {
  return StringPrototypeReplace(
    StringPrototypeReplace(s, new SafeRegExp(/(&)(?!#\d{1,7};)/g), "&amp;"),
    new SafeRegExp(/</g),
    "&lt;",
  );
}

function junitEscapeAttribute(s = "") {
  return junitEscapeContent(
    StringPrototypeReplace(
      StringPrototypeReplace(s, new SafeRegExp(/\n/g), "&#10;"),
      new SafeRegExp(/"/g),
      "&quot;",
    ),
  );
}

function junitEscapeComment(s = "") {
  return StringPrototypeReplace(s, new SafeRegExp(/--/g), "&#45;&#45;");
}

function junitTreeToXML(tree) {
  if (typeof tree === "string") {
    return `${junitEscapeContent(tree)}\n`;
  }
  const { tag, attrs, nesting, children, comment } = tree;
  const indent = StringPrototypeRepeat("\t", nesting + 1);
  if (comment != null) {
    return `${indent}<!-- ${junitEscapeComment(comment)} -->\n`;
  }
  const attrsString = ArrayPrototypeJoin(
    ArrayPrototypeMap(
      ObjectEntries(attrs),
      ({ 0: key, 1: value }) =>
        `${key}="${junitEscapeAttribute(String(value))}"`,
    ),
    " ",
  );
  if (!children?.length) {
    return `${indent}<${tag} ${attrsString}/>\n`;
  }
  let childrenString = "";
  for (const child of new SafeArrayIterator(children)) {
    childrenString += junitTreeToXML(child);
  }
  return `${indent}<${tag} ${attrsString}>\n${childrenString}${indent}</${tag}>\n`;
}

function junitIsFailure(node) {
  if (node?.attrs?.failures) return true;
  if (!node?.children) return false;
  for (const child of new SafeArrayIterator(node.children)) {
    if (child.tag === "failure") return true;
  }
  return false;
}

function junitIsSkipped(node) {
  if (node?.attrs?.skipped) return true;
  if (!node?.children) return false;
  for (const child of new SafeArrayIterator(node.children)) {
    if (child.tag === "skipped") return true;
  }
  return false;
}

function junitHostname() {
  try {
    return Deno.hostname();
  } catch {
    return "";
  }
}

class JunitReporter {
  #roots = [];
  #current = null;

  header() {}
  plan() {}

  start(entry, depth) {
    const node = {
      __proto__: null,
      attrs: { __proto__: null, name: entry.name },
      nesting: depth,
      parent: this.#current,
      children: [],
    };
    ArrayPrototypePush(this.#current?.children ?? this.#roots, node);
    this.#current = node;
  }

  end(entry, depth, _n, result) {
    const node = this.#current;
    this.#current = node.parent;
    node.attrs.time = NumberPrototypeToFixed(result.durationMs / 1000, 6);
    let tests = 0;
    let failures = 0;
    let skipped = 0;
    for (const child of new SafeArrayIterator(node.children)) {
      if (child.comment != null) continue;
      tests++;
      if (junitIsFailure(child)) failures++;
      if (junitIsSkipped(child)) skipped++;
    }
    if (tests > 0) {
      node.tag = "testsuite";
      node.attrs.disabled = 0;
      node.attrs.errors = 0;
      node.attrs.tests = tests;
      node.attrs.failures = failures;
      node.attrs.skipped = skipped;
      node.attrs.timestamp = DatePrototypeToISOString(
        new Date(DateNow() - result.durationMs),
      );
      node.attrs.hostname = junitHostname();
      return;
    }
    node.tag = "testcase";
    node.attrs.classname = "test";
    if (entry.loc?.file) node.attrs.file = entry.loc.file;
    const { skip, todo } = entry.options;
    if (skip) {
      ArrayPrototypePush(node.children, {
        __proto__: null,
        nesting: depth + 1,
        tag: "skipped",
        attrs: {
          __proto__: null,
          type: "skipped",
          message: typeof skip === "string" && skip.length ? skip : true,
        },
      });
    }
    if (todo) {
      ArrayPrototypePush(node.children, {
        __proto__: null,
        nesting: depth + 1,
        tag: "skipped",
        attrs: {
          __proto__: null,
          type: "todo",
          message: typeof todo === "string" && todo.length ? todo : true,
        },
      });
    }
    if (result.failed) {
      const error = result.error;
      const message = typeof error?.message === "string" ? error.message : "";
      ArrayPrototypePush(node.children, {
        __proto__: null,
        nesting: depth + 1,
        tag: "failure",
        attrs: {
          __proto__: null,
          type: error?.failureType || error?.code,
          message: StringPrototypeTrim(message),
        },
        children: [
          lazyInspect()(error, { colors: false, breakLength: Infinity }),
        ],
      });
      node.attrs.failure = message;
    }
  }

  diagnostic(depth, message) {
    ArrayPrototypePush(this.#current?.children ?? this.#roots, {
      __proto__: null,
      nesting: depth,
      comment: message,
    });
  }

  summary(_n, stats, durationMs) {
    for (const line of new SafeArrayIterator(summaryLines(stats, durationMs))) {
      this.diagnostic(0, line);
    }
    let out = '<?xml version="1.0" encoding="utf-8"?>\n<testsuites>\n';
    for (const root of new SafeArrayIterator(this.#roots)) {
      out += junitTreeToXML(root);
    }
    out += "</testsuites>\n";
    lazyProcess().default.stdout.write(out);
  }
}

function dotLineLength() {
  let columns;
  try {
    columns = lazyProcess().default.stdout.columns;
  } catch { /* no stdout */ }
  return MathMax(columns ?? 20, 20);
}

let runReporter = null;
function getRunReporter() {
  if (runReporter === null) {
    const kind = getReporterKind();
    runReporter = kind === "tap"
      ? new TapReporter()
      : kind === "dot"
      ? new DotReporter()
      : kind === "junit"
      ? new JunitReporter()
      : new SpecReporter();
  }
  return runReporter;
}

// The test context of the run-mode runner (named as Node's, which stack
// traces through a test body show).
class TestContext {
  #diagnostics = [];
  #name;
  #depth;
  #subtestTail = PromiseResolve();
  #subtestCount = 0;
  #failedSubtests = 0;
  #parentChildren;
  #abortController = new AbortController();
  // Per-context "warning printed" flag for the `--test-only` diagnostic.
  // Mutated by `runTapEntry` when a child uses `only: true`.
  onlyWarningEmitted = false;
  // When set via `runOnly(true)`, only direct subtests registered with the
  // `only: true` option are executed; all other subtests are filtered out.
  #runOnly = false;
  #assert;
  #ancestors;
  // `t.before()` / `t.after()` / `t.beforeEach()` / `t.afterEach()`.
  beforeAllHooks = [];
  afterAllHooks = [];
  beforeEachHooks = [];
  afterEachHooks = [];
  #beforeRan = false;
  // Randomized runs: subtests wait here until the parent picks them.
  #random = createSubtestQueueRandom();
  #pending = [];
  #pendingScheduled = false;
  #dequeuing = null;
  #reportOrder = 0;

  constructor(name, depth, parentChildren, ancestors) {
    this.#name = name;
    this.#depth = depth;
    this.#parentChildren = parentChildren;
    this.#ancestors = ancestors;
  }

  get name() {
    return this.#name;
  }

  // Node's: the names of the enclosing suites and tests, then this one's.
  get fullName() {
    let fullName = "";
    for (const ancestor of new SafeArrayIterator(this.#ancestors)) {
      fullName += `${ancestor.name} > `;
    }
    return fullName + this.#name;
  }

  get filePath() {
    return getEntryFilePath();
  }

  get signal() {
    return this.#abortController.signal;
  }

  // Aborts this test's own AbortSignal (t.signal); see NodeTestContext._abort.
  _abort(reason) {
    if (!this.#abortController.signal.aborted) {
      this.#abortController.abort(reason);
    }
  }

  get assert() {
    this.#assert ??= createContextAssert(this, () => undefined);
    return this.#assert;
  }

  get mock() {
    return mock;
  }

  diagnostic(message) {
    ArrayPrototypePush(this.#diagnostics, String(message));
  }

  _drainDiagnostics() {
    const out = this.#diagnostics;
    this.#diagnostics = [];
    return out;
  }

  before(fn) {
    validateFunction(fn, "fn");
    ArrayPrototypePush(this.beforeAllHooks, fn);
  }

  after(fn) {
    validateFunction(fn, "fn");
    ArrayPrototypePush(this.afterAllHooks, fn);
  }

  beforeEach(fn) {
    validateFunction(fn, "fn");
    ArrayPrototypePush(this.beforeEachHooks, fn);
  }

  afterEach(fn) {
    validateFunction(fn, "fn");
    ArrayPrototypePush(this.afterEachHooks, fn);
  }

  // Subtest registration: `t.test(name, opts?, fn?)` queues a subtest that runs
  // sequentially in the order it was registered. Concurrent calls (Promise.all)
  // are serialized through the parent's subtest tail.
  runOnly(value) {
    this.#runOnly = !!value;
    return null;
  }

  test(name, options, fn) {
    const prepared = prepareOptions(name, options, fn, {});
    // In run-only mode, subtests not flagged with `only: true` are filtered
    // out entirely: they are neither registered nor reported, matching Node.
    if (this.#runOnly && !prepared.options.only) {
      return PromiseResolve();
    }
    this.#subtestCount++;
    const entry = newRunEntry(prepared, "test");
    entry.childNumber = this.#subtestCount;
    if (this.#parentChildren) {
      ArrayPrototypePush(this.#parentChildren, entry);
    }
    if (this.#random !== null) {
      // Node's randomized queue (`Test#start`): the first start waits a
      // microtask, so the siblings created in the same synchronous turn are
      // all eligible for the first pick.
      const deferred = PromiseWithResolvers();
      ArrayPrototypePush(this.#pending, { entry, deferred });
      if (!this.#pendingScheduled) {
        this.#pendingScheduled = true;
        queueMicrotask(() => {
          this.#pendingScheduled = false;
          this.#processPending();
        });
      }
      return deferred.promise;
    }
    this.#reportOrder++;
    const p = this.#runSubtest(entry, this.#reportOrder);
    this.#subtestTail = PromisePrototypeThen(p, () => {}, () => {});
    return p;
  }

  #runSubtest(entry, n) {
    // deno-lint-ignore no-this-alias
    const parentState = this;
    const ancestors = ArrayPrototypeSlice(this.#ancestors);
    ArrayPrototypePush(ancestors, this);
    const childDepth = this.#depth + 1;
    return PromisePrototypeThen(
      this.#subtestTail,
      async () => {
        if (!parentState.#beforeRan) {
          parentState.#beforeRan = true;
          await runHooks(parentState.beforeAllHooks, parentState);
        }
        const failed = await runTapEntry(
          entry,
          childDepth,
          n,
          parentState,
          ancestors,
        );
        if (failed) parentState.#failedSubtests++;
      },
    );
  }

  // Node's `processPendingSubtests()` with one subtest at a time: pick a
  // pending sibling at random, run it, and pick again among what is pending
  // then (subtests it registered on this context included). The report
  // number is the pick's position, as Node assigns it at dequeue time.
  #processPending() {
    if (this.#dequeuing !== null) return;
    this.#dequeuing = (async () => {
      try {
        while (this.#pending.length > 0) {
          const pending = this.#pending;
          const index = pending.length < 2
            ? 0
            : MathFloor(this.#random() * pending.length);
          const [{ entry, deferred }] = ArrayPrototypeSplice(
            pending,
            index,
            1,
          );
          this.#reportOrder++;
          const p = this.#runSubtest(entry, this.#reportOrder);
          this.#subtestTail = PromisePrototypeThen(p, () => {}, () => {});
          try {
            await p;
            deferred.resolve();
          } catch (err) {
            deferred.reject(err);
          }
        }
      } finally {
        this.#dequeuing = null;
      }
    })();
  }

  async _drainSubtests() {
    // A randomized parent's queue may still hold subtests (or be waiting for
    // the microtask that starts it).
    while (
      this.#pending.length > 0 || this.#pendingScheduled ||
      this.#dequeuing !== null
    ) {
      if (this.#dequeuing !== null) await this.#dequeuing;
      else await PromiseResolve();
      if (this.#pending.length > 0 && this.#dequeuing === null) {
        this.#processPending();
      }
    }
    return this.#subtestTail;
  }

  _subtestCount() {
    return this.#subtestCount;
  }

  _failedSubtests() {
    return this.#failedSubtests;
  }
}

function newRunEntry(prepared, kind) {
  return {
    name: prepared.name,
    fn: prepared.fn,
    options: prepared.options,
    kind,
    children: [],
    bodyPromise: null,
    bodyError: null,
    loc: captureTestLocation(),
    beforeAllHooks: [],
    afterAllHooks: [],
    beforeEachHooks: [],
    afterEachHooks: [],
  };
}

async function runHooks(hooks, ctx) {
  for (const hook of new SafeArrayIterator(hooks)) {
    const r = ReflectApply(hook, ctx, [ctx]);
    if (isThenable(r)) await r;
  }
}

function scheduleTapRun() {
  if (tapRunScheduled) return;
  tapRunScheduled = true;
  // Defer to the macrotask queue so synchronous top-level test() calls finish
  // queueing before we start running.
  setTimeout(() => {
    runTapTop();
  }, 0);
}

async function runTapTop() {
  const reporter = getRunReporter();
  const started = nowMs();
  const topState = { onlyWarningEmitted: false };
  const rootRandom = createSubtestQueueRandom();
  let n = 0;
  // By index: a test registered while the run is going (after a top-level
  // `await`) runs too, as in Node.
  const runPending = async () => {
    // Hold the event loop open while tests run so that fixtures using
    // unref'd timers (Node's runner keeps itself alive internally) don't
    // cause Deno to exit before subtests complete.
    const keepAlive = setInterval(() => {}, 1 << 30);
    try {
      while (n < tapTopEntries.length) {
        dequeueSubtest(tapTopEntries, n, rootRandom);
        n++;
        await runTapEntry(tapTopEntries[n - 1], 0, n, topState, []);
      }
    } finally {
      clearInterval(keepAlive);
    }
  };
  // Match Node's ordering: top-level `before()` callbacks fire before the
  // `TAP version 13` line, so any console output they produce appears
  // before the reporter header in the captured stream.
  if (rootBeforeHooks.length > 0) {
    const rootCtx = { name: "<root>", fullName: "<root>" };
    for (const hook of new SafeArrayIterator(rootBeforeHooks)) {
      try {
        const r = ReflectApply(hook, null, [rootCtx]);
        if (isThenable(r)) await r;
      } catch {
        /* swallow to keep parity with Node's lenient hook errors */
      }
    }
  }
  reporter.header();
  await runPending();
  // Top-level `after()` hooks run once the tests are done, before the
  // reporter's output for them is flushed (Node's order in its fixtures).
  await runRootAfterHooks();
  const forceExit = isTestForceExit();
  if (!forceExit) {
    // Node's root test ends when the event loop is otherwise done
    // ('beforeExit'), so tests registered later (after a top-level `await`)
    // still run before the summary; from there on the run holds the loop
    // open itself until the summary is out.
    const { promise, resolve } = PromiseWithResolvers();
    lazyProcess().default.once("beforeExit", resolve);
    await promise;
    await runPending();
  }
  const keepAlive = setInterval(() => {}, 1 << 30);
  try {
    await finishRun();
  } finally {
    clearInterval(keepAlive);
  }
  const failed = tapStats.fail > 0 || tapStats.cancelled > 0;
  try {
    const process = lazyProcess().default;
    if (failed) process.exitCode = 1;
    // `--test-force-exit`: end once the tests are done, whatever else the
    // event loop holds.
    if (forceExit) process.exit();
  } catch { /* exit code unavailable */ }

  async function finishRun() {
    // Hooks registered by a late test's module, if any.
    await runRootAfterHooks();
    reporter.summary(n, tapStats, nowMs() - started);
  }
}

async function runRootAfterHooks() {
  if (rootAfterHooks.length > 0) {
    const rootCtx = { name: "<root>", fullName: "<root>" };
    const hooks = ArrayPrototypeSplice(
      rootAfterHooks,
      0,
      rootAfterHooks.length,
    );
    for (const hook of new SafeArrayIterator(hooks)) {
      try {
        const r = ReflectApply(hook, null, [rootCtx]);
        if (isThenable(r)) await r;
      } catch { /* swallow */ }
    }
  }
}

function isTestForceExit() {
  let nodeOptions = "";
  try {
    nodeOptions = globalThis.Deno?.env?.get("NODE_OPTIONS") || "";
  } catch { /* permission denied */ }
  return RegExpPrototypeTest(
    new SafeRegExp(/(?:^|\s)--test-force-exit(?:\s|=|$)/),
    nodeOptions,
  );
}

// Recursively run a test or suite entry, reporting it at the given nesting
// depth. `parentState` is the runtime state of the immediate parent (a
// TestContext for test-bodies, or a `{ onlyWarningEmitted }` object for
// suite/root scopes); it's used to emit the `# 'only' and 'runOnly' require
// the --test-only command-line option.` warning at most once per parent.
// `ancestors` are the suites and test contexts enclosing the entry, outermost
// first, whose `beforeEach` / `afterEach` hooks it runs (Node's inherited
// hooks). Returns whether the entry failed.
async function runTapEntry(
  entry,
  depth,
  n,
  parentState,
  ancestors,
) {
  const reporter = getRunReporter();
  const isSuite = entry.kind === "suite";
  reporter.start(entry, depth);
  const started = nowMs();

  let error;
  let diagnostics = [];
  let childCount = 0;

  // Each test/suite body gets its own state object so warnings emitted
  // because of an `only: true` child don't leak across siblings.
  const myChildrenState = { onlyWarningEmitted: false };

  if (entry.options.skip) {
    if (isSuite) {
      tapStats.suites++;
    } else {
      tapStats.skipped++;
      tapStats.tests++;
    }
  } else if (entry.options.todo) {
    // Per Node behavior, a TODO test that throws is not counted as a failure.
    // The runner skips invoking the body to match snapshot output.
    if (isSuite) {
      tapStats.suites++;
    } else {
      tapStats.todo++;
      tapStats.tests++;
    }
  } else if (isSuite) {
    try {
      if (entry.bodyError) throw entry.bodyError;
      if (entry.bodyPromise) await entry.bodyPromise;
    } catch (err) {
      error = testFailure(err, kTestCodeFailure);
    }
    if (error === undefined) {
      const suiteCtx = { name: entry.name, fullName: entry.name };
      try {
        if (entry.children.length > 0) {
          await runHooks(entry.beforeAllHooks, suiteCtx);
        }
      } catch (err) {
        error = testFailure(err, kHookFailure);
      }
      let failedChildren = 0;
      if (error === undefined) {
        const childAncestors = ArrayPrototypeSlice(ancestors);
        ArrayPrototypePush(childAncestors, entry);
        let childN = 0;
        const childRandom = createSubtestQueueRandom();
        for (let i = 0; i < entry.children.length; i++) {
          dequeueSubtest(entry.children, i, childRandom);
          const child = entry.children[i];
          childN++;
          if (
            await runTapEntry(
              child,
              depth + 1,
              childN,
              myChildrenState,
              childAncestors,
            )
          ) {
            failedChildren++;
          }
        }
        childCount = childN;
        try {
          if (entry.children.length > 0) {
            await runHooks(entry.afterAllHooks, suiteCtx);
          }
        } catch (err) {
          error = testFailure(err, kHookFailure);
        }
      }
      if (error === undefined && failedChildren > 0) {
        error = subtestsFailedError(failedChildren);
      }
    }
    tapStats.suites++;
  } else {
    // test/it body
    const ctx = new TestContext(
      entry.name,
      depth,
      entry.children,
      ancestors,
    );
    const eachHooks = (key) => {
      const hooks = key === "beforeEachHooks"
        ? ArrayPrototypeSlice(rootBeforeEachHooks)
        : [];
      for (const holder of new SafeArrayIterator(ancestors)) {
        for (const hook of new SafeArrayIterator(holder[key])) {
          ArrayPrototypePush(hooks, hook);
        }
      }
      if (key === "afterEachHooks") {
        // Innermost first, then the root's.
        const reversed = [];
        for (let i = hooks.length - 1; i >= 0; i--) {
          ArrayPrototypePush(reversed, hooks[i]);
        }
        for (const hook of new SafeArrayIterator(rootAfterEachHooks)) {
          ArrayPrototypePush(reversed, hook);
        }
        return reversed;
      }
      return hooks;
    };
    let hookStage = true;
    try {
      await runHooks(eachHooks("beforeEachHooks"), ctx);
      hookStage = false;
      await runWithTestGuards(async () => {
        const ret = ReflectApply(entry.fn, ctx, [ctx]);
        if (isThenable(ret)) await ret;
      }, {
        timeout: entry.options.timeout,
        signal: entry.options.signal,
        abort: (reason) => ctx._abort(reason),
      });
    } catch (err) {
      error = testFailure(err, hookStage ? kHookFailure : kTestCodeFailure);
    }
    // Wait for any t.test() calls the body didn't await, so their output is
    // reported before the parent's result.
    try {
      await ctx._drainSubtests();
    } catch { /* reported by the subtests */ }
    childCount = ctx._subtestCount();
    try {
      if (childCount > 0) await runHooks(ctx.afterAllHooks, ctx);
      await runHooks(eachHooks("afterEachHooks"), ctx);
    } catch (err) {
      error ??= testFailure(err, kHookFailure);
    }
    if (error === undefined && ctx._failedSubtests() > 0) {
      error = subtestsFailedError(ctx._failedSubtests());
    }
    if (error === undefined) tapStats.pass++;
    else tapStats.fail++;
    diagnostics = ctx._drainDiagnostics();
    tapStats.tests++;
  }
  if (isSuite && error !== undefined) tapStats.fail++;

  if (childCount > 0) {
    reporter.plan(depth + 1, childCount);
  }

  const failed = error !== undefined;
  // A test's number is its place among its siblings as declared (Node's
  // `childNumber`), which a randomized run reports out of order.
  reporter.end(entry, depth, entry.childNumber ?? n, {
    failed,
    error,
    isSuite,
    durationMs: nowMs() - started,
  });
  for (const d of new SafeArrayIterator(diagnostics)) {
    reporter.diagnostic(depth, d);
  }
  // If this entry was registered with `only: true` and the `--test-only` flag
  // wasn't supplied, Node prints a one-off warning in the parent's scope
  // immediately after the entry's yaml/diagnostics. Emit it at the same depth
  // and only once per parent.
  if (
    entry.options.only &&
    !isTestOnlyFlagSet() &&
    parentState &&
    !parentState.onlyWarningEmitted
  ) {
    reporter.diagnostic(
      depth,
      StringPrototypeSlice(TEST_ONLY_WARNING, 2),
    );
    parentState.onlyWarningEmitted = true;
  }
  return failed;
}

function queueTapTest(name, options, fn, overrides) {
  const prepared = prepareOptions(name, options, fn, overrides);
  if (shouldSkipByPattern(prepared.name)) {
    // Filtered out entirely: do not register or run.
    scheduleTapRun();
    return PromiseResolve();
  }
  const entry = newRunEntry(prepared, "test");
  const parentSuite = getTapCurrentSuite();
  if (parentSuite !== null) {
    entry.childNumber = ArrayPrototypePush(parentSuite.children, entry);
  } else {
    entry.childNumber = ArrayPrototypePush(tapTopEntries, entry);
    scheduleTapRun();
  }
  return PromiseResolve();
}

function queueTapSuite(name, options, fn, overrides) {
  const prepared = prepareOptions(name, options, fn, overrides);
  if (shouldSkipByPattern(prepared.name)) {
    // Filtered out entirely: do not register, but the suite body must not
    // run (it would otherwise add unwanted children to the parent).
    scheduleTapRun();
    return PromiseResolve();
  }
  const entry = newRunEntry(prepared, "suite");
  const parentSuite = getTapCurrentSuite();
  if (parentSuite !== null) {
    entry.childNumber = ArrayPrototypePush(parentSuite.children, entry);
  } else {
    entry.childNumber = ArrayPrototypePush(tapTopEntries, entry);
    scheduleTapRun();
  }
  // Evaluate the suite body inside an AsyncLocalStorage scope so any nested
  // describe()/test() calls - including those scheduled after `await` inside
  // the body - register against this suite, not the outer (or null) scope.
  // The sync fallback handles environments without ALS available.
  const als = getTapSuiteALS();
  const prev = tapCurrentSuiteSync;
  tapCurrentSuiteSync = entry;
  try {
    als.run(entry, () => {
      try {
        const ret = ReflectApply(prepared.fn, null, []);
        if (isThenable(ret)) {
          entry.bodyPromise = ret;
          // The runner awaits it when the suite runs; until then a rejection
          // is the suite's failure, not an unhandled one.
          PromisePrototypeThen(ret, undefined, noop);
        }
      } catch (err) {
        entry.bodyError = err;
      }
    });
  } finally {
    tapCurrentSuiteSync = prev;
  }
  return PromiseResolve();
}

function isThenable(value) {
  return value !== null && value !== undefined &&
    typeof value.then === "function";
}

function getExpectFailureMatch(expectFailure) {
  if (
    expectFailure !== null && typeof expectFailure === "object" &&
    ObjectPrototypeHasOwnProperty(expectFailure, "match")
  ) {
    return expectFailure.match;
  }
  if (
    expectFailure === true ||
    typeof expectFailure === "string" ||
    expectFailure === undefined
  ) {
    return undefined;
  }
  return expectFailure;
}

function assertExpectedFailure(err, expectFailure) {
  const match = getExpectFailureMatch(expectFailure);
  if (match !== undefined) {
    assert.throws(() => {
      throw err;
    }, match);
  }
}

// Runs `invoke()` (a thunk that executes a single test body) while enforcing
// the `timeout` option, honoring a caller-supplied abort `signal`, and capturing
// any unhandled rejection / uncaught exception that fires during the body. The
// returned promise resolves with the body's value, or rejects with the first
// failure observed: a body error, a timeout, an abort, or a captured rejection.
// `opts.abort(reason)` is invoked on timeout/abort so the test's own
// AbortSignal (`t.signal`) is aborted and user code can react.
async function runWithTestGuards(invoke, opts) {
  ensureRealTimers();
  const timeout = opts?.timeout;
  const signal = opts?.signal;
  const abort = opts?.abort;

  const failure = PromiseWithResolvers();
  const result = PromiseWithResolvers();
  const sink = {
    settled: false,
    fail(reason) {
      if (this.settled) return;
      this.settled = true;
      failure.reject(reason);
    },
  };
  pushTestSink(sink);

  let timeoutId = null;
  let onAbort = null;
  try {
    if (signal !== undefined && signal !== null) {
      if (signal.aborted) {
        if (typeof abort === "function") abort(signal.reason);
        sink.fail(buildAbortError(signal));
      } else {
        onAbort = () => {
          if (typeof abort === "function") abort(signal.reason);
          sink.fail(buildAbortError(signal));
        };
        signal.addEventListener("abort", onAbort, { once: true });
      }
    }

    if (typeof timeout === "number" && timeout > 0) {
      timeoutId = realSetTimeout(() => {
        const err = buildTimeoutError(timeout);
        if (typeof abort === "function") abort(err);
        sink.fail(err);
      }, timeout);
    }

    // Race the body against any externally-signalled failure. We avoid
    // Promise.race so a late body settlement cannot resurface after a failure.
    const body = (async () => {
      const value = await invoke();
      // Stop attributing as soon as the body settles. An unhandled rejection
      // whose `unhandledrejection` event already fired while the body was
      // running (for example one surfacing during an await, as in the
      // denoland/deno#34818 repro) has already failed this test through the
      // sink. This is best-effort and bounded to the body's lifetime: a
      // rejection that only surfaces after the body returns is treated as
      // post-test asynchronous activity and not attributed, matching the
      // limitation Node also has for activity that outlives a test. We avoid
      // draining extra event-loop turns here because doing so perturbs the
      // runner's output ordering and timing.
      sink.settled = true;
      return value;
    })();
    PromisePrototypeThen(
      body,
      (value) => result.resolve(value),
      (err) => result.reject(err),
    );
    PromisePrototypeThen(failure.promise, undefined, (err) => {
      result.reject(err);
    });
    return await result.promise;
  } finally {
    sink.settled = true;
    popTestSink(sink);
    if (timeoutId !== null) {
      realClearTimeout(timeoutId);
    }
    if (onAbort !== null && signal) {
      signal.removeEventListener("abort", onAbort);
    }
  }
}

async function runNodeTestFunction(fn, nodeTestContext) {
  if (fn.length >= 2) {
    // Node-style callback API: fn(t, done) - wait for `done()` (or promise
    // rejection) before treating the test as complete.
    await new Promise((testResolve, testReject) => {
      pendingCallbackReject = testReject;
      const done = (err) => {
        pendingCallbackReject = null;
        if (err) testReject(err);
        else testResolve(undefined);
      };
      try {
        const result = ReflectApply(fn, nodeTestContext, [
          nodeTestContext,
          done,
        ]);
        if (isThenable(result)) {
          PromisePrototypeThen(result, undefined, (err) => {
            pendingCallbackReject = null;
            testReject(err);
          });
        }
      } catch (err) {
        pendingCallbackReject = null;
        testReject(err);
      }
    });
    return undefined;
  }
  return await ReflectApply(fn, nodeTestContext, [nodeTestContext]);
}

async function runPossiblyExpectingFailure(fn, nodeTestContext, options) {
  const guards = {
    timeout: options.timeout,
    signal: options.signal,
    abort: (reason) => nodeTestContext._abort(reason),
  };
  // Install this context as the current one for the duration of the body so a
  // top-level `test()` called inside it is routed here as a subtest (see the
  // dispatcher in `test()`). `enterWith` is a synchronous setter that binds the
  // context for the current async root and its descendants without adding a
  // stack frame - extra frames can push `ext:cli/40_test.js` out of a failure's
  // captured stack and break the source location reported for failed tests. No
  // save/restore is needed: each body runs in its own async root, so concurrent
  // subtest bodies each keep their own context even when they interleave across
  // `await`.
  getTestContextALS().enterWith(nodeTestContext);
  if (
    !options.expectFailure ||
    options.skip ||
    options.todo
  ) {
    try {
      const result = await runWithTestGuards(
        () => runNodeTestFunction(fn, nodeTestContext),
        guards,
      );
      nodeTestContext._checkPlan();
      return result;
    } finally {
      // Drain even on failure so subtests registered before the error finish
      // their Deno test steps before this test settles.
      await nodeTestContext._drainSubtests();
    }
  }

  let failed = false;
  try {
    await runWithTestGuards(
      () => runNodeTestFunction(fn, nodeTestContext),
      guards,
    );
    nodeTestContext._checkPlan();
  } catch (err) {
    failed = true;
    assertExpectedFailure(err, options.expectFailure);
  } finally {
    await nodeTestContext._drainSubtests();
  }

  if (!failed) {
    throw new Error("test was expected to fail but passed");
  }
  return undefined;
}

const TIMEOUT_MAX = 2 ** 31 - 1;

class TestPlan {
  #expected;
  #actual = 0;

  constructor(count) {
    this.#expected = count;
  }

  increment() {
    this.#actual++;
  }

  check() {
    if (this.#actual !== this.#expected) {
      throw new Error(
        `plan expected ${this.#expected} assertion(s) but received ${this.#actual}`,
      );
    }
  }
}

// Parents waiting for a subtest (`_drainSubtests`): `{ depth, resolve }`.
const subtestWaiters = new SafeSet();

// Waits for a subtest's step to settle, or for `onEventLoopIdle` to give the
// wait up.
function awaitSubtestUnlessIdle(promise, depth) {
  const { promise: givenUp, resolve } = PromiseWithResolvers();
  const waiter = { depth, resolve };
  SetPrototypeAdd(subtestWaiters, waiter);
  return PromisePrototypeThen(
    SafePromiseRace([promise, givenUp]),
    () => SetPrototypeDelete(subtestWaiters, waiter),
    (err) => {
      SetPrototypeDelete(subtestWaiters, waiter);
      throw err;
    },
  );
}

// `deno test` calls this when a test's promise is still pending but the event
// loop has nothing left to run (`invoke_test_function` in
// cli/tools/test/mod.rs). Node cancels the tests still running when its loop
// empties ('beforeExit'); here the innermost parents stop waiting for their
// unfinished subtests, which Deno then reports as incomplete. Returns whether
// a wait was given up, so the runner polls the test again.
function onEventLoopIdle() {
  if (SetPrototypeGetSize(subtestWaiters) === 0) return false;
  let deepest = -1;
  for (const waiter of new SafeSetIterator(subtestWaiters)) {
    if (waiter.depth > deepest) deepest = waiter.depth;
  }
  for (const waiter of new SafeSetIterator(subtestWaiters)) {
    if (waiter.depth === deepest) {
      SetPrototypeDelete(subtestWaiters, waiter);
      waiter.resolve();
    }
  }
  return true;
}
internals.nodeTestOnEventLoopIdle = onEventLoopIdle;

class NodeTestContext {
  #denoContext;
  #afterHooks = [];
  #beforeHooks = [];
  #parent;
  #skipped = false;
  #name;
  #abortController = new AbortController();
  #plan;
  #assert;
  #passed = false;
  #beforeEachHooks = [];
  #afterEachHooks = [];
  // Guards so the once-per-test `before()`/`after()` hooks run a single time
  // regardless of how many subtests this context has.
  #beforeHooksRun = false;
  #afterHooksRun = false;
  #mock;
  // When set via `runOnly(true)`, only direct subtests registered with the
  // `only: true` option are executed; all other subtests are skipped.
  #runOnly = false;
  // Promises for top-level `test()` calls routed here from inside this
  // context's body (see `_trackSubtest`). The body awaits these before settling
  // so such subtests still complete (Node semantics) and their Deno test steps
  // finish before the parent step returns.
  #subtestPromises = [];
  // Canonical tags declared directly on this test/suite (see prepareTags).
  #ownTags = [];

  constructor(t, parent, name, tags) {
    this.#denoContext = t;
    this.#parent = parent;
    this.#name = name;
    if (tags !== undefined) {
      this.#ownTags = tags;
    }
  }

  // The effective tag set: the parent's tags followed by this context's own
  // tags, deduped (parent-first order). Tags are already canonical (lowercased
  // and deduped) individually.
  get tags() {
    const parentTags = this.#parent ? this.#parent.tags : [];
    if (this.#ownTags.length === 0) {
      // Return a fresh copy so callers cannot mutate internal state.
      return ArrayPrototypeSlice(parentTags);
    }
    const result = ArrayPrototypeSlice(parentTags);
    for (let i = 0; i < this.#ownTags.length; i++) {
      if (!ArrayPrototypeIncludes(result, this.#ownTags[i])) {
        ArrayPrototypePush(result, this.#ownTags[i]);
      }
    }
    return result;
  }

  get [skippedSymbol]() {
    return this.#skipped || (this.#parent?.[skippedSymbol] ?? false);
  }

  get assert() {
    this.#assert ??= createContextAssert(this, () => this.#plan);
    return this.#assert;
  }

  plan(count, options = kEmptyObject) {
    validateInteger(count, "count", 1);
    validateObject(options, "options");
    const { wait = false } = options;
    if (typeof wait !== "boolean" && typeof wait !== "number") {
      throw new ERR_INVALID_ARG_TYPE("options.wait", [
        "boolean",
        "number",
      ], wait);
    }
    this.#plan = new TestPlan(count);
  }

  // Polls `condition` until it returns or resolves, rejecting with its last
  // failure as the cause once `timeout` passes (Node's TestContext#waitFor).
  waitFor(condition, options = kEmptyObject) {
    validateFunction(condition, "condition");
    validateObject(options, "options");
    const { interval = 50, timeout = 1000 } = options;
    validateNumber(interval, "options.interval", 0, TIMEOUT_MAX);
    validateNumber(timeout, "options.timeout", 0, TIMEOUT_MAX);
    const { promise, resolve, reject } = PromiseWithResolvers();
    const noError = Symbol("noError");
    let cause = noError;
    let pollerId;
    const done = (err, result) => {
      clearTimeout(pollerId);
      clearTimeout(timeoutId);
      if (err === noError) {
        resolve(result);
      } else {
        reject(err);
      }
    };
    const timeoutId = setTimeout(() => {
      const err = new Error("waitFor() timed out");
      if (cause !== noError) {
        err.cause = cause;
      }
      done(err);
    }, timeout);
    const poller = async () => {
      try {
        const result = await condition();
        done(noError, result);
      } catch (err) {
        cause = err;
        pollerId = setTimeout(poller, interval);
      }
    };
    poller();
    return promise;
  }

  _checkPlan() {
    if (this.#plan) this.#plan.check();
  }

  get signal() {
    return this.#abortController.signal;
  }

  // Aborts this test's own AbortSignal (t.signal). Called by the test guards on
  // timeout or when a caller-supplied signal aborts, so user code awaiting
  // t.signal observes the abort.
  _abort(reason) {
    if (!this.#abortController.signal.aborted) {
      this.#abortController.abort(reason);
    }
  }

  get name() {
    return this.#name;
  }

  get fullName() {
    if (this.#parent) {
      return this.#parent.fullName + " > " + this.#name;
    }
    return this.#name;
  }

  // The test file being run (Node's `entryFile`), also for tests a module it
  // imports declares.
  get filePath() {
    return getEntryFilePath();
  }

  // Whether the test (or suite) has finished and passed; `false` while it runs.
  get passed() {
    return this.#passed;
  }

  _setPassed() {
    this.#passed = true;
  }

  // Node's `--test-rerun-failures` attempt number; Deno runs each test once.
  get attempt() {
    return 0;
  }

  diagnostic(message) {
    // deno-lint-ignore no-console
    console.log("DIAGNOSTIC:", message);
  }

  // Each test's own tracker (Node's `t.mock`): what it mocks is restored, and
  // the timers it enabled reset, when the test finishes.
  get mock() {
    this.#mock ??= new MockTracker();
    return this.#mock;
  }

  runOnly(value) {
    this.#runOnly = !!value;
    return null;
  }

  skip() {
    this.#skipped = true;
    return null;
  }

  todo() {
    this.#skipped = true;
    return null;
  }

  test(name, options, fn, overrides) {
    const prepared = prepareOptions(name, options, fn, overrides);
    if (this.#plan) this.#plan.increment();
    // deno-lint-ignore no-this-alias
    const parentContext = this;
    const stepPromise = PromisePrototypeThen(
      this.#denoContext.step({
        name: prepared.name,
        fn: async (denoTestContext) => {
          const newNodeTextContext = new NodeTestContext(
            denoTestContext,
            parentContext,
            prepared.name,
            prepared.options.tags,
          );
          let bodyOk = false;
          try {
            // The parent's `before()` hooks run once, before its first subtest.
            await parentContext._runBeforeHooksOnce();
            for (
              const hook of new SafeArrayIterator(
                parentContext.#beforeEachHooks,
              )
            ) {
              // `t.beforeEach()` hooks run in the parent test's context, so
              // `getTestContext()` inside them observes the parent (Node).
              await runInTestContext(
                parentContext,
                () => hook(newNodeTextContext),
              );
            }
            await runPossiblyExpectingFailure(
              prepared.fn,
              newNodeTextContext,
              prepared.options,
            );
            bodyOk = true;
            // This subtest's own `after()` hooks run once, after its body and
            // all of its own subtests have completed. The parent's `after()`
            // hooks run after the parent finishes, not here.
            await newNodeTextContext._runAfterHooksOnce();
            newNodeTextContext._setPassed();
          } catch (err) {
            if (!bodyOk) {
              try {
                await newNodeTextContext._runAfterHooksOnce();
              } catch { /* ignore, test is already failing */ }
            }
            if (!newNodeTextContext[skippedSymbol]) {
              throw err;
            }
          } finally {
            try {
              for (
                const hook of new SafeArrayIterator(
                  parentContext.#afterEachHooks,
                )
              ) {
                // `t.afterEach()` hooks likewise run in the parent's context.
                await runInTestContext(
                  parentContext,
                  () => hook(newNodeTextContext),
                );
              }
            } finally {
              // As Node, the subtest's `t.mock` is reset after its parent's
              // `afterEach()` hooks, which still see the subtest's mocks.
              newNodeTextContext._resetMock();
            }
          }
        },
        ignore: !!prepared.options.todo || !!prepared.options.skip ||
          (this.#runOnly && !prepared.options.only),
        sanitizeExit: false,
        sanitizeOps: false,
        sanitizeResources: false,
      }),
      () => undefined,
      // A failed step settles `false` rather than rejecting, but guard against
      // rejection so the returned promise never surfaces as unhandled.
      () => undefined,
    );
    // As Node, the parent waits for its subtests, awaited or not, before it
    // finishes (`subtestsPromise`).
    this._trackSubtest(stepPromise);
    return stepPromise;
  }

  // How deep this context is (a top-level test is 0).
  get _depth() {
    let depth = 0;
    for (let p = this.#parent; p; p = p.#parent) depth++;
    return depth;
  }

  // Records a subtest promise so the body runner can await it before this test
  // settles: every `t.test()` (an unawaited one too, as Node's parent waits
  // for its `subtestsPromise`) and every top-level `test()` routed here from
  // inside the body (see the dispatcher in `test()`).
  _trackSubtest(promise) {
    ArrayPrototypePush(this.#subtestPromises, promise);
  }

  // Awaits this test's subtests. Called by the body runner after the test
  // function returns, so that subtests the body didn't await (fastify's suites,
  // `t.test()` in a sync body) still run to completion before the parent test
  // settles. A wait the event loop can't finish (the subtest is held only by
  // an unref'd timer) is given up when the loop idles; see
  // `onEventLoopIdle`.
  async _drainSubtests() {
    while (this.#subtestPromises.length > 0) {
      const promises = ArrayPrototypeSplice(
        this.#subtestPromises,
        0,
        this.#subtestPromises.length,
      );
      for (const p of new SafeArrayIterator(promises)) {
        try {
          await awaitSubtestUnlessIdle(p, this._depth);
        } catch {
          /* failures already reported through the subtest's own step */
        }
      }
    }
  }

  before(fn, _options) {
    if (typeof fn !== "function") {
      throw new TypeError("before() requires a function");
    }
    ArrayPrototypePush(this.#beforeHooks, fn);
  }

  after(fn, _options) {
    if (typeof fn !== "function") {
      throw new TypeError("after() requires a function");
    }
    ArrayPrototypePush(this.#afterHooks, fn);
  }

  beforeEach(fn, _options) {
    if (typeof fn !== "function") {
      throw new TypeError("beforeEach() requires a function");
    }
    ArrayPrototypePush(this.#beforeEachHooks, fn);
  }

  afterEach(fn, _options) {
    if (typeof fn !== "function") {
      throw new TypeError("afterEach() requires a function");
    }
    ArrayPrototypePush(this.#afterEachHooks, fn);
  }

  // Runs this context's `before()` hooks a single time, before its first
  // subtest. Idempotent so it is safe to call from every subtest's step.
  async _runBeforeHooksOnce() {
    if (this.#beforeHooksRun) return;
    this.#beforeHooksRun = true;
    // deno-lint-ignore no-this-alias
    const ctx = this;
    for (const hook of new SafeArrayIterator(this.#beforeHooks)) {
      await runInTestContext(ctx, () => hook(ctx));
    }
  }

  // Runs this context's `after()` hooks a single time, after its body and all
  // of its subtests have completed. Idempotent so success and failure paths can
  // both invoke it without double-running.
  async _runAfterHooksOnce() {
    if (this.#afterHooksRun) return;
    this.#afterHooksRun = true;
    // deno-lint-ignore no-this-alias
    const ctx = this;
    for (const hook of new SafeArrayIterator(this.#afterHooks)) {
      await runInTestContext(ctx, () => hook(ctx));
    }
  }

  // Restores what this test mocked through `t.mock` (Node resets each test's
  // MockTracker when the test finishes).
  _resetMock() {
    this.#mock?.reset();
  }
}

let currentSuite = null;

// AsyncLocalStorage holding the NodeTestContext whose body is currently
// executing, or null when no test body is on the stack. Set by
// `runPossiblyExpectingFailure` for the duration of each body so that a
// top-level `test()` / `it()` call made from inside another test's body is
// registered as a subtest of that test (via `t.step()`), matching Node, rather
// than hitting Deno's "Nested Deno.test() calls are not supported" error.
//
// This must be an ALS, not a single module global with synchronous
// save/restore: the unawaited routed-subtest path this enables makes execution
// non-LIFO. With concurrent subtest bodies suspended across `await`, a plain
// variable can point at a sibling when a parent resumes and silently mis-nest
// the next routed `test()`. Each Deno test/step fn is its own async root, so
// the store stays scoped to that body's subtree and survives `await`
// boundaries, giving every interleaved body its own context. See
// https://github.com/denoland/deno/issues/35391.
let testContextALS = null;
function getTestContextALS() {
  if (testContextALS !== null) return testContextALS;
  const mod = core.loadExtScript("ext:deno_node/async_hooks.ts");
  const ALS = mod.AsyncLocalStorage;
  testContextALS = new ALS();
  return testContextALS;
}
function getCurrentTestContext() {
  if (testContextALS === null) return null;
  return testContextALS.getStore() ?? null;
}

// node:test `getTestContext()` (Node v26.1.0+). Returns the TestContext /
// SuiteContext of the test, subtest, or hook currently executing, or
// `undefined` when called outside of any test. It reads the same
// AsyncLocalStorage that routes nested `test()` calls (see getTestContextALS):
// every test/it body runs within it, and each hook is executed inside the ALS
// scope of the context it was registered on (see runInTestContext) so a hook
// observes its owning test/suite context rather than the subtest it runs for.
function getTestContext() {
  return getCurrentTestContext() ?? undefined;
}

// Runs `fn` with `ctx` installed as the current test context for the duration
// of the call, including across its `await` points, so that `getTestContext()`
// (and nested `test()` routing) observe `ctx` while a hook runs.
function runInTestContext(ctx, fn) {
  return getTestContextALS().run(ctx, fn);
}

const rootBeforeHooks = [];
const rootAfterHooks = [];
const rootBeforeEachHooks = [];
const rootAfterEachHooks = [];
let rootBeforeRan = false;

async function runRootBeforeOnce() {
  if (rootBeforeRan) return;
  rootBeforeRan = true;
  if (rootBeforeHooks.length === 0) return;
  const rootCtx = { name: "<root>", fullName: "<root>" };
  for (const hook of new SafeArrayIterator(rootBeforeHooks)) {
    await hook(rootCtx);
  }
}

async function runRootAfterIfDone() {
  if (activeNodeTests !== 0) return;
  if (rootAfterHooks.length === 0) return;
  const rootCtx = { name: "<root>", fullName: "<root>" };
  // Snapshot and clear so we only run once even if more tests get queued.
  const hooks = ArrayPrototypeSplice(
    rootAfterHooks,
    0,
    rootAfterHooks.length,
  );
  for (const hook of new SafeArrayIterator(hooks)) {
    try {
      await hook(rootCtx);
    } catch { /* ignore */ }
  }
}

class TestSuite {
  #denoTestContext;
  nodeTestContext;
  // The enclosing suite, or null for a top-level suite. Used to cascade
  // beforeEach()/afterEach() hooks from every ancestor suite onto each test
  // (issue #35404).
  parent = null;
  entries = [];
  beforeAllHooks = [];
  afterAllHooks = [];
  beforeEachHooks = [];
  afterEachHooks = [];

  constructor(t, nodeTestContext, parent) {
    this.#denoTestContext = t;
    this.nodeTestContext = nodeTestContext;
    this.parent = parent ?? null;
  }

  addTest(name, options, fn, overrides) {
    const prepared = prepareOptions(name, options, fn, overrides);
    const suiteNodeContext = this.nodeTestContext;
    // deno-lint-ignore no-this-alias
    const suite = this;
    ArrayPrototypePush(this.entries, {
      name: prepared.name,
      fn: async (denoTestContext) => {
        const newNodeTextContext = new NodeTestContext(
          denoTestContext,
          suiteNodeContext,
          prepared.name,
          prepared.options.tags,
        );
        // beforeEach()/afterEach() cascade through the whole ancestor chain:
        // each test runs every enclosing suite's hooks, plus the file-scope
        // (root) hooks. beforeEach runs outermost-first, afterEach runs
        // innermost-first, matching Node (issue #35404). The chain is collected
        // here, at run time, so hooks registered late in a suite body are seen.
        const suiteChain = []; // innermost-first
        for (let s = suite; s !== null; s = s.parent) {
          ArrayPrototypePush(suiteChain, s);
        }
        try {
          for (const hook of new SafeArrayIterator(rootBeforeEachHooks)) {
            await runInTestContext(
              newNodeTextContext,
              () => hook(newNodeTextContext),
            );
          }
          for (let i = suiteChain.length - 1; i >= 0; i--) {
            // A suite's beforeEach() runs in that suite's context, so
            // `getTestContext()` observes the suite, not the subtest (Node).
            const suiteCtx = suiteChain[i].nodeTestContext;
            for (
              const hook of new SafeArrayIterator(suiteChain[i].beforeEachHooks)
            ) {
              await runInTestContext(suiteCtx, () => hook(newNodeTextContext));
            }
          }
          const result = await runPossiblyExpectingFailure(
            prepared.fn,
            newNodeTextContext,
            prepared.options,
          );
          newNodeTextContext._setPassed();
          return result;
        } catch (err) {
          if (newNodeTextContext[skippedSymbol]) {
            return undefined;
          } else {
            throw err;
          }
        } finally {
          for (let i = 0; i < suiteChain.length; i++) {
            const suiteCtx = suiteChain[i].nodeTestContext;
            for (
              const hook of new SafeArrayIterator(suiteChain[i].afterEachHooks)
            ) {
              try {
                await runInTestContext(
                  suiteCtx,
                  () => hook(newNodeTextContext),
                );
              } catch { /* ignore */ }
            }
          }
          for (const hook of new SafeArrayIterator(rootAfterEachHooks)) {
            try {
              await runInTestContext(
                newNodeTextContext,
                () => hook(newNodeTextContext),
              );
            } catch { /* ignore */ }
          }
          newNodeTextContext._resetMock();
        }
      },
      ignore: !!prepared.options.todo || !!prepared.options.skip,
    });
  }

  addSuite(name, options, fn, overrides) {
    const prepared = prepareOptions(name, options, fn, overrides);
    const { promise, resolve } = PromiseWithResolvers();
    const parentSuiteContext = this.nodeTestContext;
    // deno-lint-ignore no-this-alias
    const parentSuite = this;
    ArrayPrototypePush(this.entries, {
      name: prepared.name,
      fn: wrapSuiteFn(
        prepared.fn,
        resolve,
        prepared.name,
        parentSuiteContext,
        parentSuite,
        prepared.options.tags,
      ),
      ignore: !!prepared.options.todo || !!prepared.options.skip,
    });
    return promise;
  }

  async execute() {
    for (const entry of new SafeArrayIterator(this.entries)) {
      await this.#denoTestContext.step({
        name: entry.name,
        fn: entry.fn,
        ignore: entry.ignore,
        sanitizeExit: false,
        sanitizeOps: false,
        sanitizeResources: false,
      });
    }
  }
}

// Node's experimental test `tags` feature. Tags are validated (array of
// non-empty strings), canonicalized (lowercased and deduped, preserving
// declaration order), and the experimental warning is emitted once the first
// time any tag is registered.
let tagsWarningEmitted = false;
function prepareTags(tags, argName) {
  if (!ArrayIsArray(tags)) {
    throw new ERR_INVALID_ARG_TYPE(argName, "string[]", tags);
  }
  const canonical = [];
  for (let i = 0; i < tags.length; i++) {
    const tag = tags[i];
    if (typeof tag !== "string") {
      throw new ERR_INVALID_ARG_TYPE(`${argName}[${i}]`, "string", tag);
    }
    if (tag === "") {
      throw new ERR_INVALID_ARG_VALUE(
        `${argName}[${i}]`,
        tag,
        "must not be an empty string",
      );
    }
    const lower = StringPrototypeToLowerCase(tag);
    if (!ArrayPrototypeIncludes(canonical, lower)) {
      ArrayPrototypePush(canonical, lower);
    }
  }
  if (canonical.length > 0 && !tagsWarningEmitted) {
    tagsWarningEmitted = true;
    emitExperimentalWarning("Test tags");
  }
  return canonical;
}

function prepareOptions(name, options, fn, overrides) {
  if (typeof name === "function") {
    fn = name;
  } else if (name !== null && typeof name === "object") {
    fn = options;
    options = name;
  } else if (typeof options === "function") {
    fn = options;
  }

  if (options === null || typeof options !== "object") {
    options = {};
  }

  const finalOptions = { ...options, ...overrides };

  if (finalOptions.tags !== undefined) {
    finalOptions.tags = prepareTags(finalOptions.tags, "options.tags");
  }

  if (typeof fn !== "function") {
    fn = noop;
  }

  if (typeof name !== "string" || name === "") {
    name = fn.name || "<anonymous>";
  }

  return { fn, options: finalOptions, name };
}

function wrapTestFn(fn, resolve, name, options) {
  return async function (t) {
    const nodeTestContext = new NodeTestContext(
      t,
      undefined,
      name,
      options.tags,
    );
    let beforeEachOk = false;
    try {
      await runRootBeforeOnce();
      for (const hook of new SafeArrayIterator(rootBeforeEachHooks)) {
        await runInTestContext(nodeTestContext, () => hook(nodeTestContext));
      }
      beforeEachOk = true;
      await runPossiblyExpectingFailure(fn, nodeTestContext, options);
      // The test's own `after()` hooks run once, after its body and all of its
      // subtests complete (a top-level test awaits its subtests inline).
      await nodeTestContext._runAfterHooksOnce();
      nodeTestContext._setPassed();
    } catch (err) {
      try {
        await nodeTestContext._runAfterHooksOnce();
      } catch { /* ignore, test is already failing */ }
      if (!nodeTestContext[skippedSymbol]) {
        throw sanitizeThrowValue(err);
      }
    } finally {
      if (beforeEachOk) {
        for (const hook of new SafeArrayIterator(rootAfterEachHooks)) {
          try {
            await runInTestContext(
              nodeTestContext,
              () => hook(nodeTestContext),
            );
          } catch { /* swallow to match node behavior on hook error */ }
        }
      }
      try {
        nodeTestContext._resetMock();
      } finally {
        activeNodeTests--;
        await runRootAfterIfDone();
        resolve();
      }
    }
  };
}

function prepareDenoTest(name, options, fn, overrides) {
  const prepared = prepareOptions(name, options, fn, overrides);

  activeNodeTests++;

  const denoTestOptions = {
    name: prepared.name,
    fn: wrapTestFn(prepared.fn, noop, prepared.name, prepared.options),
    only: prepared.options.only,
    ignore: !!prepared.options.todo || !!prepared.options.skip,
    sanitizeOnly: false,
    sanitizeExit: false,
    sanitizeOps: false,
    sanitizeResources: false,
  };
  Deno.test(denoTestOptions);
  // Node resolves the returned promise on test completion, but the
  // Deno runner only executes registered tests after the module
  // finishes evaluating, so top-level `await test(...)` deadlocks.
  // Resolve immediately to unblock; the test still runs and is
  // reported normally. Trade-off: code that awaits `test()` for
  // sequencing (`await test('a'); await test('b')`) sees them run
  // out of order.
  return PromiseResolve();
}

function wrapSuiteFn(fn, resolve, name, parentNodeContext, parentSuite, tags) {
  return async function (t) {
    const isTopLevel = parentNodeContext === undefined;
    if (isTopLevel) await runRootBeforeOnce();
    const suiteNodeContext = new NodeTestContext(
      t,
      parentNodeContext,
      name,
      tags,
    );
    const prevSuite = currentSuite;
    const suite = currentSuite = new TestSuite(
      t,
      suiteNodeContext,
      parentSuite,
    );
    try {
      // Run the suite body in the suite's context so `getTestContext()` inside
      // it (and any synchronously-registered hook bodies) observes the suite.
      runInTestContext(suiteNodeContext, () => fn(suiteNodeContext));
    } finally {
      currentSuite = prevSuite;
    }
    try {
      // Suite-level before()/after() hooks run in the suite's context and
      // receive it as their argument, matching Node.
      for (const hook of new SafeArrayIterator(suite.beforeAllHooks)) {
        await runInTestContext(suiteNodeContext, () => hook(suiteNodeContext));
      }
      await suite.execute();
    } finally {
      try {
        for (const hook of new SafeArrayIterator(suite.afterAllHooks)) {
          await runInTestContext(
            suiteNodeContext,
            () => hook(suiteNodeContext),
          );
        }
      } finally {
        if (isTopLevel) {
          activeNodeTests--;
          await runRootAfterIfDone();
        }
        resolve();
      }
    }
  };
}

function prepareDenoTestForSuite(name, options, fn, overrides) {
  const prepared = prepareOptions(name, options, fn, overrides);

  activeNodeTests++;

  const denoTestOptions = {
    name: prepared.name,
    fn: wrapSuiteFn(
      prepared.fn,
      noop,
      prepared.name,
      undefined,
      undefined,
      prepared.options.tags,
    ),
    only: prepared.options.only,
    ignore: !!prepared.options.todo || !!prepared.options.skip,
    sanitizeOnly: false,
    sanitizeExit: false,
    sanitizeOps: false,
    sanitizeResources: false,
  };
  Deno.test(denoTestOptions);
  // See `prepareDenoTest` for the Node-divergence trade-off; top-level
  // `await suite(...)` would deadlock if we waited for completion.
  return PromiseResolve();
}

function test(name, options, fn, overrides) {
  installErrorHandlers();
  if (isReporterMode()) {
    return queueTapTest(name, options, fn, overrides);
  }
  if (currentSuite) {
    return currentSuite.addTest(name, options, fn, overrides);
  }
  // A top-level `test()` called from inside another test's body becomes a
  // subtest of that test, matching Node. Without this it would fall through to
  // `Deno.test()`, which rejects nested registration.
  const currentContext = getCurrentTestContext();
  if (currentContext !== null) {
    // The context tracks it, so the running test waits for it before settling
    // even when the body doesn't await it (Node).
    return currentContext.test(name, options, fn, overrides);
  }
  return prepareDenoTest(name, options, fn, overrides);
}

test.skip = function skip(name, options, fn) {
  return test(name, options, fn, { skip: true });
};

test.todo = function todo(name, options, fn) {
  return test(name, options, fn, { todo: true });
};

test.only = function only(name, options, fn) {
  return test(name, options, fn, { only: true });
};

test.expectFailure = function expectFailure(name, options, fn) {
  return test(name, options, fn, { expectFailure: true });
};

function suite(name, options, fn, overrides) {
  installErrorHandlers();
  if (isReporterMode()) {
    return queueTapSuite(name, options, fn, overrides);
  }
  if (currentSuite) {
    return currentSuite.addSuite(name, options, fn, overrides);
  }
  return prepareDenoTestForSuite(name, options, fn, overrides);
}

suite.skip = function skip(name, options, fn) {
  return suite(name, options, fn, { skip: true });
};
suite.todo = function todo(name, options, fn) {
  return suite(name, options, fn, { todo: true });
};
suite.only = function only(name, options, fn) {
  return suite(name, options, fn, { only: true });
};

const it = test;
const describe = suite;

function before(fn, _options) {
  if (typeof fn !== "function") {
    throw new TypeError("before() requires a function argument");
  }
  if (isReporterMode()) {
    const tapSuite = getTapCurrentSuite();
    if (tapSuite !== null) {
      ArrayPrototypePush(tapSuite.beforeAllHooks, fn);
      return;
    }
    ArrayPrototypePush(rootBeforeHooks, fn);
    // A bare top-level `before()` with no tests must still produce TAP
    // output (`before` runs, then `TAP version 13`, then `1..0`).
    scheduleTapRun();
    return;
  }
  if (currentSuite) {
    ArrayPrototypePush(currentSuite.beforeAllHooks, fn);
    return;
  }
  ArrayPrototypePush(rootBeforeHooks, fn);
}

function after(fn, _options) {
  if (typeof fn !== "function") {
    throw new TypeError("after() requires a function argument");
  }
  if (isReporterMode()) {
    const tapSuite = getTapCurrentSuite();
    if (tapSuite !== null) {
      ArrayPrototypePush(tapSuite.afterAllHooks, fn);
      return;
    }
    ArrayPrototypePush(rootAfterHooks, fn);
    scheduleTapRun();
    return;
  }
  if (currentSuite) {
    ArrayPrototypePush(currentSuite.afterAllHooks, fn);
    return;
  }
  ArrayPrototypePush(rootAfterHooks, fn);
}

function beforeEach(fn, _options) {
  if (typeof fn !== "function") {
    throw new TypeError("beforeEach() requires a function argument");
  }
  if (isReporterMode()) {
    const runSuite = getTapCurrentSuite();
    if (runSuite !== null) {
      ArrayPrototypePush(runSuite.beforeEachHooks, fn);
      return;
    }
    ArrayPrototypePush(rootBeforeEachHooks, fn);
    return;
  }
  if (currentSuite) {
    ArrayPrototypePush(currentSuite.beforeEachHooks, fn);
    return;
  }
  ArrayPrototypePush(rootBeforeEachHooks, fn);
}

function afterEach(fn, _options) {
  if (typeof fn !== "function") {
    throw new TypeError("afterEach() requires a function argument");
  }
  if (isReporterMode()) {
    const runSuite = getTapCurrentSuite();
    if (runSuite !== null) {
      ArrayPrototypePush(runSuite.afterEachHooks, fn);
      return;
    }
    ArrayPrototypePush(rootAfterEachHooks, fn);
    return;
  }
  if (currentSuite) {
    ArrayPrototypePush(currentSuite.afterEachHooks, fn);
    return;
  }
  ArrayPrototypePush(rootAfterEachHooks, fn);
}

test.it = test;
test.describe = suite;
test.suite = suite;
test.before = before;
test.after = after;
test.beforeEach = beforeEach;
test.afterEach = afterEach;
test.getTestContext = getTestContext;

// `node:test` mocking, as Node's `lib/internal/test_runner/mock/mock.js`:
// a mock function is a Proxy of the function it spies on (so its name,
// length, prototype and `new.target` are the original's), and each
// MockTracker (the module's `mock`, and every test's own `t.mock`) keeps the
// mocks it created so that `reset()` / `restoreAll()` restore exactly those.

function kDefaultFunction() {}

class MockFunctionContext {
  #calls = [];
  #mocks = new SafeMap();
  #implementation;
  #restore;
  #times;

  constructor(implementation, restore, times) {
    this.#implementation = implementation;
    this.#restore = restore;
    this.#times = times;
  }

  get calls() {
    return ArrayPrototypeSlice(this.#calls, 0);
  }

  callCount() {
    return this.#calls.length;
  }

  mockImplementation(implementation) {
    validateFunction(implementation, "implementation");
    this.#implementation = implementation;
  }

  mockImplementationOnce(implementation, onCall) {
    validateFunction(implementation, "implementation");
    const nextCall = this.#calls.length;
    const call = onCall ?? nextCall;
    validateInteger(call, "onCall", nextCall);
    MapPrototypeSet(this.#mocks, call, implementation);
  }

  restore() {
    const { descriptor, object, original, methodName } = this.#restore;

    if (object !== undefined) {
      // An object method spy: put the original descriptor back. Node checks
      // `typeof methodName === "string"` here, which leaves a method mocked
      // under a symbol in place; any property key is restored.
      ObjectDefineProperty(object, methodName, {
        __proto__: null,
        ...descriptor,
      });
    } else {
      // A bare function spy: the mock calls the original function again.
      this.#implementation = original;
    }
  }

  resetCalls() {
    this.#calls = [];
  }

  trackCall(call) {
    ArrayPrototypePush(this.#calls, call);
  }

  nextImpl() {
    const nextCall = this.#calls.length;
    const mock = MapPrototypeGet(this.#mocks, nextCall);
    const impl = mock ?? this.#implementation;

    if (nextCall + 1 === this.#times) {
      this.restore();
    }

    MapPrototypeDelete(this.#mocks, nextCall);
    return impl;
  }
}

// Node keeps these off the public prototype.
const {
  nextImpl: mockNextImpl,
  restore: restoreMockFunction,
  trackCall: mockTrackCall,
} = MockFunctionContext.prototype;
delete MockFunctionContext.prototype.trackCall;
delete MockFunctionContext.prototype.nextImpl;

class MockPropertyContext {
  #object;
  #propertyName;
  #value;
  #originalValue;
  #descriptor;
  #accesses = [];
  #onceValues = new SafeMap();

  constructor(object, propertyName, value) {
    this.#object = object;
    this.#propertyName = propertyName;
    this.#originalValue = object[propertyName];
    this.#value = arguments.length > 2 ? value : this.#originalValue;
    this.#descriptor = ObjectGetOwnPropertyDescriptor(object, propertyName);
    if (!this.#descriptor) {
      throw new ERR_INVALID_ARG_VALUE(
        "propertyName",
        propertyName,
        "is not a property of the object",
      );
    }

    const { configurable, enumerable } = this.#descriptor;
    ObjectDefineProperty(object, propertyName, {
      __proto__: null,
      configurable,
      enumerable,
      get: () => {
        const nextValue = this.#getAccessValue(this.#value);
        ArrayPrototypePush(this.#accesses, {
          __proto__: null,
          type: "get",
          value: nextValue,
          stack: new Error(),
        });
        return nextValue;
      },
      set: (v) => this.mockImplementation(v),
    });
  }

  get accesses() {
    return ArrayPrototypeSlice(this.#accesses, 0);
  }

  accessCount() {
    return this.#accesses.length;
  }

  mockImplementation(value) {
    if (!this.#descriptor.writable) {
      throw new ERR_INVALID_ARG_VALUE(
        "propertyName",
        this.#propertyName,
        "cannot be set",
      );
    }
    const nextValue = this.#getAccessValue(value);
    ArrayPrototypePush(this.#accesses, {
      __proto__: null,
      type: "set",
      value: nextValue,
      stack: new Error(),
    });
    this.#value = nextValue;
  }

  #getAccessValue(value) {
    const accessIndex = this.#accesses.length;
    let accessValue;
    if (MapPrototypeHas(this.#onceValues, accessIndex)) {
      accessValue = MapPrototypeGet(this.#onceValues, accessIndex);
      MapPrototypeDelete(this.#onceValues, accessIndex);
    } else {
      accessValue = value;
    }
    return accessValue;
  }

  mockImplementationOnce(value, onAccess) {
    const nextAccess = this.#accesses.length;
    const accessIndex = onAccess ?? nextAccess;
    validateInteger(accessIndex, "onAccess", nextAccess);
    MapPrototypeSet(this.#onceValues, accessIndex, value);
  }

  resetAccesses() {
    this.#accesses = [];
  }

  restore() {
    // Reinstall the original descriptor. Unlike Node we don't force a `value`
    // field, since the original property may be an accessor (e.g.
    // `process.platform` is a getter in Deno) and mixing `value` with
    // `get`/`set` is an invalid descriptor.
    ObjectDefineProperty(this.#object, this.#propertyName, {
      __proto__: null,
      ...this.#descriptor,
    });
  }
}

const { restore: restoreMockProperty } = MockPropertyContext.prototype;

function validateStringOrSymbol(value, name) {
  if (typeof value !== "string" && typeof value !== "symbol") {
    throw new ERR_INVALID_ARG_TYPE(name, ["string", "symbol"], value);
  }
}

function validateTimes(value, name) {
  if (value === Infinity) {
    return;
  }
  validateInteger(value, name, 1);
}

function findMethodOnPrototypeChain(instance, methodName) {
  let host = instance;
  let descriptor;

  while (host !== null) {
    descriptor = ObjectGetOwnPropertyDescriptor(host, methodName);
    if (descriptor) {
      break;
    }
    host = ObjectGetPrototypeOf(host);
  }

  return descriptor;
}

const SUPPORTED_APIS = [
  "setTimeout",
  "setInterval",
  "setImmediate",
  "Date",
  "scheduler.wait",
  "AbortSignal.timeout",
];

class MockTimersHandle {
  #id;
  #timer;
  #timers;
  #refed;
  constructor(timer, timers, refed) {
    this.#id = timer.id;
    this.#timer = timer;
    this.#timers = timers;
    this.#refed = refed;
  }
  ref() {
    this.#refed = true;
    return this;
  }
  unref() {
    this.#refed = false;
    return this;
  }
  hasRef() {
    return this.#refed;
  }
  close() {
    this.#timers._clearTimer(this);
    return this;
  }
  [SymbolDispose]() {
    this.#timers._clearTimer(this);
  }
  refresh() {
    if (this.#timer && !this.#timer.interval) {
      this.#timer.fireAt = this.#timers._now + this.#timer.delay;
      MapPrototypeSet(this.#timers._timers, this.#id, this.#timer);
    }
    return this;
  }
  [SymbolToPrimitive]() {
    return this.#id;
  }
  [SymbolFor("Deno.customInspect")]() {
    return `MockTimer { id: ${this.#id} }`;
  }
  get _id() {
    return this.#id;
  }
}

// Installs/uninstalls this MockTimers on the `node:timers` module so that
// `require("node:timers")` and `import ... from "node:timers[/promises]"`
// callers route through the virtual clock too, not just `globalThis`.
const kInstallMockTimers = SymbolFor("Deno.internal.node.mockTimers");

class MockTimers {
  _enabled = false;
  _now = 0;
  _timers = new SafeMap();
  _nextId = 1;
  #originals = new SafeMap();
  #mockedApis = new SafeMap();
  // `AbortSignal.timeout` is a static method, not a `globalThis` binding, so it
  // is saved/restored separately from `#originals` (which maps global names).
  #abortSignalTimeoutOriginal = null;
  #abortSignalTimeoutMocked = false;

  // Own property descriptors of the `node:timers` / `node:timers/promises`
  // exports replaced while enabled, as [object, key, descriptor].
  #moduleOriginals = [];

  #mockGlobal(name, value) {
    if (!MapPrototypeHas(this.#originals, name)) {
      MapPrototypeSet(this.#originals, name, globalThis[name]);
    }
    globalThis[name] = value;
  }

  // Replaces an export of the `node:timers` modules, as Node's MockTimers
  // does, so `require("node:timers").setTimeout` is the mock while enabled and
  // the original (same descriptor) after `reset()`. The module functions also
  // consult the installed instance (`kInstallMockTimers`), which covers
  // bindings captured before `enable()`.
  #mockModuleExport(object, key, value) {
    ArrayPrototypePush(this.#moduleOriginals, [
      object,
      key,
      ObjectGetOwnPropertyDescriptor(object, key),
    ]);
    ObjectDefineProperty(object, key, {
      __proto__: null,
      value,
      writable: true,
      enumerable: true,
      configurable: true,
    });
  }

  // Whether `api` (e.g. `"setTimeout"`) is currently being intercepted. Read by
  // the `node:timers` module functions to decide per-call whether to use the
  // virtual clock, mirroring the per-api selection of `enable({ apis })`.
  _apiEnabled(api) {
    return MapPrototypeHas(this.#mockedApis, api);
  }

  enable(options = kEmptyObject) {
    if (this._enabled) {
      throw new ERR_INVALID_STATE(
        "MockTimers is already enabled. Reset it first to enable it again",
      );
    }
    validateObject(options, "options");
    let { apis, now } = options;
    if (apis === undefined) {
      apis = SUPPORTED_APIS;
    } else {
      validateStringArray(apis, "options.apis");
      for (let i = 0; i < apis.length; i++) {
        if (!ArrayPrototypeIncludes(SUPPORTED_APIS, apis[i])) {
          throw new ERR_INVALID_ARG_VALUE(
            `options.apis[${i}]`,
            apis[i],
            `must be one of ${ArrayPrototypeJoin(SUPPORTED_APIS, ", ")}`,
          );
        }
      }
    }
    if (now === undefined) {
      now = 0;
    } else if (ObjectPrototypeIsPrototypeOf(originalDatePrototype, now)) {
      now = DatePrototypeGetTime(now);
    }
    validateNumber(now, "options.now");
    if (now < 0 || !NumberIsFinite(now) || !NumberIsInteger(now)) {
      throw new ERR_INVALID_ARG_VALUE(
        "options.now",
        now,
        "must be a finite, non-negative integer or a Date object",
      );
    }

    this._enabled = true;
    this._now = now;

    for (let i = 0; i < apis.length; i++) {
      MapPrototypeSet(this.#mockedApis, apis[i], true);
    }

    for (let i = 0; i < apis.length; i++) {
      const api = apis[i];
      if (api === "Date") {
        this.#mockGlobal("Date", createMockDate(this));
      } else if (api === "setTimeout") {
        this.#mockGlobal(
          "setTimeout",
          (callback, delay, ...args) =>
            this._setTimeout(callback, delay, args, false),
        );
        this.#mockGlobal(
          "clearTimeout",
          (handle) => this._clearTimer(handle),
        );
      } else if (api === "setInterval") {
        this.#mockGlobal(
          "setInterval",
          (callback, delay, ...args) =>
            this._setInterval(callback, delay, args),
        );
        this.#mockGlobal(
          "clearInterval",
          (handle) => this._clearTimer(handle),
        );
      } else if (api === "setImmediate") {
        this.#mockGlobal(
          "setImmediate",
          (callback, ...args) => this._setTimeout(callback, 0, args, true),
        );
        this.#mockGlobal(
          "clearImmediate",
          (handle) => this._clearTimer(handle),
        );
      } else if (api === "AbortSignal.timeout") {
        this.#abortSignalTimeoutOriginal = globalThis.AbortSignal.timeout;
        this.#abortSignalTimeoutMocked = true;
        globalThis.AbortSignal.timeout = (delay) =>
          this.#mockedAbortSignalTimeout(delay);
      }
    }

    // Route the `node:timers` / `node:timers/promises` module functions through
    // this instance too (see `kInstallMockTimers` in `timers.ts`).
    const timersModule = core.loadExtScript("ext:deno_node/timers.ts");
    timersModule[kInstallMockTimers](this);
    const timersPromisesModule = core.loadExtScript(
      "ext:deno_node/timers/promises.ts",
    );
    const pairs = [
      ["setTimeout", "clearTimeout"],
      ["setInterval", "clearInterval"],
      ["setImmediate", "clearImmediate"],
    ];
    for (let i = 0; i < pairs.length; i++) {
      const { 0: set, 1: clear } = pairs[i];
      if (!this._apiEnabled(set)) continue;
      // The global mocks installed above.
      this.#mockModuleExport(timersModule, set, globalThis[set]);
      this.#mockModuleExport(timersModule, clear, globalThis[clear]);
      const realPromise = timersPromisesModule[set];
      this.#mockModuleExport(
        timersPromisesModule,
        set,
        function (...args) {
          return ReflectApply(realPromise, this, args);
        },
      );
    }
  }

  reset() {
    if (!this._enabled) return;
    core.loadExtScript("ext:deno_node/timers.ts")[kInstallMockTimers](null);
    while (this.#moduleOriginals.length > 0) {
      const { 0: object, 1: key, 2: descriptor } = ArrayPrototypePop(
        this.#moduleOriginals,
      );
      ObjectDefineProperty(object, key, { __proto__: null, ...descriptor });
    }
    for (
      const { 0: name, 1: original } of new SafeMapIterator(this.#originals)
    ) {
      globalThis[name] = original;
    }
    if (this.#abortSignalTimeoutMocked) {
      globalThis.AbortSignal.timeout = this.#abortSignalTimeoutOriginal;
      this.#abortSignalTimeoutOriginal = null;
      this.#abortSignalTimeoutMocked = false;
    }
    MapPrototypeClear(this.#originals);
    MapPrototypeClear(this.#mockedApis);
    MapPrototypeClear(this._timers);
    this._now = 0;
    this._nextId = 1;
    this._enabled = false;
  }

  #assertEnabled() {
    if (!this._enabled) {
      throw new ERR_INVALID_STATE(
        "You should enable MockTimers first by calling the .enable function",
      );
    }
  }

  #assertTimeArg(time) {
    if (time < 0) {
      throw new ERR_INVALID_ARG_VALUE(
        "time",
        time,
        "must be a non-negative integer",
      );
    }
  }

  // Mirrors Node's `MockTimers.tick`: advance the clock to each due timer's
  // scheduled time as it fires (not straight to the end of the window), so a
  // callback reading `Date.now()` sees the time the timer was scheduled for.
  // Intervals re-arm and may fire again within the same tick.
  tick(milliseconds = 1) {
    this.#assertEnabled();
    this.#assertTimeArg(milliseconds);
    const target = this._now + milliseconds;
    while (true) {
      const next = this.#findNextTimer();
      if (next === null || next.fireAt > target) break;
      this._now = next.fireAt;
      this.#fireTimer(next);
    }
    this._now = target;
  }

  // Mirrors Node's `MockTimers.runAll`: tick up to the longest pending timer.
  // Intervals fire as many times as fit within that window, then re-arm past
  // `_now` so the loop terminates.
  runAll() {
    this.#assertEnabled();
    const longest = this.#findLongestTimer();
    if (longest === null) return;
    this.tick(longest.fireAt - this._now);
  }

  setTime(milliseconds) {
    validateNumber(milliseconds, "time");
    this.#assertTimeArg(milliseconds);
    this.#assertEnabled();
    this._now = milliseconds;
  }

  [SymbolDispose]() {
    this.reset();
  }

  // Mirrors Node's mocked `AbortSignal.timeout`: the returned signal aborts
  // with a `TimeoutError` once the virtual clock advances past `delay`, instead
  // of using real time, so `tick()` / `runAll()` drive the timeout.
  #mockedAbortSignalTimeout(delay) {
    const controller = new AbortController();
    this._setTimeout(
      () => {
        controller.abort(
          new DOMException("The operation timed out.", "TimeoutError"),
        );
      },
      delay,
      [],
      false,
    );
    return controller.signal;
  }

  // Like Node's mocked timers, these take the callback as given: the
  // `node:timers` functions validate it before they get here.
  _setTimeout(callback, delay, args, immediate) {
    if (delay === undefined || delay === null) delay = 1;
    if (typeof delay !== "number") delay = +delay;
    if (!NumberIsFinite(delay) || delay < 0) delay = 1;
    if (delay > 2147483647) delay = 1;
    const id = this._nextId++;
    const timer = {
      id,
      callback,
      args,
      delay,
      fireAt: this._now + delay,
      interval: null,
      immediate,
    };
    MapPrototypeSet(this._timers, id, timer);
    return new MockTimersHandle(timer, this, true);
  }

  _setInterval(callback, delay, args) {
    if (delay === undefined || delay === null) delay = 1;
    if (typeof delay !== "number") delay = +delay;
    if (!NumberIsFinite(delay) || delay < 1) delay = 1;
    if (delay > 2147483647) delay = 1;
    const id = this._nextId++;
    const timer = {
      id,
      callback,
      args,
      delay,
      fireAt: this._now + delay,
      interval: delay,
      immediate: false,
    };
    MapPrototypeSet(this._timers, id, timer);
    return new MockTimersHandle(timer, this, true);
  }

  _clearTimer(handle) {
    if (handle === null || handle === undefined) return;
    let id;
    if (typeof handle === "number") {
      id = handle;
    } else if (typeof handle === "object" && typeof handle._id === "number") {
      id = handle._id;
    } else {
      return;
    }
    MapPrototypeDelete(this._timers, id);
  }

  #findNextTimer() {
    let next = null;
    for (const { 1: t } of new SafeMapIterator(this._timers)) {
      if (
        next === null ||
        t.fireAt < next.fireAt ||
        (t.fireAt === next.fireAt &&
          (t.immediate !== next.immediate ? t.immediate : t.id < next.id))
      ) {
        next = t;
      }
    }
    return next;
  }

  #findLongestTimer() {
    let longest = null;
    for (const { 1: t } of new SafeMapIterator(this._timers)) {
      if (longest === null || t.fireAt > longest.fireAt) {
        longest = t;
      }
    }
    return longest;
  }

  #fireTimer(timer) {
    // Match Node: invoke the callback first (errors propagate synchronously out
    // of `tick`), then re-arm intervals or drop one-shot timers. A timer that
    // clears itself inside its own callback is already gone from `_timers`, so
    // the bookkeeping below is a no-op for it.
    ReflectApply(timer.callback, undefined, timer.args);
    if (timer.interval !== null) {
      timer.fireAt += timer.interval;
    } else {
      MapPrototypeDelete(this._timers, timer.id);
    }
  }
}

const originalDate = globalThis.Date;
const originalDatePrototype = originalDate.prototype;

function createMockDate(mockTimers) {
  function MockDate(...args) {
    if (!new.target) {
      return DatePrototypeToString(
        ReflectConstruct(originalDate, [mockTimers._now], MockDate),
      );
    }
    if (args.length === 0) {
      return ReflectConstruct(originalDate, [mockTimers._now], MockDate);
    }
    return ReflectConstruct(originalDate, args, MockDate);
  }
  ObjectDefineProperty(MockDate, "prototype", {
    __proto__: null,
    value: originalDate.prototype,
    writable: false,
  });
  ObjectDefineProperty(MockDate, "name", {
    __proto__: null,
    value: "Date",
    configurable: true,
  });
  MockDate.now = () => mockTimers._now;
  MockDate.parse = originalDate.parse;
  MockDate.UTC = originalDate.UTC;
  MockDate.isMock = true;
  MockDate.toString = () => "function Date() { [native code] }";
  return MockDate;
}

const mockTimers = new MockTimers();

// `mock.module()` support.
//
// Module mocking is built on top of `module.registerHooks()` (the Node module
// customization hooks). A single registration intercepts both `import()` and
// `require()`. The whole subsystem is lazily initialized on the first
// `mock.module()` call so that importing `node:test` without mocking modules
// pays no startup cost and registers no hooks.

// Symbol under which the live mock registry is published on `globalThis`. The
// synthetic module source generated for a mocked specifier reads the live
// export values back out of this registry at evaluation time, so functions and
// objects passed to `mock.module()` cross into the mocked module unchanged.
const kMockModuleRegistryName = "deno.internal.nodeTestMockModules";
// Search param appended to a mocked ESM url to bust the module cache when
// `cache` is false. ESM modules cannot be evicted from the loader cache, so a
// fresh url (new version) is resolved on every import instead.
const kMockSearchParam = "node-test-mock";
const kBadExportsMessage = "Cannot create mock because named exports cannot " +
  "be applied to the provided default export.";

let mockModuleRegistry = null;
let mockModuleHooksHandle = null;
let nodeModuleNamespace = null;
// Monotonic counter used to version mocked ESM urls. A global (rather than
// per-mock) counter guarantees that a freshly created mock never collides with
// a versioned url that a previous mock already cached.
let mockModuleVersion = 0;

function getNodeModuleNamespace() {
  if (nodeModuleNamespace === null) {
    nodeModuleNamespace = core.createLazyLoader("node:module")();
  }
  return nodeModuleNamespace;
}

function isUrlLike(value) {
  return value !== null && typeof value === "object" &&
    typeof value.href === "string" && typeof value.protocol === "string";
}

function ensureMockModuleHooks() {
  if (mockModuleHooksHandle !== null) {
    return;
  }
  mockModuleRegistry = new SafeMap();
  globalThis[SymbolFor(kMockModuleRegistryName)] = mockModuleRegistry;
  const { registerHooks } = getNodeModuleNamespace();
  mockModuleHooksHandle = registerHooks({
    __proto__: null,
    resolve: mockModuleResolveHook,
    load: mockModuleLoadHook,
  });
}

// Best effort discovery of the file that called `mock.module()`, used to
// resolve relative specifiers the same way Node does (eagerly, against the
// caller). Returns undefined when no user frame can be found, in which case
// the default resolver falls back to the current working directory.
const kStackFrameUrl = new SafeRegExp(
  "((?:file|https?):\\/\\/[^\\s)]+?):\\d+:\\d+",
);
function getCallerUrl() {
  const stack = new Error().stack;
  if (typeof stack !== "string") {
    return undefined;
  }
  const lines = StringPrototypeSplit(stack, "\n");
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (
      StringPrototypeIncludes(line, "ext:deno_node/") ||
      StringPrototypeIncludes(line, "ext:core/")
    ) {
      continue;
    }
    const match = RegExpPrototypeExec(kStackFrameUrl, line);
    if (match !== null) {
      const candidate = match[1];
      if (
        StringPrototypeStartsWith(candidate, "file:") ||
        StringPrototypeStartsWith(candidate, "http")
      ) {
        return candidate;
      }
    }
  }
  return undefined;
}

// The path of the test file being run, as Node's `entryFile`: the main
// module, not the module that declared the test.
function getEntryFilePath() {
  const main = globalThis.Deno?.mainModule;
  if (typeof main === "string" && StringPrototypeStartsWith(main, "file:")) {
    return getNodeUrl().fileURLToPath(main);
  }
  return undefined;
}

let nodeUrlNamespace = null;
function getNodeUrl() {
  if (nodeUrlNamespace === null) {
    nodeUrlNamespace = core.createLazyLoader("node:url")();
  }
  return nodeUrlNamespace;
}

// Normalizes a builtin specifier so that both `readline` and `node:readline`
// map to the same registry key.
function normalizeNodeScheme(specifier) {
  if (StringPrototypeStartsWith(specifier, "node:")) {
    return specifier;
  }
  const { isBuiltin } = getNodeModuleNamespace();
  if (isBuiltin(specifier)) {
    return "node:" + specifier;
  }
  return specifier;
}

// Canonicalizes a resolved url into the registry key. Builtins are normalized
// to the `node:` scheme; file urls are resolved through the real path so that
// the key registered by `mock.module()` matches the (realpath based) url the
// require/import hooks observe, regardless of symlinks such as macOS
// `/tmp` to `/private/tmp`.
function canonicalizeUrlKey(u) {
  if (StringPrototypeStartsWith(u, "file:")) {
    try {
      const path = getNodeUrl().fileURLToPath(u);
      const real = core.ops.op_require_real_path(path);
      return getNodeUrl().pathToFileURL(real).href;
    } catch {
      return u;
    }
  }
  return normalizeNodeScheme(u);
}

// Resolves a `mock.module()` specifier to the canonical registry key (the
// resolved module url, with builtins normalized to the `node:` scheme).
function resolveSpecifierToKey(specifier) {
  if (StringPrototypeStartsWith(specifier, "node:")) {
    return specifier;
  }
  const parentUrl = getCallerUrl();
  let resolved;
  try {
    resolved = core.ops.op_module_default_resolve(specifier, parentUrl);
  } catch {
    resolved = specifier;
  }
  return canonicalizeUrlKey(resolved);
}

// Strips the cache busting search param so a versioned ESM url maps back to its
// registry key.
function stripMockParam(u) {
  const idx = StringPrototypeIndexOf(u, kMockSearchParam);
  if (idx === -1) {
    return u;
  }
  try {
    const parsed = new URL(u);
    parsed.searchParams.delete(kMockSearchParam);
    let href = parsed.href;
    if (StringPrototypeEndsWith(href, "?")) {
      href = StringPrototypeSlice(href, 0, href.length - 1);
    }
    return href;
  } catch {
    return u;
  }
}

function appendMockParam(u, version) {
  const sep = StringPrototypeIncludes(u, "?") ? "&" : "?";
  return u + sep + kMockSearchParam + "=" + version;
}

// Looks up the active mock entry (if any) for a url seen by a hook.
function lookupMockEntry(u) {
  if (mockModuleRegistry === null) {
    return undefined;
  }
  let entry = MapPrototypeGet(mockModuleRegistry, u);
  if (entry === undefined && StringPrototypeIncludes(u, kMockSearchParam)) {
    entry = MapPrototypeGet(mockModuleRegistry, stripMockParam(u));
  }
  if (entry === undefined) {
    return undefined;
  }
  return entry.active ? entry : undefined;
}

function detectFormat(key) {
  if (StringPrototypeStartsWith(key, "node:")) {
    return "commonjs";
  }
  if (
    StringPrototypeEndsWith(key, ".mjs") || StringPrototypeEndsWith(key, ".mts")
  ) {
    return "module";
  }
  if (
    StringPrototypeEndsWith(key, ".cjs") || StringPrototypeEndsWith(key, ".cts")
  ) {
    return "commonjs";
  }
  if (StringPrototypeStartsWith(key, "file:")) {
    try {
      const path = getNodeUrl().fileURLToPath(key);
      return core.ops.op_require_is_maybe_cjs(path) ? "commonjs" : "module";
    } catch {
      return "commonjs";
    }
  }
  // Remote modules (http/https) are only loadable as ESM.
  if (
    StringPrototypeStartsWith(key, "http:") ||
    StringPrototypeStartsWith(key, "https:")
  ) {
    return "module";
  }
  return "commonjs";
}

function registryAccessSource() {
  return "globalThis[Symbol.for(" + JSONStringify(kMockModuleRegistryName) +
    ")]";
}

function generateCjsSource(entry, key) {
  let src = '"use strict";\n';
  src += "const $e = " + registryAccessSource() + ".get(" +
    JSONStringify(key) + ");\n";
  src += "if ($e === undefined) { throw new Error(" +
    JSONStringify('mock exports not found for "' + key + '"') + "); }\n";
  if (entry.hasDefaultExport) {
    src += "module.exports = $e.moduleExports.default;\n";
  }
  if (entry.exportNames.length > 0) {
    src += "if (module.exports === null || typeof module.exports !== " +
      '"object") { throw new Error(' + JSONStringify(kBadExportsMessage) +
      "); }\n";
    for (let i = 0; i < entry.exportNames.length; i++) {
      const name = entry.exportNames[i];
      src += "module.exports[" + JSONStringify(name) + "] = " +
        "$e.moduleExports[" + JSONStringify(name) + "];\n";
    }
  }
  return src;
}

function generateEsmSource(entry, key) {
  let src = "const $e = " + registryAccessSource() + ".get(" +
    JSONStringify(key) + ");\n";
  src += "if ($e === undefined) { throw new Error(" +
    JSONStringify('mock exports not found for "' + key + '"') + "); }\n";
  if (entry.isCjs) {
    // For a CommonJS module the named exports are applied onto the default
    // export object, mirroring how Deno exposes a required CJS module to an
    // ESM importer (default plus the merged keys).
    src += "let $d = $e.hasDefaultExport ? $e.moduleExports.default : {};\n";
    if (entry.exportNames.length > 0) {
      src += 'if ($d === null || typeof $d !== "object") { throw new Error(' +
        JSONStringify(kBadExportsMessage) + "); }\n";
      for (let i = 0; i < entry.exportNames.length; i++) {
        const name = entry.exportNames[i];
        src += "$d[" + JSONStringify(name) + "] = $e.moduleExports[" +
          JSONStringify(name) + "];\n";
      }
    }
    src += "export default $d;\n";
    for (let i = 0; i < entry.exportNames.length; i++) {
      const name = entry.exportNames[i];
      // Use a safe local identifier plus a string export name so export names
      // that are not valid identifiers (e.g. "foo-bar") still generate valid
      // source.
      src += "let $n" + i + " = $d[" + JSONStringify(name) + "];\n";
      src += "export { $n" + i + " as " + JSONStringify(name) + " };\n";
    }
  } else {
    // For an ESM module the default export and named exports are independent.
    if (entry.hasDefaultExport) {
      src += "export default $e.moduleExports.default;\n";
    }
    for (let i = 0; i < entry.exportNames.length; i++) {
      const name = entry.exportNames[i];
      src += "let $n" + i + " = $e.moduleExports[" + JSONStringify(name) +
        "];\n";
      src += "export { $n" + i + " as " + JSONStringify(name) + " };\n";
    }
  }
  return src;
}

// The require cache (Module._cache) key for a mocked module: builtins are keyed
// by their `node:` url, file modules by their (real) path.
function cjsCacheKeyFor(key) {
  if (StringPrototypeStartsWith(key, "file:")) {
    try {
      return getNodeUrl().fileURLToPath(key);
    } catch {
      return key;
    }
  }
  return key;
}

// Removes a mocked CommonJS module from the require cache so the next
// `require()` re-runs the load hook and produces a fresh exports object.
function evictCjsCache(key) {
  const { default: Module } = getNodeModuleNamespace();
  delete Module._cache[cjsCacheKeyFor(key)];
}

function mockModuleResolveHook(specifier, context, nextResolve) {
  const result = nextResolve(specifier, context);
  if (result === null || result === undefined || result.url == null) {
    return result;
  }
  const conditions = context?.conditions;
  // Only the ESM import path needs per-import versioned urls. The require path
  // busts its cache by evicting the require cache entry in the load hook.
  if (
    conditions === undefined ||
    !ArrayPrototypeIncludes(conditions, "import")
  ) {
    return result;
  }
  const entry = lookupMockEntry(canonicalizeUrlKey(result.url));
  if (entry === undefined) {
    return result;
  }
  // A stable version (cache: true) reuses the cached module; otherwise bump the
  // global counter so every import resolves to a brand new url.
  const version = entry.cache ? entry.stableVersion : ++mockModuleVersion;
  return {
    __proto__: null,
    url: appendMockParam(result.url, version),
    shortCircuit: true,
  };
}

function mockModuleLoadHook(url, context, nextLoad) {
  const key = canonicalizeUrlKey(stripMockParam(url));
  const entry = lookupMockEntry(key);
  if (entry === undefined) {
    return nextLoad(url, context);
  }
  const conditions = context?.conditions;
  const viaRequire = conditions !== undefined &&
    ArrayPrototypeIncludes(conditions, "require");
  if (viaRequire) {
    if (!entry.cache) {
      evictCjsCache(key);
    }
    return {
      __proto__: null,
      source: generateCjsSource(entry, key),
      format: "commonjs",
      shortCircuit: true,
    };
  }
  return {
    __proto__: null,
    source: generateEsmSource(entry, key),
    format: "module",
    shortCircuit: true,
  };
}

class MockModuleContext {
  #restore;

  constructor(restore) {
    this.#restore = restore;
  }

  restore() {
    if (this.#restore !== undefined) {
      this.#restore();
      this.#restore = undefined;
    }
  }
}

const { restore: restoreMockModule } = MockModuleContext.prototype;

function mockModule(specifier, options = kEmptyObject) {
  let specStr;
  if (typeof specifier === "string") {
    specStr = specifier;
  } else if (isUrlLike(specifier)) {
    specStr = `${specifier}`;
  } else {
    throw new ERR_INVALID_ARG_TYPE(
      "specifier",
      ["string", "URL"],
      specifier,
    );
  }

  validateObject(options, "options");
  const cache = options.cache === undefined ? false : options.cache;
  validateBoolean(cache, "options.cache");

  // `exports` is a legacy alias that bundles `defaultExport` and
  // `namedExports` into a single object: its `default` property (if any)
  // becomes the default export and its remaining own enumerable keys become
  // named exports. It cannot be combined with either of the newer options.
  const exportsOption = options.exports;
  const namedExports = options.namedExports;
  let exportSource = namedExports;
  let hasDefaultExport = ObjectPrototypeHasOwnProperty(
    options,
    "defaultExport",
  );
  let defaultExportValue = options.defaultExport;
  if (exportsOption !== undefined) {
    if (namedExports !== undefined || options.defaultExport !== undefined) {
      throw new ERR_INVALID_ARG_VALUE(
        "options.exports",
        exportsOption,
        'cannot be used with "namedExports" or "defaultExport"',
      );
    }
    validateObject(exportsOption, "options.exports");
    exportSource = exportsOption;
    if (ObjectPrototypeHasOwnProperty(exportsOption, "default")) {
      hasDefaultExport = true;
      defaultExportValue = exportsOption.default;
    }
  } else if (namedExports !== undefined) {
    validateObject(namedExports, "options.namedExports");
  }

  ensureMockModuleHooks();

  const moduleExports = { __proto__: null };
  const exportNames = [];
  if (exportSource !== undefined && exportSource !== null) {
    const keys = ObjectKeys(exportSource);
    for (let i = 0; i < keys.length; i++) {
      const name = keys[i];
      // For the `exports` form the `default` key is the default export, not a
      // named export.
      if (exportsOption !== undefined && name === "default") {
        continue;
      }
      moduleExports[name] = exportSource[name];
      ArrayPrototypePush(exportNames, name);
    }
  }
  if (hasDefaultExport) {
    moduleExports.default = defaultExportValue;
  }

  const key = resolveSpecifierToKey(specStr);
  const existing = MapPrototypeGet(mockModuleRegistry, key);
  if (existing !== undefined && existing.active) {
    throw new ERR_INVALID_STATE(
      `Cannot mock '${specStr}'. The module is already mocked.`,
    );
  }

  const format = detectFormat(key);
  const entry = {
    __proto__: null,
    url: key,
    isCjs: format === "commonjs",
    cache,
    stableVersion: cache ? ++mockModuleVersion : 0,
    active: true,
    moduleExports,
    exportNames,
    hasDefaultExport,
  };
  MapPrototypeSet(mockModuleRegistry, key, entry);
  // Save and evict any previously loaded copy of this module so the next
  // require/import re-runs the hooks and observes the mock. The saved entry is
  // put back on restore so the exact original instance is returned again.
  const { default: Module } = getNodeModuleNamespace();
  const cacheKey = cjsCacheKeyFor(key);
  entry.savedCacheEntry = Module._cache[cacheKey];
  delete Module._cache[cacheKey];

  const restore = () => {
    if (!entry.active) {
      return;
    }
    entry.active = false;
    if (MapPrototypeGet(mockModuleRegistry, key) === entry) {
      MapPrototypeDelete(mockModuleRegistry, key);
    }
    delete Module._cache[cacheKey];
    if (entry.savedCacheEntry !== undefined) {
      Module._cache[cacheKey] = entry.savedCacheEntry;
    }
  };

  return new MockModuleContext(restore);
}

// A tracker's `timers`. There is one virtual clock (`mockTimers`, which the
// `node:timers` module functions consult), so each tracker's `timers` drives
// that clock and remembers whether it enabled it: `reset()` on a tracker
// (as at the end of a test) resets the clock only if that tracker enabled it.
class MockTrackerTimers {
  _owns = false;

  enable(options) {
    mockTimers.enable(options);
    this._owns = true;
  }

  reset() {
    mockTimers.reset();
    this._owns = false;
  }

  tick(milliseconds) {
    mockTimers.tick(milliseconds);
  }

  runAll() {
    mockTimers.runAll();
  }

  setTime(milliseconds) {
    // deno-lint-ignore deno-internal/prefer-primordials
    mockTimers.setTime(milliseconds);
  }

  [SymbolDispose]() {
    this.reset();
  }
}

class MockTracker {
  #mocks = [];
  #timers;

  get timers() {
    this.#timers ??= new MockTrackerTimers();
    return this.#timers;
  }

  fn(
    original = function () {},
    implementation = original,
    options = kEmptyObject,
  ) {
    if (original !== null && typeof original === "object") {
      options = original;
      original = function () {};
      implementation = original;
    } else if (implementation !== null && typeof implementation === "object") {
      options = implementation;
      implementation = original;
    }

    validateFunction(original, "original");
    validateFunction(implementation, "implementation");
    validateObject(options, "options");
    const { times = Infinity } = options;
    validateTimes(times, "options.times");
    const ctx = new MockFunctionContext(
      implementation,
      { __proto__: null, original },
      times,
    );
    return this.#setupMock(ctx, original);
  }

  method(
    objectOrFunction,
    methodName,
    implementation = kDefaultFunction,
    options = kEmptyObject,
  ) {
    validateStringOrSymbol(methodName, "methodName");
    if (typeof objectOrFunction !== "function") {
      validateObject(objectOrFunction, "object");
    }

    if (implementation !== null && typeof implementation === "object") {
      options = implementation;
      implementation = kDefaultFunction;
    }

    validateFunction(implementation, "implementation");
    validateObject(options, "options");

    const {
      getter = false,
      setter = false,
      times = Infinity,
    } = options;

    validateBoolean(getter, "options.getter");
    validateBoolean(setter, "options.setter");
    validateTimes(times, "options.times");

    if (setter && getter) {
      throw new ERR_INVALID_ARG_VALUE(
        "options.setter",
        setter,
        "cannot be used with 'options.getter'",
      );
    }
    const descriptor = findMethodOnPrototypeChain(objectOrFunction, methodName);

    let original;

    if (getter) {
      original = descriptor?.get;
    } else if (setter) {
      original = descriptor?.set;
    } else {
      original = descriptor?.value;
    }

    if (typeof original !== "function") {
      throw new ERR_INVALID_ARG_VALUE(
        "methodName",
        original,
        "must be a method",
      );
    }

    const restore = {
      __proto__: null,
      descriptor,
      object: objectOrFunction,
      methodName,
    };
    const impl = implementation === kDefaultFunction
      ? original
      : implementation;
    const ctx = new MockFunctionContext(impl, restore, times);
    const mock = this.#setupMock(ctx, original);
    const mockDescriptor = {
      __proto__: null,
      configurable: descriptor.configurable,
      enumerable: descriptor.enumerable,
    };

    if (getter) {
      mockDescriptor.get = mock;
      mockDescriptor.set = descriptor.set;
    } else if (setter) {
      mockDescriptor.get = descriptor.get;
      mockDescriptor.set = mock;
    } else {
      mockDescriptor.writable = descriptor.writable;
      mockDescriptor.value = mock;
    }

    ObjectDefineProperty(objectOrFunction, methodName, mockDescriptor);

    return mock;
  }

  getter(
    object,
    methodName,
    implementation = kDefaultFunction,
    options = kEmptyObject,
  ) {
    if (implementation !== null && typeof implementation === "object") {
      options = implementation;
      implementation = kDefaultFunction;
    } else {
      validateObject(options, "options");
    }

    const { getter = true } = options;

    if (getter === false) {
      throw new ERR_INVALID_ARG_VALUE(
        "options.getter",
        getter,
        "cannot be false",
      );
    }

    return this.method(object, methodName, implementation, {
      __proto__: null,
      ...options,
      getter,
    });
  }

  setter(
    object,
    methodName,
    implementation = kDefaultFunction,
    options = kEmptyObject,
  ) {
    if (implementation !== null && typeof implementation === "object") {
      options = implementation;
      implementation = kDefaultFunction;
    } else {
      validateObject(options, "options");
    }

    const { setter = true } = options;

    if (setter === false) {
      throw new ERR_INVALID_ARG_VALUE(
        "options.setter",
        setter,
        "cannot be false",
      );
    }

    return this.method(object, methodName, implementation, {
      __proto__: null,
      ...options,
      setter,
    });
  }

  module(specifier, options) {
    const ctx = mockModule(specifier, options);
    ArrayPrototypePush(this.#mocks, {
      __proto__: null,
      ctx,
      restore: restoreMockModule,
    });
    return ctx;
  }

  property(object, propertyName, value) {
    validateObject(object, "object");
    validateStringOrSymbol(propertyName, "propertyName");

    const ctx = arguments.length > 2
      ? new MockPropertyContext(object, propertyName, value)
      : new MockPropertyContext(object, propertyName);
    ArrayPrototypePush(this.#mocks, {
      __proto__: null,
      ctx,
      restore: restoreMockProperty,
    });

    return new Proxy(object, {
      __proto__: null,
      get(target, property, receiver) {
        if (property === "mock") {
          return ctx;
        }
        return ReflectGet(target, property, receiver);
      },
    });
  }

  // Restores every mock this tracker created and forgets them; the mocks'
  // recorded calls are kept. Resets the mock timers if this tracker enabled
  // them.
  reset() {
    this.restoreAll();
    if (this.#timers?._owns) {
      this.#timers.reset();
    }
    this.#mocks = [];
  }

  restoreAll() {
    for (let i = 0; i < this.#mocks.length; i++) {
      const { ctx, restore } = this.#mocks[i];
      FunctionPrototypeCall(restore, ctx);
    }
  }

  #setupMock(ctx, fnToMatch) {
    const mock = new Proxy(fnToMatch, {
      __proto__: null,
      apply(_fn, thisArg, argList) {
        const fn = FunctionPrototypeCall(mockNextImpl, ctx);
        let result;
        let error;

        try {
          result = ReflectApply(fn, thisArg, argList);
        } catch (err) {
          error = err;
          throw err;
        } finally {
          FunctionPrototypeCall(mockTrackCall, ctx, {
            __proto__: null,
            arguments: argList,
            error,
            result,
            stack: new Error(),
            target: undefined,
            this: thisArg,
          });
        }

        return result;
      },
      construct(target, argList, newTarget) {
        const realTarget = FunctionPrototypeCall(mockNextImpl, ctx);
        let result;
        let error;

        try {
          result = ReflectConstruct(realTarget, argList, newTarget);
        } catch (err) {
          error = err;
          throw err;
        } finally {
          FunctionPrototypeCall(mockTrackCall, ctx, {
            __proto__: null,
            arguments: argList,
            error,
            result,
            stack: new Error(),
            target,
            this: result,
          });
        }

        return result;
      },
      get(target, property, receiver) {
        if (property === "mock") {
          return ctx;
        }
        return ReflectGet(target, property, receiver);
      },
    });

    ArrayPrototypePush(this.#mocks, {
      __proto__: null,
      ctx,
      restore: restoreMockFunction,
    });
    return mock;
  }
}

const mock = new MockTracker();

// `require("node:test").assert`: `register(name, fn)` adds or overrides a
// `t.assert` method.
const testAssert = { __proto__: null, register: registerAssertion };
// `require("node:test").snapshot`.
const testSnapshot = {
  __proto__: null,
  setDefaultSnapshotSerializers,
  setResolveSnapshotPath,
};

test.test = test;
test.mock = mock;
test.assert = testAssert;
test.snapshot = testSnapshot;
test.before = before;
test.after = after;
test.beforeEach = beforeEach;
test.afterEach = afterEach;
test.run = run;

return {
  run,
  test,
  suite,
  it,
  describe,
  before,
  after,
  beforeEach,
  afterEach,
  mock,
  assert: testAssert,
  snapshot: testSnapshot,
  getTestContext,
  default: test,
};
})();
