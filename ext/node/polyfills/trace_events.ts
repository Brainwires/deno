// Copyright 2018-2026 the Deno authors. MIT license.

(function () {
const { core, internals, primordials } = __bootstrap;
const {
  ArrayFrom,
  ArrayIsArray,
  ArrayPrototypeJoin,
  ArrayPrototypePush,
  ArrayPrototypePushApply,
  ArrayPrototypeSlice,
  ArrayPrototypeUnshift,
  JSONParse,
  JSONStringify,
  MathTrunc,
  Number,
  NumberPrototypeToString,
  ReflectApply,
  SafeArrayIterator,
  SafeMap,
  SafeSet,
  SafeSetIterator,
  StringFromCharCode,
  StringPrototypeEndsWith,
  StringPrototypeReplaceAll,
  StringPrototypeSplit,
  StringPrototypeStartsWith,
  StringPrototypeTrim,
  Symbol,
  Uint8Array,
} = primordials;
const { ERR_TRACE_EVENTS_CATEGORY_REQUIRED } = core.loadExtScript(
  "ext:deno_node/internal/errors.ts",
);
const { validateObject, validateStringArray } = core.loadExtScript(
  "ext:deno_node/internal/validators.mjs",
);
const lazyBindingMod = core.createLazyLoader(
  "ext:deno_node/internal_binding/mod.ts",
);

function getProc() {
  // deno-lint-ignore no-process-global
  return typeof process !== "undefined" ? process : undefined;
}

// Each isolate (main + each worker thread) writes its own slice of trace
// events. The main thread, on exit, aggregates any worker slices left in cwd
// into a single `node_trace.${rotation}.log` so consumers see one combined
// file (matching Node's process-wide TracingController behavior).
// Resolved on first use rather than at module-load time so that worker_threads
// bootstrap (which sets up threadId/isMainThread) has finished by the time we
// read those fields.
let _wtExports = null;
function getWorkerThreadsExports() {
  if (_wtExports !== null) return _wtExports;
  try {
    _wtExports = core.loadExtScript("ext:deno_node/worker_threads.ts");
  } catch {
    _wtExports = {};
  }
  return _wtExports;
}

function getThreadId() {
  const wt = getWorkerThreadsExports();
  const tid = wt?.threadId ?? wt?.default?.threadId;
  return typeof tid === "number" ? tid : 0;
}

function isMainThreadProc() {
  const wt = getWorkerThreadsExports();
  const isMain = wt?.isMainThread ?? wt?.default?.isMainThread;
  if (typeof isMain === "boolean") return isMain;
  return getThreadId() === 0;
}

function workerSliceFilename(pid, tid) {
  return `.deno_trace_events_${pid}_t${tid}.json`;
}

const kCategories = Symbol("categories");
const kEnabled = Symbol("enabled");

const kMaxTracingCount = 10;

// Phase codes per V8/Chrome trace format.
const PHASE_NESTABLE_ASYNC_BEGIN = 98; // 'b'
const PHASE_NESTABLE_ASYNC_END = 101; // 'e'

const enabledTracingObjects = new SafeSet();
const categoryBuffers = new SafeMap();
const categoryRefCounts = new SafeMap();
const recordedEvents = [];
let asyncHooksRefcount = 0;
let exitHandlerRegistered = false;
let originalSetTimeout = null;
let originalSetInterval = null;
let originalSetImmediate = null;
let traceIdCounter = 0;

function getCategoryEnabledBuffer(category) {
  let buf = categoryBuffers.get(category);
  if (buf === undefined) {
    buf = new Uint8Array(1);
    categoryBuffers.set(category, buf);
  }
  return buf;
}

function incrementCategory(category) {
  const prev = categoryRefCounts.get(category) ?? 0;
  const next = prev + 1;
  categoryRefCounts.set(category, next);
  const buf = getCategoryEnabledBuffer(category);
  buf[0] = next > 255 ? 255 : next;
  if (category === "node.async_hooks") {
    asyncHooksRefcount++;
    if (asyncHooksRefcount === 1) installAsyncHooksTimerTracing();
  }
  if (category === "node" || category === "node.console") {
    installConsoleTracing();
  }
}

function decrementCategory(category) {
  const prev = categoryRefCounts.get(category) ?? 0;
  const next = prev > 0 ? prev - 1 : 0;
  if (next === 0) {
    categoryRefCounts.delete(category);
  } else {
    categoryRefCounts.set(category, next);
  }
  const buf = getCategoryEnabledBuffer(category);
  buf[0] = next > 255 ? 255 : next;
  if (category === "node.async_hooks") {
    if (asyncHooksRefcount > 0) asyncHooksRefcount--;
    if (asyncHooksRefcount === 0) uninstallAsyncHooksTimerTracing();
  }
}

class Tracing {
  constructor(categories) {
    this[kCategories] = categories;
    this[kEnabled] = false;
  }

  enable() {
    if (!this[kEnabled]) {
      this[kEnabled] = true;
      for (const category of new SafeArrayIterator(this[kCategories])) {
        incrementCategory(category);
      }
      enabledTracingObjects.add(this);
      if (enabledTracingObjects.size > kMaxTracingCount) {
        const p = getProc();
        if (p && p.emitWarning) {
          p.emitWarning(
            "Possible trace_events memory leak detected. There are more than " +
              `${kMaxTracingCount} enabled Tracing objects.`,
          );
        }
      }
      ensureExitHandlerInstalled();
    }
  }

  disable() {
    if (this[kEnabled]) {
      this[kEnabled] = false;
      for (const category of new SafeArrayIterator(this[kCategories])) {
        decrementCategory(category);
      }
      enabledTracingObjects.delete(this);
    }
  }

  get enabled() {
    return this[kEnabled];
  }

  get categories() {
    return ArrayPrototypeJoin(this[kCategories], ",");
  }
}

function createTracing(options) {
  validateObject(options, "options");
  validateStringArray(options.categories, "options.categories");
  if (options.categories.length <= 0) {
    throw new ERR_TRACE_EVENTS_CATEGORY_REQUIRED();
  }
  return new Tracing(options.categories);
}

function getEnabledCategories() {
  const seen = new SafeSet();
  for (const tracing of new SafeSetIterator(enabledTracingObjects)) {
    for (const category of new SafeArrayIterator(tracing[kCategories])) {
      seen.add(category);
    }
  }
  if (seen.size === 0) {
    return undefined;
  }
  return ArrayPrototypeJoin(ArrayFrom(seen), ",");
}

// denext: tracing requested on the command line (`--trace-event-categories`,
// `--trace-events-enabled`): the process then always writes its trace file,
// with Node's `__metadata` events, even when no category produced an event.
let cliTracing = false;
function markCliTracing() {
  cliTracing = true;
  ensureExitHandlerInstalled();
}

// Whether an event of `category` (Node's comma-separated list, such as
// `node,node.console`) is recorded: one of its names is enabled.
function isTraceCategoryEnabled(category) {
  if (typeof category !== "string") return false;
  for (
    const name of new SafeArrayIterator(StringPrototypeSplit(category, ","))
  ) {
    if ((categoryRefCounts.get(StringPrototypeTrim(name)) ?? 0) > 0) {
      return true;
    }
  }
  return false;
}

function nowMicros() {
  return MathTrunc(performance.now() * 1000);
}

// Node's `internalBinding('trace_events').trace(phase, category, name, id,
// data)`: an event of a category nobody enabled is dropped, and `data`
// becomes `args.data`.
function trace(phase, category, name, id, data) {
  if (!isTraceCategoryEnabled(category)) return;
  const ph = StringFromCharCode(phase);
  const p = getProc();
  const event = {
    pid: p ? p.pid : 0,
    tid: getThreadId(),
    ts: nowMicros(),
    ph,
    cat: category,
    name,
  };
  if (id !== undefined && id !== null) {
    event.id = "0x" + NumberPrototypeToString(Number(id), 16);
  }
  if (data !== undefined && data !== null) {
    event.args = { data };
  } else {
    event.args = {};
  }
  ArrayPrototypePush(recordedEvents, event);
  ensureExitHandlerInstalled();
}

function writeTraceFile() {
  const p = getProc();
  const pid = p ? p.pid : 0;
  if (isMainThreadProc()) {
    writeMainTraceFile(pid);
  } else {
    writeWorkerSliceFile(pid);
  }
}

let _fsExports = null;
function getFs() {
  if (_fsExports !== null) return _fsExports;
  try {
    _fsExports = core.loadExtScript("ext:deno_node/fs.ts");
  } catch {
    _fsExports = {};
  }
  return _fsExports;
}

function writeMainTraceFile(pid) {
  const fs = getFs();
  const allEvents = ArrayPrototypeSlice(recordedEvents);
  // Pull in any worker-thread slices written by this process before exit.
  let entries;
  try {
    entries = fs.readdirSync(".");
  } catch {
    entries = [];
  }
  const prefix = `.deno_trace_events_${pid}_t`;
  for (const entryName of new SafeArrayIterator(entries)) {
    if (typeof entryName !== "string") continue;
    if (
      !StringPrototypeStartsWith(entryName, prefix) ||
      !StringPrototypeEndsWith(entryName, ".json")
    ) {
      continue;
    }
    try {
      const text = fs.readFileSync(entryName, "utf-8");
      const slice = JSONParse(text);
      if (slice && ArrayIsArray(slice.traceEvents)) {
        for (const ev of new SafeArrayIterator(slice.traceEvents)) {
          ArrayPrototypePush(allEvents, ev);
        }
      }
    } catch {
      // Skip unreadable / partial slice files.
    }
    try {
      fs.unlinkSync(entryName);
    } catch {
      // Best-effort cleanup.
    }
  }
  if (allEvents.length === 0 && !cliTracing) return;
  if (cliTracing) {
    ArrayPrototypePushApply(allEvents, mainMetadataEvents(pid));
  }
  // Node's `--trace-event-file-pattern`, `${pid}` and `${rotation}`
  // substituted.
  const pattern = traceFilePattern();
  const fileFor = (rotation) =>
    StringPrototypeReplaceAll(
      StringPrototypeReplaceAll(pattern, "${rotation}", `${rotation}`),
      "${pid}",
      `${pid}`,
    );
  let rotation = 1;
  let filename = fileFor(rotation);
  while (existsSync(filename) && rotation < 1000) {
    rotation++;
    filename = fileFor(rotation);
  }
  try {
    fs.writeFileSync(filename, JSONStringify({ traceEvents: allEvents }));
  } catch {
    // Best-effort exit-time write.
  }
}

function writeWorkerSliceFile(pid) {
  if (recordedEvents.length === 0 && !cliTracing) return;
  const tid = getThreadId();
  const filename = workerSliceFilename(pid, tid);
  const events = ArrayPrototypeSlice(recordedEvents);
  if (cliTracing) {
    const wt = getWorkerThreadsExports();
    const name = wt?.threadName ?? wt?.default?.threadName;
    ArrayPrototypePush(
      events,
      metadataEvent(pid, tid, "thread_name", {
        name: `[worker ${tid}] ${name || "WorkerThread"}`,
      }),
    );
  }
  try {
    getFs().writeFileSync(
      filename,
      JSONStringify({ traceEvents: events }),
    );
  } catch {
    // Best-effort exit-time write.
  }
}

function metadataEvent(pid, tid, name, args) {
  return { pid, tid, ts: 0, ph: "M", cat: "__metadata", name, args };
}

function getOptionValue(name) {
  try {
    return core.loadExtScript("ext:deno_node/internal/options.ts")
      .getOptionValue(name);
  } catch {
    return undefined;
  }
}

function traceFilePattern() {
  const pattern = getOptionValue("--trace-event-file-pattern");
  return typeof pattern === "string" && pattern !== ""
    ? pattern
    : "node_trace.${rotation}.log";
}

// Node's `__metadata` events for the process: the thread names, the
// process name (as started, and as `process.title` set it since), and the
// versions.
function mainMetadataEvents(pid) {
  const p = getProc();
  const tid = getThreadId();
  const events = [
    metadataEvent(pid, tid, "thread_name", { name: "JavaScriptMainThread" }),
    metadataEvent(pid, tid + 1, "thread_name", {
      name: "PlatformWorkerThread",
    }),
  ];
  const startTitle = getOptionValue("--title") || "node";
  ArrayPrototypePush(
    events,
    metadataEvent(pid, tid, "process_name", { name: startTitle }),
  );
  const title = p?.title;
  if (typeof title === "string" && title !== startTitle) {
    ArrayPrototypePush(
      events,
      metadataEvent(pid, tid, "process_name", { name: title }),
    );
  }
  if (p) {
    ArrayPrototypePush(
      events,
      metadataEvent(pid, tid, "version", { node: p.versions.node }),
      metadataEvent(pid, tid, "node", {
        process: {
          versions: p.versions,
          arch: p.arch,
          platform: p.platform,
          release: p.release,
        },
      }),
    );
  }
  return events;
}

function existsSync(path) {
  try {
    getFs().statSync(path);
    return true;
  } catch {
    return false;
  }
}

function ensureExitHandlerInstalled() {
  if (exitHandlerRegistered) return;
  const p = getProc();
  if (!p || !p.on) return;
  exitHandlerRegistered = true;
  p.on("exit", writeTraceFile);
}

function installAsyncHooksTimerTracing() {
  if (originalSetTimeout !== null) return;
  originalSetTimeout = globalThis.setTimeout;
  originalSetInterval = globalThis.setInterval;
  originalSetImmediate = globalThis.setImmediate;

  globalThis.setTimeout = function (cb, ms, ...args) {
    if (typeof cb !== "function") {
      const callArgs = ArrayPrototypeSlice(args);
      ArrayPrototypeUnshift(callArgs, cb, ms);
      return ReflectApply(originalSetTimeout, null, callArgs);
    }
    const id = ++traceIdCounter;
    trace(PHASE_NESTABLE_ASYNC_BEGIN, "node,node.async_hooks", "Timeout", id);
    const wrapped = function () {
      try {
        return ReflectApply(cb, this, arguments);
      } finally {
        trace(
          PHASE_NESTABLE_ASYNC_END,
          "node,node.async_hooks",
          "Timeout",
          id,
        );
      }
    };
    const callArgs = ArrayPrototypeSlice(args);
    ArrayPrototypeUnshift(callArgs, wrapped, ms);
    return ReflectApply(originalSetTimeout, null, callArgs);
  };

  if (typeof originalSetImmediate === "function") {
    globalThis.setImmediate = function (cb, ...args) {
      if (typeof cb !== "function") {
        const callArgs = ArrayPrototypeSlice(args);
        ArrayPrototypeUnshift(callArgs, cb);
        return ReflectApply(originalSetImmediate, null, callArgs);
      }
      const id = ++traceIdCounter;
      trace(
        PHASE_NESTABLE_ASYNC_BEGIN,
        "node,node.async_hooks",
        "Immediate",
        id,
      );
      const wrapped = function () {
        try {
          return ReflectApply(cb, this, arguments);
        } finally {
          trace(
            PHASE_NESTABLE_ASYNC_END,
            "node,node.async_hooks",
            "Immediate",
            id,
          );
        }
      };
      const callArgs = ArrayPrototypeSlice(args);
      ArrayPrototypeUnshift(callArgs, wrapped);
      return ReflectApply(originalSetImmediate, null, callArgs);
    };
  }
}

// denext: Node's `node.console` events: `console.count()` /
// `countReset()` as counter events (`C`, `args.data` the count) and
// `console.time()` / `timeLog()` / `timeEnd()` as begin / instant / end
// events, named `count::<label>` and `time::<label>`. The global console's
// counters live in its native wrap, so the traced methods keep their own.
// Installed once a category that covers them is enabled, and left in place
// (the events are dropped while it isn't).
const PHASE_COUNTER = 67; // 'C'
const PHASE_INSTANT = 110; // 'n'
const kConsoleCategory = "node,node.console";
let consoleTracingInstalled = false;

function installConsoleTracing() {
  if (consoleTracingInstalled) return;
  const target = globalThis.console;
  if (target === undefined || target === null) return;
  consoleTracingInstalled = true;
  const counts = new SafeMap();
  const timers = new SafeSet();
  const wrap = (name, before) => {
    const original = target[name];
    if (typeof original !== "function") return;
    target[name] = function (label = "default", ...args) {
      before(`${label}`);
      const callArgs = ArrayPrototypeSlice(args);
      ArrayPrototypeUnshift(callArgs, label);
      return ReflectApply(original, this, callArgs);
    };
  };
  wrap("count", (label) => {
    const count = (counts.get(label) ?? 0) + 1;
    counts.set(label, count);
    trace(PHASE_COUNTER, kConsoleCategory, `count::${label}`, 0, count);
  });
  wrap("countReset", (label) => {
    if (!counts.has(label)) return;
    counts.delete(label);
    trace(PHASE_COUNTER, kConsoleCategory, `count::${label}`, 0, 0);
  });
  wrap("time", (label) => {
    if (timers.has(label)) return;
    timers.add(label);
    trace(PHASE_NESTABLE_ASYNC_BEGIN, kConsoleCategory, `time::${label}`, 0);
  });
  wrap("timeLog", (label) => {
    if (!timers.has(label)) return;
    trace(PHASE_INSTANT, kConsoleCategory, `time::${label}`, 0);
  });
  wrap("timeEnd", (label) => {
    if (!timers.has(label)) return;
    timers.delete(label);
    trace(PHASE_NESTABLE_ASYNC_END, kConsoleCategory, `time::${label}`, 0);
  });
}

function uninstallAsyncHooksTimerTracing() {
  if (originalSetTimeout === null) return;
  globalThis.setTimeout = originalSetTimeout;
  globalThis.setInterval = originalSetInterval;
  if (originalSetImmediate !== null) {
    globalThis.setImmediate = originalSetImmediate;
  }
  originalSetTimeout = null;
  originalSetInterval = null;
  originalSetImmediate = null;
}

// Expose trace + getCategoryEnabledBuffer on the internalBinding('trace_events')
// surface so the Node test fixtures that go through `internal/test/binding`
// observe the same state as the public API.
try {
  const binding = lazyBindingMod().getBinding("trace_events");
  if (binding && typeof binding === "object") {
    binding.getCategoryEnabledBuffer = getCategoryEnabledBuffer;
    binding.isTraceCategoryEnabled = isTraceCategoryEnabled;
    binding.trace = trace;
  }
} catch {
  // best-effort: binding registry may not be available in all contexts
}

// denext: node:console's `node.console` events (console.count / time).
internals.nodeTraceEvent = trace;

return {
  markCliTracing,
  default: {
    createTracing,
    getEnabledCategories,
  },
  createTracing,
  getEnabledCategories,
};
})();
