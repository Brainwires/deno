// Copyright 2018-2026 the Deno authors. MIT license.

// `node --test` and `run()`: each test file in its own process, as Node's
// `lib/internal/test_runner/runner.js`. A child started with
// `NODE_TEST_CONTEXT=child-v8` reports its events on stdout as frames (JSON
// here, not Node's V8 serialization; a child is always this runtime), and the
// parent reports them in file order, renumbers the top-level tests, counts the
// summary, and reports a file itself when it reported no test or failed outside
// one.

(function () {
"use strict";

const { core, primordials } = __bootstrap;
const {
  ArrayFrom,
  ArrayPrototypeFilter,
  ArrayPrototypeJoin,
  ArrayPrototypePush,
  ArrayPrototypePushApply,
  ArrayPrototypeShift,
  ArrayPrototypeSlice,
  ArrayPrototypeSome,
  ArrayPrototypeSort,
  DateNow,
  encodeURIComponent,
  Error,
  JSONParse,
  JSONStringify,
  MathMax,
  ObjectAssign,
  ObjectDefineProperty,
  ObjectFreeze,
  PromiseWithResolvers,
  RegExpPrototypeTest,
  SafeArrayIterator,
  SafePromiseAll,
  SafeRegExp,
  SafeSet,
  String,
  StringPrototypeEndsWith,
  StringPrototypeIndexOf,
  StringPrototypeSlice,
  StringPrototypeStartsWith,
} = primordials;

// The frame a child writes before each event's JSON and a newline.
const kFrame = "\u0001denext-test\u0001";

const kDefaultPatterns = ["test", "test/**/*", "test-*", "*[._-]test"];
const kFailureSubtests = "subtestsFailed";
const kCancelledTypes = new SafeSet([
  "cancelledByParent",
  "testAborted",
  "testTimeoutFailure",
]);
const kBuiltinReporters = ["spec", "tap", "dot", "junit", "lcov"];
// A file's own events carry no tags (Node's `FileTest`).
const kNoTags = ObjectFreeze([]);

function builtin(name) {
  return globalThis.process.getBuiltinModule(name);
}

function defaultPattern(stripTypes) {
  const extensions = stripTypes ? "js,mjs,cjs,ts,mts,cts" : "js,mjs,cjs";
  return `**/{${ArrayPrototypeJoin(kDefaultPatterns, ",")}}.{${extensions}}`;
}

// A child's error, whatever its class, as JSON. `actual` / `expected` go along
// as they are when JSON can carry them, else as their inspected text.
function serializeError(err, seen = new SafeSet(), styleStack = undefined) {
  if (err === null || typeof err !== "object") return err;
  if (seen.has(err)) return undefined;
  seen.add(err);
  const out = { __proto__: null, __error: true };
  for (
    const key of new SafeArrayIterator([
      "name",
      "message",
      "stack",
      "code",
      "failureType",
      "operator",
      "generatedMessage",
    ])
  ) {
    if (err[key] !== undefined) out[key] = err[key];
  }
  // The child knows which of its modules are CommonJS (see the runner's
  // `nodeStyleStack()`).
  if (styleStack !== undefined && typeof out.stack === "string") {
    out.stack = styleStack(out.stack);
  }
  for (const key of new SafeArrayIterator(["actual", "expected"])) {
    if (err[key] === undefined) continue;
    try {
      out[key] = JSONParse(JSONStringify(err[key]));
    } catch {
      out[key] = builtin("node:util").inspect(err[key]);
    }
  }
  if (err.cause !== undefined) {
    out.cause = serializeError(err.cause, seen, styleStack);
  }
  return out;
}

function deserializeError(value) {
  if (value === null || typeof value !== "object" || !value.__error) {
    return value;
  }
  const err = new Error(value.message);
  for (
    const key of new SafeArrayIterator([
      "name",
      "stack",
      "code",
      "failureType",
      "operator",
      "generatedMessage",
      "actual",
      "expected",
    ])
  ) {
    if (value[key] !== undefined) {
      // Own and hidden from `inspect()` where an `Error`'s own are.
      ObjectDefineProperty(err, key, {
        __proto__: null,
        value: value[key],
        writable: true,
        configurable: true,
        enumerable: key !== "name" && key !== "stack",
      });
    }
  }
  if (value.cause !== undefined) err.cause = deserializeError(value.cause);
  return err;
}

function frame(type, data) {
  return `${kFrame}${JSONStringify({ type, data })}\n`;
}

// Splits a child's stdout into frames and the text between them.
function createFrameParser(onEvent, onText) {
  let buffer = "";
  return {
    push(chunk) {
      buffer += chunk;
      while (buffer.length > 0) {
        const at = StringPrototypeIndexOf(buffer, kFrame);
        if (at === -1) {
          // Hold back what may be the start of a frame.
          let hold = 0;
          for (let n = kFrame.length - 1; n > 0; n--) {
            if (
              StringPrototypeEndsWith(
                buffer,
                StringPrototypeSlice(kFrame, 0, n),
              )
            ) {
              hold = n;
              break;
            }
          }
          const text = StringPrototypeSlice(buffer, 0, buffer.length - hold);
          if (text.length > 0) onText(text);
          buffer = StringPrototypeSlice(buffer, buffer.length - hold);
          return;
        }
        if (at > 0) onText(StringPrototypeSlice(buffer, 0, at));
        const end = StringPrototypeIndexOf(buffer, "\n", at);
        if (end === -1) {
          buffer = StringPrototypeSlice(buffer, at);
          return;
        }
        let event = null;
        try {
          event = JSONParse(
            StringPrototypeSlice(buffer, at + kFrame.length, end),
          );
        } catch { /* not ours after all */ }
        if (event !== null) onEvent(event);
        buffer = StringPrototypeSlice(buffer, end + 1);
      }
    },
    end() {
      if (buffer.length > 0) onText(buffer);
      buffer = "";
    },
  };
}

function hasMagic(pattern) {
  return RegExpPrototypeTest(new SafeRegExp(/[*?{}[\]!()]/), pattern);
}

// Node's `createTestFileList()`.
function createTestFileList(patterns, cwd, stripTypes, onMissing) {
  const fs = builtin("node:fs");
  const supplied = patterns !== undefined && patterns !== null &&
    patterns.length > 0;
  const results = fs.globSync(
    supplied ? patterns : defaultPattern(stripTypes),
    {
      __proto__: null,
      cwd,
      exclude: (name) => {
        const base = typeof name === "string" ? name : name?.name;
        return base === "node_modules";
      },
    },
  );
  const files = ArrayPrototypeFilter(results, (file) => {
    try {
      return !fs.statSync(builtin("node:path").resolve(cwd, file))
        .isDirectory();
    } catch {
      return false;
    }
  });
  if (
    supplied && files.length === 0 &&
    !ArrayPrototypeSome(patterns, hasMagic)
  ) {
    onMissing(`Could not find '${ArrayPrototypeJoin(patterns, ", ")}'`);
    return [];
  }
  return ArrayPrototypeSort(files);
}

// The run: reports through `report(type, data)`, resolves with the counters.
function startRun(options, report) {
  const process = globalThis.process;
  const { spawn } = builtin("node:child_process");
  const cwd = options.cwd ?? process.cwd();
  const counters = {
    __proto__: null,
    tests: 0,
    suites: 0,
    passed: 0,
    failed: 0,
    cancelled: 0,
    skipped: 0,
    todo: 0,
    topLevel: 0,
  };
  const started = DateNow();
  const running = new SafeSet();
  const children = new SafeSet();
  // Set once the run is interrupted: a child killed then is not a failure of
  // its file, and no file starts after it.
  let interrupted = false;

  function count(type, data) {
    if (data.nesting === 0) {
      counters.topLevel++;
      data.testNumber = counters.topLevel;
    }
    if (data.details?.type === "suite") {
      counters.suites++;
      return;
    }
    counters.tests++;
    if (data.skip !== undefined) counters.skipped++;
    else if (data.todo !== undefined) counters.todo++;
    else if (kCancelledTypes.has(data.details?.error?.failureType)) {
      counters.cancelled++;
    } else if (type === "test:fail") counters.failed++;
    else counters.passed++;
  }

  // Events of the files, in file order: a file's are held until the files
  // before it are done.
  const slots = [];
  let cursor = 0;
  function deliver(item) {
    if (item.type === "test:pass" || item.type === "test:fail") {
      count(item.type, item.data);
    }
    report(item.type, item.data);
  }
  function drain() {
    while (cursor < slots.length) {
      const slot = slots[cursor];
      while (slot.items.length > 0) deliver(ArrayPrototypeShift(slot.items));
      if (!slot.done) return;
      cursor++;
    }
  }

  function runFile(file, index, workerId) {
    const slot = slots[index];
    const { promise, resolve } = PromiseWithResolvers();
    const isSyntheticName = file === "";
    const args = ArrayPrototypeSlice(options.execArgv ?? []);
    if (isSyntheticName) {
      ArrayPrototypePush(
        args,
        `data:text/javascript,${
          encodeURIComponent(
            `import t from"node:test";await t[Symbol.for("denext.nodeTestIsolationNone")](${
              JSONStringify(options.noneFiles)
            });`,
          )
        }`,
      );
    } else {
      ArrayPrototypePush(args, file);
    }
    ArrayPrototypePushApply(args, options.argv ?? []);
    const env = {
      __proto__: null,
      ...(options.env ?? process.env),
      NODE_TEST_CONTEXT: "child-v8",
    };
    if (workerId !== null) env.NODE_TEST_WORKER_ID = String(workerId);
    if (options.colorChildren) env.FORCE_COLOR = "1";
    const extraOptions = [];
    if (options.only === true) ArrayPrototypePush(extraOptions, "--test-only");
    if (options.forceExit === true) {
      ArrayPrototypePush(extraOptions, "--test-force-exit");
    }
    if (options.timeout !== undefined && options.timeout !== null) {
      ArrayPrototypePush(extraOptions, `--test-timeout=${options.timeout}`);
    }
    // A value is quoted as NODE_OPTIONS quotes, so it stays one token.
    const { quoteNodeOptionsToken } = core.loadExtScript(
      "ext:deno_node/internal_binding/node_options.ts",
    );
    for (const value of new SafeArrayIterator(options.testNamePatterns ?? [])) {
      ArrayPrototypePush(
        extraOptions,
        quoteNodeOptionsToken(`--test-name-pattern=${value}`),
      );
    }
    for (const value of new SafeArrayIterator(options.testSkipPatterns ?? [])) {
      ArrayPrototypePush(
        extraOptions,
        quoteNodeOptionsToken(`--test-skip-pattern=${value}`),
      );
    }
    for (const value of new SafeArrayIterator(options.testTagFilters ?? [])) {
      ArrayPrototypePush(
        extraOptions,
        RegExpPrototypeTest(new SafeRegExp(/\s/), value)
          ? `"--experimental-test-tag-filter=${value}"`
          : `--experimental-test-tag-filter=${value}`,
      );
    }
    if (options.rerunFailuresFilePath) {
      const value = options.rerunFailuresFilePath;
      ArrayPrototypePush(
        extraOptions,
        RegExpPrototypeTest(new SafeRegExp(/\s/), value)
          ? `"--test-rerun-failures=${value}"`
          : `--test-rerun-failures=${value}`,
      );
    }
    if (options.randomSeed !== undefined && options.randomSeed !== null) {
      ArrayPrototypePush(
        extraOptions,
        `--test-random-seed=${options.randomSeed}`,
      );
    }
    if (extraOptions.length > 0) {
      env.NODE_OPTIONS = ArrayPrototypeJoin([
        env.NODE_OPTIONS ?? "",
        ...new SafeArrayIterator(extraOptions),
      ], " ");
    }
    if (options.coverageDir) env.DENO_COVERAGE_DIR = options.coverageDir;

    let reported = 0;
    let failedTop = false;
    const absoluteFile = isSyntheticName
      ? undefined
      : builtin("node:path").resolve(cwd, file);
    const push = (type, data) => {
      data.file ??= absoluteFile;
      ArrayPrototypePush(slot.items, { type, data });
      drain();
    };
    // Execution-ordered: past the per-file buffer, as Node's are.
    const pushQueue = (type, data) => {
      report(type, {
        __proto__: null,
        name: data.name,
        nesting: data.nesting,
        tags: data.tags ?? kNoTags,
        file: absoluteFile ?? data.file,
      });
    };
    // A test is reported as started once something in it is done (Node's
    // `reportStarted`).
    const heldStarts = [];
    const releaseStarts = () => {
      for (const data of new SafeArrayIterator(heldStarts)) {
        push("test:start", data);
      }
      heldStarts.length = 0;
    };
    const fileEntry = {
      __proto__: null,
      name: isSyntheticName ? "<root>" : file,
      nesting: 0,
      tags: kNoTags,
    };
    // `--test-isolation=none` has no test of its own for the files' process.
    if (!isSyntheticName) {
      pushQueue("test:enqueue", fileEntry);
      pushQueue("test:dequeue", fileEntry);
    }
    const parser = createFrameParser((event) => {
      const { type, data } = event;
      if (type === "test:summary") {
        // A script's `process.stdout.columns`, which sizes the coverage table.
        if (options.silentFile && data.columns > 0) {
          process.stdout.columns = data.columns;
        }
        if (isSyntheticName) return;
      }
      if (type === "test:start") {
        pushQueue("test:enqueue", data);
        pushQueue("test:dequeue", data);
        ArrayPrototypePush(heldStarts, data);
        return;
      }
      if (type === "test:pass" || type === "test:fail") {
        pushQueue("test:complete", data);
        releaseStarts();
        reported++;
        if (data.nesting === 0 && type === "test:fail") failedTop = true;
        if (data.details?.error !== undefined) {
          data.details.error = deserializeError(data.details.error);
        }
      }
      push(type, data);
    }, (text) => {
      push("test:stdout", { __proto__: null, message: text });
    });

    const fileStarted = DateNow();
    if (!isSyntheticName) running.add(file);
    const child = spawn(process.execPath, args, {
      __proto__: null,
      cwd,
      env,
      stdio: ["ignore", "pipe", "pipe"],
      signal: options.signal,
    });
    children.add(child);
    let spawnError;
    child.on("error", (err) => {
      spawnError = err;
    });
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      // deno-lint-ignore deno-internal/prefer-primordials -- not an Array
      parser.push(chunk);
    });
    child.stderr.setEncoding("utf8");
    let stderrTail = "";
    child.stderr.on("data", (chunk) => {
      stderrTail += chunk;
      let at;
      while ((at = StringPrototypeIndexOf(stderrTail, "\n")) !== -1) {
        push("test:stderr", {
          __proto__: null,
          message: StringPrototypeSlice(stderrTail, 0, at + 1),
        });
        stderrTail = StringPrototypeSlice(stderrTail, at + 1);
      }
    });
    child.on("close", (code, signal) => {
      children.delete(child);
      running.delete(file);
      if (interrupted) {
        slot.done = true;
        resolve();
        return;
      }
      parser.end();
      releaseStarts();
      if (stderrTail.length > 0) {
        push("test:stderr", {
          __proto__: null,
          message: `${stderrTail}\n`,
        });
      }
      let error;
      if (spawnError !== undefined || code !== 0 || signal !== null) {
        const failureType = failedTop ? kFailureSubtests : "testCodeFailure";
        error = spawnError ?? ObjectAssign(new Error("test failed"), {
          __proto__: null,
          code: "ERR_TEST_FAILURE",
          cause: "test failed",
          failureType,
          exitCode: code,
          signal,
          stack: undefined,
        });
        error.failureType ??= failureType;
      }
      const skip = reported > 0 &&
        (error === undefined || error.failureType === kFailureSubtests);
      if (
        !skip && !(isSyntheticName && error === undefined) &&
        !options.silentFile
      ) {
        const loc = {
          __proto__: null,
          line: 1,
          column: 1,
          file: absoluteFile,
        };
        const name = isSyntheticName ? "<root>" : file;
        push("test:start", {
          __proto__: null,
          nesting: 0,
          name,
          tags: kNoTags,
          ...loc,
        });
        push(error === undefined ? "test:pass" : "test:fail", {
          __proto__: null,
          nesting: 0,
          name,
          tags: kNoTags,
          ...loc,
          details: {
            __proto__: null,
            duration_ms: DateNow() - fileStarted,
            type: "test",
            ...(error === undefined ? {} : { error }),
          },
        });
      }
      if (!isSyntheticName) pushQueue("test:complete", fileEntry);
      if (code !== 0 || signal !== null) counters.exitFailed = true;
      slot.done = true;
      drain();
      resolve();
    });
    return promise;
  }

  async function runAll(files) {
    for (let i = 0; i < files.length; i++) {
      ArrayPrototypePush(slots, { __proto__: null, items: [], done: false });
    }
    const limit = files.length === 0
      ? 1
      : options.isolation === "none"
      ? 1
      : options.maxConcurrency;
    let next = 0;
    const workers = [];
    for (let worker = 1; worker <= limit; worker++) {
      ArrayPrototypePush(
        workers,
        (async () => {
          while (next < files.length && !interrupted) {
            const index = next++;
            await runFile(files[index], index, worker);
          }
        })(),
      );
    }
    await SafePromiseAll(workers);
    return {
      __proto__: null,
      counters,
      durationMs: DateNow() - started,
    };
  }

  function interrupt() {
    interrupted = true;
    for (const child of new SafeArrayIterator(ArrayFrom(children))) {
      try {
        child.kill("SIGKILL");
      } catch { /* gone */ }
    }
  }

  return {
    runAll,
    interrupt,
    runningNames: () => ArrayFrom(running),
  };
}

function maxConcurrencyOf(concurrency, isolation) {
  if (isolation === "none") return 1;
  if (concurrency === true || concurrency === undefined) {
    return MathMax(builtin("node:os").availableParallelism() - 1, 1);
  }
  if (typeof concurrency === "number" && concurrency >= 1) return concurrency;
  return 1;
}

function summaryData(result, file) {
  const { counters: c, durationMs } = result;
  return {
    __proto__: null,
    success: c.failed === 0 && c.cancelled === 0,
    counts: {
      __proto__: null,
      tests: c.tests,
      suites: c.suites,
      passed: c.passed,
      failed: c.failed,
      cancelled: c.cancelled,
      skipped: c.skipped,
      todo: c.todo,
      topLevel: c.topLevel,
    },
    duration_ms: durationMs,
    file,
  };
}

// An existing path, or a `file:` URL or a package name, as an import URL.
function reporterUrl(specifier, cwd) {
  const { pathToFileURL } = builtin("node:url");
  const path = builtin("node:path");
  if (
    StringPrototypeStartsWith(specifier, "./") ||
    StringPrototypeStartsWith(specifier, "../") ||
    path.isAbsolute(specifier)
  ) {
    return pathToFileURL(path.resolve(cwd, specifier)).href;
  }
  if (RegExpPrototypeTest(new SafeRegExp(/^[a-z][a-z0-9+.-]+:/i), specifier)) {
    return specifier;
  }
  const require = builtin("node:module").createRequire(
    path.resolve(cwd, "noop.js"),
  );
  try {
    return pathToFileURL(require.resolve(specifier)).href;
  } catch (err) {
    err.code = "ERR_MODULE_NOT_FOUND";
    err.message = `[ERR_MODULE_NOT_FOUND]: ${err.message}`;
    throw err;
  }
}

function destinationStream(destination, cwd) {
  const process = globalThis.process;
  if (destination === "stdout") return process.stdout;
  if (destination === "stderr") return process.stderr;
  const path = builtin("node:path");
  return builtin("node:fs").createWriteStream(path.resolve(cwd, destination));
}

return {
  kFrame,
  kBuiltinReporters,
  defaultPattern,
  serializeError,
  deserializeError,
  frame,
  createTestFileList,
  startRun,
  maxConcurrencyOf,
  summaryData,
  reporterUrl,
  destinationStream,
};
})();
