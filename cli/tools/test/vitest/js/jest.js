// Copyright 2018-2026 the Deno authors. MIT license.

// Jest on top of the Vitest layer: the `jest` object (mapped onto `vi`),
// Jest's test API semantics (a declared parameter of a test or hook is the
// `done` callback, a hook's return value is not a teardown, `fit` / `xit` /
// `test.failing` ...), and, for a Jest project (`flavor: "jest"`), Jest's
// globals, `test.each` titles (`%p`, `$variable`), snapshot names
// ("describe test 1") and snapshot file header, `globals`,
// `snapshotSerializers` and `fakeTimers` from the config.

import { i as index } from "./vendor/vitest_core.js";
import { f as prettyFormat } from "./vendor/pretty_format.js";
import { callerUrl } from "./mocker.js";

const { vi } = index;

const TEST_NAME_SEPARATOR = Symbol.for("deno.vitest.testNameSeparator");
const FORMAT_TITLE = Symbol.for("deno.vitest.formatTitle");
const SNAPSHOT_HEADER = Symbol.for("deno.vitest.snapshotHeader");

const { createRequire } = await import("node:module");
const { fileURLToPath, pathToFileURL } = await import("node:url");
const { isAbsolute } = await import("node:path");
const { format: utilFormat } = await import("node:util");

function mocker() {
  return globalThis[Symbol.for("deno.vitest.mocker")];
}

function ops() {
  return globalThis.Deno[globalThis.Deno.internal].vitestMocks;
}

function isUserModuleUrl(url) {
  return url.startsWith("file:") && !url.includes("/node_modules/");
}

/** Loads the module at `url` (resolved from `importer`) synchronously. */
function loadSync(url, specifier, importer) {
  if (url.startsWith("deno-vitest:///virtual/")) {
    // a Vite asset module (CSS, an image): its value
    return ops().importSync(url).default;
  }
  if (isUserModuleUrl(url) && !/\.json(\?|$)/.test(url)) {
    mocker()?.prepareSync();
    return ops().importSync(url);
  }
  const require = createRequire(importer);
  if (url.startsWith("file:")) return require(fileURLToPath(url));
  return require(url.startsWith("node:") ? url : specifier);
}

/** Jest's synchronous `require` of a module, for `jest.requireActual()`. */
function requireActual(path, importer) {
  const resolved = ops().resolve(path, importer);
  if (resolved === null) return createRequire(importer)(path);
  return loadSync(resolved, path, importer);
}

/**
 * The `require` of a module in a Jest project: Jest's module registry
 * resolves like an import (aliases included), serves mocks and fresh
 * modules after `jest.resetModules()` (see the CLI's transform).
 */
export function jestRequire(importer) {
  const nodeRequire = createRequire(importer);
  function require(specifier) {
    const resolved = ops().resolveImport(String(specifier), importer);
    if (resolved === null) return nodeRequire(specifier);
    if (resolved.startsWith("deno-vitest:///mock?")) {
      const id = Number(new URL(resolved).searchParams.get("id"));
      const exports = mocker().requireMockById(id);
      if (exports === undefined) {
        throw new Error(
          `require(${
            JSON.stringify(specifier)
          }) of a mock whose factory is asynchronous is not supported by \`deno test\`: import it instead.`,
        );
      }
      return exports;
    }
    return loadSync(resolved, specifier, importer);
  }
  require.resolve = nodeRequire.resolve;
  require.cache = nodeRequire.cache;
  return require;
}

/**
 * What Jest's fake timers fake: modern timers (`@jest/fake-timers`) fake every
 * API `@sinonjs/fake-timers` can fake on Node's global, `process.nextTick`
 * and `queueMicrotask` included (Vitest leaves those two alone), except the
 * `doNotFake` ones; Node has no `Temporal`. Legacy timers fake the timer
 * functions and `process.nextTick` only.
 */
const JEST_FAKEABLE = [
  "Date",
  "hrtime",
  "nextTick",
  "performance",
  "queueMicrotask",
  "requestAnimationFrame",
  "cancelAnimationFrame",
  "requestIdleCallback",
  "cancelIdleCallback",
  "setImmediate",
  "clearImmediate",
  "setInterval",
  "clearInterval",
  "setTimeout",
  "clearTimeout",
  "Intl",
];
const JEST_LEGACY_FAKED = [
  "nextTick",
  "setImmediate",
  "clearImmediate",
  "setInterval",
  "clearInterval",
  "setTimeout",
  "clearTimeout",
];

/** The project's `fakeTimers` config, the defaults of `useFakeTimers()`. */
let projectFakeTimers = {};

/** Jest's fake timer config (merged over the project's), as Vitest's. */
export function toVitestTimerConfig(config) {
  if (config === "legacy") config = { legacyFakeTimers: true };
  config = {
    ...projectFakeTimers,
    ...(config !== null && typeof config === "object" ? config : {}),
  };
  const doNotFake = Array.isArray(config.doNotFake) ? config.doNotFake : [];
  const out = {
    loopLimit: config.timerLimit || 100_000,
    shouldClearNativeTimers: true,
    toFake:
      (config.legacyFakeTimers === true ? JEST_LEGACY_FAKED : JEST_FAKEABLE)
        .filter((name) => !doNotFake.includes(name)),
  };
  if (config.now !== undefined) out.now = config.now;
  if (config.advanceTimers === true) {
    out.shouldAdvanceTime = true;
  } else if (typeof config.advanceTimers === "number") {
    out.shouldAdvanceTime = true;
    out.advanceTimeDelta = config.advanceTimers;
  }
  return out;
}

/** The properties `jest.replaceProperty()` replaced, restored together. */
const replacedProperties = new Set();

function replaceProperty(object, key, value) {
  if (object === null || object === undefined) {
    throw new Error(
      `replaceProperty could not find an object on which to replace ${
        String(key)
      }`,
    );
  }
  const descriptor = Object.getOwnPropertyDescriptor(object, key);
  if (descriptor === undefined) {
    throw new Error(`${String(key)} property does not exist`);
  }
  if (descriptor.get !== undefined) {
    throw new Error(
      `Cannot replace the ${
        String(key)
      } property because it has a getter. Use \`jest.spyOn(object, '${
        String(key)
      }', 'get').mockReturnValue(value)\` instead.`,
    );
  }
  const original = object[key];
  const replaced = {
    replaceValue(newValue) {
      object[key] = newValue;
      return replaced;
    },
    restore() {
      object[key] = original;
      replacedProperties.delete(replaced);
    },
  };
  object[key] = value;
  replacedProperties.add(replaced);
  return replaced;
}

function restoreReplacedProperties() {
  for (const replaced of [...replacedProperties]) replaced.restore();
}

/** `jest`, mapped onto `vi`. */
export const jest = {
  fn: (...args) => vi.fn(...args),
  spyOn: (...args) => vi.spyOn(...args),
  mocked: (value) => value,
  isMockFunction: (value) => vi.isMockFunction(value),
  replaceProperty: (object, key, value) => replaceProperty(object, key, value),
  clearAllMocks: () => (vi.clearAllMocks(), jest),
  resetAllMocks: () => (vi.resetAllMocks(), jest),
  restoreAllMocks:
    () => (vi.restoreAllMocks(), restoreReplacedProperties(), jest),
  useFakeTimers: (config) => (
    // 'modern' / 'legacy' (Jest 26) and `legacyFakeTimers` all get the
    // modern implementation
    vi.useFakeTimers(toVitestTimerConfig(config)), jest
  ),
  useRealTimers: () => (vi.useRealTimers(), jest),
  isFakeTimers: () => vi.isFakeTimers(),
  advanceTimersByTime: (ms) => (vi.advanceTimersByTime(ms), jest),
  advanceTimersByTimeAsync: async (ms) => (
    await vi.advanceTimersByTimeAsync(ms), jest
  ),
  advanceTimersToNextTimer: (
    steps,
  ) => (vi.advanceTimersToNextTimer(steps), jest),
  advanceTimersToNextTimerAsync: async (steps) => (
    await vi.advanceTimersToNextTimerAsync(steps), jest
  ),
  advanceTimersToNextFrame: () => (vi.advanceTimersToNextFrame(), jest),
  runAllTimers: () => (vi.runAllTimers(), jest),
  runAllTimersAsync: async () => (await vi.runAllTimersAsync(), jest),
  runOnlyPendingTimers: () => (vi.runOnlyPendingTimers(), jest),
  runOnlyPendingTimersAsync: async () => (
    await vi.runOnlyPendingTimersAsync(), jest
  ),
  runAllTicks: () => (vi.runAllTicks(), jest),
  runAllImmediates: () => (vi.runAllTimers(), jest),
  clearAllTimers: () => (vi.clearAllTimers(), jest),
  getTimerCount: () => vi.getTimerCount(),
  setSystemTime: (time) => (vi.setSystemTime(time), jest),
  getRealSystemTime: () => vi.getRealSystemTime(),
  now: () => Date.now(),
  setTimeout: (testTimeout) => (vi.setConfig({ testTimeout }), jest),
  retryTimes: (retry) => (vi.setConfig({ retry }), jest),
  getSeed: () => globalThis.__vitest_worker__?.config?.sequence?.seed ?? 0,
  isEnvironmentTornDown: () => false,
  mock(path, factory, _options) {
    mocker().queueMock(path, callerUrl(jest.mock), factory);
    return jest;
  },
  doMock(path, factory, _options) {
    mocker().queueMock(path, callerUrl(jest.doMock), factory);
    return jest;
  },
  // Jest's ESM mocking: registered when called, before the dynamic imports
  // that follow it.
  "unstable_mockModule"(path, factory, _options) {
    mocker().queueMock(path, callerUrl(jest.unstable_mockModule), factory);
    return jest;
  },
  "unstable_unmockModule"(path) {
    mocker().queueUnmock(path, callerUrl(jest.unstable_unmockModule));
    return jest;
  },
  setMock(path, moduleExports) {
    mocker().queueMock(path, callerUrl(jest.setMock), () => moduleExports);
    return jest;
  },
  unmock(path) {
    mocker().queueUnmock(path, callerUrl(jest.unmock));
    return jest;
  },
  deepUnmock(path) {
    mocker().queueUnmock(path, callerUrl(jest.deepUnmock));
    return jest;
  },
  dontMock(path) {
    mocker().queueUnmock(path, callerUrl(jest.dontMock));
    return jest;
  },
  requireActual(path) {
    return requireActual(path, callerUrl(jest.requireActual));
  },
  requireMock(path) {
    const importer = callerUrl(jest.requireMock);
    const exports = mocker().requireMock(
      path,
      importer,
      (p) => requireActual(p, importer),
    );
    if (exports === undefined) {
      throw new Error(
        `jest.requireMock(${
          JSON.stringify(path)
        }) is not supported by \`deno test\` for this module (its mock loads asynchronously): use \`await vi.importMock()\` instead.`,
      );
    }
    return exports;
  },
  createMockFromModule(path) {
    return mocker().mockObject(
      requireActual(path, callerUrl(jest.createMockFromModule)),
    );
  },
  genMockFromModule(path) {
    return mocker().mockObject(
      requireActual(path, callerUrl(jest.genMockFromModule)),
    );
  },
  resetModules() {
    mocker().resetModules();
    return jest;
  },
  isolateModules(fn) {
    mocker().resetModules();
    try {
      fn();
    } finally {
      mocker().resetModules();
    }
    return jest;
  },
  async isolateModulesAsync(fn) {
    mocker().resetModules();
    try {
      await fn();
    } finally {
      mocker().resetModules();
    }
  },
  enableAutomock() {
    throw new Error(
      "jest.enableAutomock() is not supported by `deno test`: mock modules with jest.mock().",
    );
  },
  disableAutomock: () => jest,
};

// --- Jest's test API semantics ------------------------------------------

const DONE_AND_PROMISE =
  "Test functions cannot both take a 'done' callback and return something. Either use a 'done' callback, or return a promise.";

/** Runs `fn(done)` as a promise (Jest's callback-style tests and hooks). */
function callWithDone(fn, thisArg) {
  return new Promise((resolve, reject) => {
    let finished = false;
    const done = (reason) => {
      if (finished) return;
      finished = true;
      if (reason === undefined || reason === null) {
        resolve();
      } else {
        reject(
          reason instanceof Error ? reason : new Error(
            `Failed: ${
              typeof reason === "string" ? reason : prettyFormat(reason)
            }`,
          ),
        );
      }
    };
    done.fail = (reason) => done(reason ?? new Error("Failed"));
    try {
      const result = fn.call(thisArg, done);
      if (result !== undefined && typeof result?.then === "function") {
        finished = true;
        reject(new Error(DONE_AND_PROMISE));
      }
    } catch (error) {
      finished = true;
      reject(error);
    }
  });
}

/** A test function with Jest's semantics. */
function adaptTestFn(fn) {
  if (typeof fn !== "function" || fn.length === 0) return fn;
  return function jestTest() {
    return callWithDone(fn, this);
  };
}

/**
 * A hook with Jest's semantics: a declared parameter is `done`, and the
 * returned value (a spy, say) is not a teardown function.
 */
function adaptHookFn(fn) {
  if (typeof fn !== "function") return fn;
  if (fn.length > 0) {
    return function jestHook() {
      return callWithDone(fn, this);
    };
  }
  return function jestHook() {
    const result = fn.call(this);
    if (result !== null && typeof result?.then === "function") {
      return Promise.resolve(result).then(() => undefined);
    }
    return undefined;
  };
}

/** `test(name, fn, timeout)` / `test(name, options, fn)` arguments. */
function adaptTestArgs(args) {
  const out = [...args];
  if (typeof out[1] === "function") out[1] = adaptTestFn(out[1]);
  else if (typeof out[2] === "function") out[2] = adaptTestFn(out[2]);
  return out;
}

const CHAIN_RETURNING_API = new Set(["runIf", "skipIf"]);
const LEFT_ALONE = new Set([
  "each",
  "for",
  "extend",
  "scoped",
  "override",
  "describe",
  "beforeEach",
  "afterEach",
  "beforeAll",
  "afterAll",
  "aroundEach",
  "aroundAll",
]);

/** `test` / `it` with Jest's semantics, chains included. */
function wrapTestApi(api) {
  return new Proxy(api, {
    apply(target, thisArg, args) {
      return Reflect.apply(target, thisArg, adaptTestArgs(args));
    },
    get(target, key) {
      if (key === "failing") return wrapTestApi(target.fails);
      const value = Reflect.get(target, key);
      if (typeof value !== "function" || typeof key !== "string") return value;
      if (CHAIN_RETURNING_API.has(key)) {
        return (...args) => wrapTestApi(value.apply(target, args));
      }
      if (LEFT_ALONE.has(key)) return value.bind(target);
      return wrapTestApi(value);
    },
  });
}

function wrapHook(hook) {
  return (fn, ...rest) => hook(adaptHookFn(fn), ...rest);
}

export const it = wrapTestApi(index.it);
export const test = wrapTestApi(index.test);
export const describe = index.describe;
export const beforeAll = wrapHook(index.beforeAll);
export const beforeEach = wrapHook(index.beforeEach);
export const afterAll = wrapHook(index.afterAll);
export const afterEach = wrapHook(index.afterEach);
export const expect = index.expect;

/** Jest's globals (`injectGlobals`). */
export function jestGlobalApis() {
  return {
    afterAll,
    afterEach,
    beforeAll,
    beforeEach,
    describe,
    expect,
    fdescribe: describe.only,
    fit: it.only,
    it,
    jest,
    test,
    xdescribe: describe.skip,
    xit: it.skip,
    xtest: test.skip,
  };
}

// --- Jest's `test.each` titles ----------------------------------------------

const PLACEHOLDERS = /%[sdifjoOp]/g;

function prettyValue(value) {
  return prettyFormat(value, { maxDepth: 1, min: true });
}

/** jest-each's `formatTitle` for one row of an array table. */
function formatArrayTitle(title, row, index) {
  let formatted = title
    .replace(/%%/g, "\uE000")
    .replace(/%#/g, String(index))
    .replace(/%\$/g, String(index + 1));
  for (const value of row) {
    const match = formatted.match(PLACEHOLDERS);
    if (match === null) break;
    // a `%` inside an interpolated string must not be read as a placeholder
    const normalised = typeof value === "string"
      ? value.replace(/%/g, "\uE001")
      : value;
    if (match[0] === "%p") {
      formatted = formatted.replace("%p", () => prettyValue(normalised));
    } else {
      formatted = utilFormat(
        formatted.replace(/%(?![sdifjoOp])/g, "\uE002"),
        normalised,
      ).replace(/\uE002/g, "%");
    }
  }
  return formatted
    .replace(/\uE001/g, "%")
    .replace(/\uE000/g, "%");
}

function getPath(object, path) {
  let value = object;
  for (const key of path) {
    if (value === null || value === undefined) return undefined;
    value = value[key];
  }
  return value;
}

/** jest-each's `$variable` interpolation for an object row. */
function interpolateVariables(title, row, index) {
  const keys = Object.keys(row);
  if (keys.length === 0) return title.replace("$#", String(index));
  const pattern = new RegExp(
    `\\$(${
      keys.map((k) => k.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("|")
    })[.\\w]*`,
    "g",
  );
  return title
    .replace(pattern, (match) => {
      const value = getPath(row, match.slice(1).split("."));
      return value === null ||
          (typeof value !== "object" && typeof value !== "function")
        ? String(value)
        : prettyValue(value);
    })
    .replace("$#", String(index));
}

/** Formats a `test.each` / `describe.each` title like Jest does. */
export function formatJestTitle(template, items, index) {
  const first = items[0];
  if (
    items.length === 1 && first !== null && typeof first === "object" &&
    !Array.isArray(first) && !PLACEHOLDERS.test(template)
  ) {
    PLACEHOLDERS.lastIndex = 0;
    return interpolateVariables(template, first, index);
  }
  PLACEHOLDERS.lastIndex = 0;
  return formatArrayTitle(template, items, index);
}

// --- Jest projects ------------------------------------------------------

export const JEST_SNAPSHOT_HEADER =
  "// Jest Snapshot v1, https://jestjs.io/docs/snapshot-testing";

/**
 * Applies a Jest project's runtime options before Vitest collects the test
 * file (called by runtime.js for `flavor: "jest"`).
 */
export function installJestProject(config) {
  globalThis[TEST_NAME_SEPARATOR] = " ";
  globalThis[Symbol.for("deno.vitest.jestAsyncExpect")] = true;
  globalThis[Symbol.for("deno.vitest.jestEquality")] = true;
  globalThis[FORMAT_TITLE] = formatJestTitle;
  globalThis[SNAPSHOT_HEADER] = config.snapshotHeader ?? JEST_SNAPSHOT_HEADER;
  if (config.globals) {
    for (const [name, value] of Object.entries(jestGlobalApis())) {
      globalThis[name] = value;
    }
  }
  for (const [name, value] of Object.entries(config.jestGlobals ?? {})) {
    globalThis[name] = structuredClone(value);
  }
}

/**
 * Applies the options that need the test environment: snapshot serializers
 * and globally enabled fake timers.
 */
/**
 * A view of the global object for `window` / `self` in a Jest project's DOM
 * environment, which has no `Deno` (code under test checks
 * `"Deno" in window` to tell a server runtime, SWR for one). The `Deno`
 * global itself stays: Deno's Node.js APIs use it. A test can still set and
 * delete `window.Deno`.
 */
function windowWithoutDeno() {
  const hidden = new Set(["Deno"]);
  const shadow = new Map();
  const descriptor = (key) => ({
    value: shadow.get(key),
    writable: true,
    enumerable: true,
    configurable: true,
  });
  return new Proxy(globalThis, {
    has: (target, key) =>
      hidden.has(key) ? shadow.has(key) : Reflect.has(target, key),
    get: (target, key) =>
      hidden.has(key) ? shadow.get(key) : Reflect.get(target, key),
    set(target, key, value) {
      if (!hidden.has(key)) return Reflect.set(target, key, value);
      shadow.set(key, value);
      return true;
    },
    defineProperty(target, key, desc) {
      if (!hidden.has(key)) return Reflect.defineProperty(target, key, desc);
      shadow.set(key, desc.value ?? desc.get?.());
      return true;
    },
    deleteProperty(target, key) {
      if (!hidden.has(key)) return Reflect.deleteProperty(target, key);
      shadow.delete(key);
      return true;
    },
    ownKeys: (target) => [
      ...Reflect.ownKeys(target).filter((key) => !hidden.has(key)),
      ...shadow.keys(),
    ],
    getOwnPropertyDescriptor(target, key) {
      if (!hidden.has(key)) {
        return Reflect.getOwnPropertyDescriptor(target, key);
      }
      return shadow.has(key) ? descriptor(key) : undefined;
    },
  });
}

/**
 * Jest applies `moduleNameMapper` to every module of a test, so to the
 * `require()` calls of CommonJS dependencies too (their imports are mapped
 * by the CLI's resolver, a user module's `require` is Jest's). Installed
 * once the test environment is set up: Jest loads the environment outside
 * the test's modules.
 */
async function mapDependencyRequires(mappers) {
  const compiled = (mappers ?? [])
    .filter((m) => typeof m.find === "object")
    .map((m) => ({
      regex: new RegExp(m.find.regex, m.find.flags),
      replacement: m.replacement,
    }));
  if (compiled.length === 0) return;
  const { default: Module } = await import("node:module");
  const resolveFilename = Module._resolveFilename;
  Module._resolveFilename = function (request, parent, ...rest) {
    const from = parent?.filename ?? "";
    if (
      typeof request === "string" &&
      (from.includes("/node_modules/") || from.includes("\\node_modules\\"))
    ) {
      for (const { regex, replacement } of compiled) {
        const matches = request.match(regex);
        if (matches === null) continue;
        request = replacement.replace(
          /\$([0-9]+)/g,
          (_, index) => matches[parseInt(index, 10)],
        );
        break;
      }
    }
    return resolveFilename.call(this, request, parent, ...rest);
  };
}

/**
 * `runnerFs` is the runner's own reads (see preload.js), or undefined: the
 * fixed paths `moduleNameMapper` maps to are then loaded like the test's.
 */
export async function setupJestEnvironment(
  config,
  environmentName,
  runnerFs,
) {
  if (environmentName === "jsdom" || environmentName === "happy-dom") {
    // Jest's DOM environments have no setImmediate (React's scheduler then
    // uses a MessageChannel, which changes when effects run).
    delete globalThis.setImmediate;
    delete globalThis.clearImmediate;
    const window = windowWithoutDeno();
    globalThis.window = window;
    globalThis.self = window;
  }
  for (const path of config.snapshotSerializers ?? []) {
    const mod = await import(pathToFileURL(path).href);
    expect.addSnapshotSerializer(mod.default ?? mod);
  }
  // a fixed path the config maps a module to is the runner's own import
  for (const { replacement } of config.moduleNameMapper ?? []) {
    if (
      typeof replacement === "string" && !replacement.includes("$") &&
      isAbsolute(replacement)
    ) {
      runnerFs?.allowImport(pathToFileURL(replacement).href);
    }
  }
  await mapDependencyRequires(config.moduleNameMapper);
  projectFakeTimers = config.jestFakeTimersConfig ?? {};
  if (config.jestFakeTimers) {
    vi.useFakeTimers(toVitestTimerConfig(config.jestFakeTimers));
  }
}

/**
 * Registers the `__mocks__` files of the project's roots for the packages
 * they name: Jest uses them without a `jest.mock()` call (Node's built-in
 * modules excepted).
 */
export async function registerRootMocks(roots, runnerFs) {
  const fs = runnerFs ?? await import("node:fs");
  const path = await import("node:path");
  const { isBuiltin } = await import("node:module");
  for (const root of roots ?? []) {
    const dir = path.join(root, "__mocks__");
    let entries;
    try {
      entries = fs.readdirSync(dir, { recursive: true });
    } catch {
      continue;
    }
    const importer = pathToFileURL(path.join(root, "__deno_jest_root__.js"))
      .href;
    for (const entry of entries) {
      const file = path.join(dir, String(entry));
      if (!fs.statSync(file).isFile()) continue;
      const name = String(entry).replace(/\\/g, "/").replace(/\.[^/.]+$/, "")
        .replace(/\/index$/, "");
      if (isBuiltin(name) || name.startsWith(".")) continue;
      mocker().queueRedirect(name, importer, file);
    }
  }
}
