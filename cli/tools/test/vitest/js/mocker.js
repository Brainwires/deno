// Copyright 2018-2026 the Deno authors. MIT license.

// The runtime side of Vitest's module mocking under `deno test`.
//
// The CLI hoists `vi.mock()` & co. above a test module's imports (which
// become dynamic imports, see cli/tools/test/vitest/transform.rs), so the
// mocks below are registered before anything imports a mocked module. A
// mock is registered with the worker's module loader under the URL its path
// resolves to (cli/tools/test/vitest/mocks.rs); the loader then resolves
// every import of that URL from user code to a `deno-vitest:///mock?id=<n>`
// module, whose source is generated here once the loader asks for it: one
// export per key of the mock factory's result (or of the automocked module,
// or of the `__mocks__` file), each a binding that `vi.spyOn()` can replace.

import { mockObject as vitestMockObject } from "./vendor/mocker.js";
import { d as createMockInstance } from "./vendor/spy.js";

const MOCKER = Symbol.for("deno.vitest.mocker");
const EXPORT_SETTERS = Symbol.for("deno.vitest.exports");
const ACTUAL_PREFIX = "deno-vitest:///actual?url=";

const fs = await import("node:fs");
const nodePath = await import("node:path");
const nodeModule = await import("node:module");
const { fileURLToPath, pathToFileURL } = await import("node:url");

// Captured before a test can install fake timers.
const realSetTimeout = globalThis.setTimeout;
const realClearTimeout = globalThis.clearTimeout;
const FACTORY_CYCLE_TIMEOUT = 1000;
/** The getters of the factory results' properties the mocker keeps live. */
const liveGetters = new WeakSet();

const FACTORY_ERROR =
  '[vitest] There was an error when mocking a module. If you are using "vi.mock" factory, make sure there are no top level variables inside, since this call is hoisted to top of the file. Read more: https://vitest.dev/api/vi.html#vi-mock';

/** The file (URL) of the first caller outside the built-in modules. */
export function callerUrl(fn) {
  const holder = {};
  const limit = Error.stackTraceLimit;
  Error.stackTraceLimit = 30;
  Error.captureStackTrace(holder, fn);
  Error.stackTraceLimit = limit;
  for (const line of String(holder.stack).split("\n").slice(1)) {
    const match = /\(?((?:file|https?|npm|jsr):[^\s()]+):\d+:\d+\)?\s*$/
      .exec(line);
    if (match) return match[1];
  }
  return pathToFileURL(Deno.cwd() + "/__deno_vitest_root__.js").href;
}

function toFileUrl(importer) {
  if (
    /^[a-z][a-z0-9+.-]*:/i.test(importer) && !/^[a-z]:[\\/]/i.test(importer)
  ) {
    return importer;
  }
  return pathToFileURL(importer).href;
}

/** Vitest's `findMockRedirect`: the `__mocks__` file for a module. */
function findMockRedirect(root, mockPath, external) {
  const path = external || mockPath;
  if (
    external || nodeModule.isBuiltin(mockPath) || !fs.existsSync(mockPath)
  ) {
    const mockFolder = nodePath.join(root, "__mocks__", nodePath.dirname(path));
    if (!fs.existsSync(mockFolder)) return null;
    const findFile = (folder, base) => {
      for (const file of fs.readdirSync(folder)) {
        if (nodePath.basename(file, nodePath.extname(file)) !== base) continue;
        const candidate = nodePath.resolve(folder, file);
        if (fs.statSync(candidate).isFile()) return candidate;
        const index = findFile(candidate, "index");
        if (index) return index;
      }
      return null;
    };
    return findFile(mockFolder, nodePath.basename(path));
  }
  const fullPath = nodePath.resolve(
    nodePath.dirname(path),
    "__mocks__",
    nodePath.basename(path),
  );
  return fs.existsSync(fullPath) ? fullPath : null;
}

function mockObject(object, mockExports = {}, type = "automock") {
  return vitestMockObject(
    {
      globalConstructors: { Object, Function, Array, Map, RegExp },
      createMockInstance,
      type,
    },
    object,
    mockExports,
  );
}

function isBareSpecifier(path) {
  return !path.startsWith(".") && !path.startsWith("/") &&
    !/^[a-z][a-z0-9+.-]*:/i.test(path);
}

/**
 * A stand-in for a module namespace that `vi.spyOn()` can redefine
 * properties of: each redefinition replaces the module's export.
 */
function namespaceProxy(namespace, setter) {
  const descriptor = (key) =>
    key in namespace
      ? {
        value: namespace[key],
        writable: true,
        enumerable: true,
        configurable: true,
      }
      : undefined;
  return new Proxy(Object.create(null), {
    get: (_, key) => namespace[key],
    has: (_, key) => key in namespace,
    ownKeys: () => Reflect.ownKeys(namespace),
    getOwnPropertyDescriptor: (_, key) => descriptor(key),
    getPrototypeOf: () => null,
    set(_, key, value) {
      return setter(key, value) || key in namespace;
    },
    defineProperty(_, key, desc) {
      if ("value" in desc) return setter(key, desc.value);
      if (typeof desc.get === "function") return setter(key, desc.get());
      return false;
    },
    deleteProperty: () => true,
  });
}

class ModuleMocker {
  #ops;
  #root;
  #nextId = 1;
  /** id -> mock */
  #mocks = new Map();
  /** resolved URL -> id */
  #byUrl = new Map();
  /** id -> setters of the generated module instances */
  #instances = new Map();
  #errors = new Map();
  #generation = 0;

  constructor(ops, root) {
    this.#ops = ops;
    this.#root = root;
    ops.enable();
    this.#poll();
  }

  async #poll() {
    for (;;) {
      let request;
      try {
        request = await this.#ops.poll();
      } catch {
        return;
      }
      this.#provide(request[0], request[1]);
    }
  }

  /** Resolves a mocked path like an import from `importer` does. */
  #resolve(path, importer) {
    const importerUrl = toFileUrl(importer);
    const url = this.#ops.resolve(path, importerUrl);
    if (url === null) return null;
    let fsPath = null;
    if (url.startsWith("file:")) {
      try {
        fsPath = fileURLToPath(url);
      } catch {
        // not a local path
      }
    }
    const external = isBareSpecifier(path) || url.includes("/node_modules/") ||
        url.startsWith("node:")
      ? path.replace(/^node:/, "")
      : null;
    return { url, fsPath: fsPath ?? path, external };
  }

  queueMock(path, importer, factoryOrOptions) {
    const resolved = this.#resolve(path, importer);
    // A module that does not resolve cannot be imported either.
    if (resolved === null) return;
    let type;
    let redirect = null;
    let factory;
    if (typeof factoryOrOptions === "function") {
      type = "manual";
      factory = factoryOrOptions;
    } else if (factoryOrOptions?.spy) {
      type = "autospy";
    } else {
      redirect = findMockRedirect(
        this.#root,
        resolved.fsPath,
        resolved.external,
      );
      type = redirect === null ? "automock" : "redirect";
    }
    const id = this.#nextId++;
    this.#mocks.set(id, {
      id,
      path,
      importer: toFileUrl(importer),
      url: resolved.url,
      type,
      factory,
      redirect,
      promise: undefined,
      exports: undefined,
      running: false,
      actualKeys: undefined,
    });
    this.#byUrl.set(resolved.url, id);
    this.#ops.set(resolved.url, id);
  }

  queueUnmock(path, importer) {
    const resolved = this.#resolve(path, importer);
    if (resolved === null) return;
    this.#byUrl.delete(resolved.url);
    this.#ops.set(resolved.url, 0);
  }

  async importActual(path, importer) {
    const resolved = this.#ops.resolve(path, toFileUrl(importer));
    if (resolved === null) return await import(path);
    return await import(ACTUAL_PREFIX + encodeURIComponent(resolved));
  }

  async importMock(path, importer) {
    const resolved = this.#resolve(path, importer);
    if (resolved === null) {
      throw new Error(`[vitest] Cannot resolve "${path}" to mock it.`);
    }
    const id = this.#byUrl.get(resolved.url);
    if (id !== undefined) return await this.moduleFor(id);
    const redirect = findMockRedirect(
      this.#root,
      resolved.fsPath,
      resolved.external,
    );
    if (redirect !== null) return await import(pathToFileURL(redirect).href);
    const actual = await import(
      ACTUAL_PREFIX + encodeURIComponent(resolved.url)
    );
    return mockObject(actual);
  }

  mockObject(object, mockExports, type) {
    return mockObject(object, mockExports, type);
  }

  getMockContext() {
    return { callstack: null };
  }

  /**
   * Runs the synchronous factories of mocks nothing imported yet, so a test
   * importing one lazily (`React.lazy(() => import("./x"))`) gets it as soon
   * as an unmocked module: Vitest serves a mock within microtasks, which a
   * test may rely on (an `act()` that has to see the lazy component).
   * Factories that take `importOriginal` or are async keep running lazily.
   */
  prewarm() {
    for (const mock of this.#mocks.values()) {
      if (
        mock.promise !== undefined || mock.type !== "manual" ||
        mock.factory.length > 0 ||
        mock.factory.constructor?.name === "AsyncFunction" ||
        this.#byUrl.get(mock.url) !== mock.id
      ) {
        continue;
      }
      mock.promise = this.#evaluate(mock);
      // A factory failing now runs again when the mock is imported.
      mock.promise.catch(() => {
        mock.promise = undefined;
      });
    }
  }

  resetModules() {
    this.#generation++;
    this.#ops.setGeneration(this.#generation);
  }

  /** The exports of mock `id` (runs its factory once). */
  moduleFor(id) {
    const mock = this.#mocks.get(id);
    mock.promise ??= this.#evaluate(mock);
    return mock.promise;
  }

  /** The exports of mock `id` so far (see `#provide`). */
  peek(id) {
    return this.#mocks.get(id).exports ?? {};
  }

  async #evaluate(mock) {
    mock.running = true;
    try {
      let exports;
      switch (mock.type) {
        case "manual": {
          try {
            exports = await mock.factory(async () => {
              const actual = await this.importActual(mock.path, mock.importer);
              mock.actualKeys ??= Object.keys(actual);
              return actual;
            });
          } catch (cause) {
            const error = new Error(FACTORY_ERROR, { cause });
            error.stack = `${error.name}: ${error.message}\n${
              String(cause?.stack ?? "").split("\n").slice(1).join("\n")
            }`;
            throw error;
          }
          if (
            exports === null ||
            (typeof exports !== "object" && typeof exports !== "function")
          ) {
            throw new TypeError(
              `[vitest] vi.mock("${mock.path}", factory?) is not returning an object. Did you mean to return an object with a "default" key?`,
            );
          }
          break;
        }
        case "redirect":
          exports = await import(pathToFileURL(mock.redirect).href);
          break;
        default:
          exports = mockObject(
            await import(ACTUAL_PREFIX + encodeURIComponent(mock.url)),
            {},
            mock.type,
          );
      }
      // A promise the factory exports (say, a rejected `startup`) is
      // awaited by its importers only once their modules are loaded, which
      // takes another turn of the event loop than under Vitest: do not
      // report its rejection as unhandled meanwhile.
      if (exports !== null && typeof exports === "object") {
        for (const key of Object.keys(exports)) {
          const value = exports[key];
          if (value instanceof Promise) value.catch(() => {});
        }
      }
      if (mock.type === "manual") this.#makeLive(mock, exports);
      mock.exports = exports;
      // the loader generates this mock's modules itself from now on
      if (exports !== null && typeof exports === "object") {
        this.#ops.keys(mock.id, Object.keys(exports));
      }
      // modules generated while the factory ran get the values now
      for (const setter of this.#instances.get(mock.id) ?? []) {
        for (const key of Object.keys(exports)) setter(key, exports[key]);
      }
      return exports;
    } finally {
      mock.running = false;
    }
  }

  /**
   * Under Vitest, importers of a factory mock read the factory's object
   * itself, so assigning one of its properties later (say, a `vi.hoisted()`
   * object a test mutates) changes what they see. Each data property becomes
   * an accessor that also updates the mock modules' bindings.
   */
  #makeLive(mock, exports) {
    if (exports === null || typeof exports !== "object") return;
    for (const key of Object.keys(exports)) {
      const descriptor = Object.getOwnPropertyDescriptor(exports, key);
      if (
        !descriptor?.configurable || !descriptor.writable ||
        !("value" in descriptor)
      ) {
        continue;
      }
      let value = descriptor.value;
      const get = () => value;
      liveGetters.add(get);
      Object.defineProperty(exports, key, {
        configurable: true,
        enumerable: descriptor.enumerable,
        get,
        set: (newValue) => {
          value = newValue;
          for (const setter of this.#instances.get(mock.id) ?? []) {
            setter(key, newValue);
          }
        },
      });
    }
  }

  /** Registers a generated module instance of mock `id`. */
  bind(id, namespace, setter) {
    let setters = this.#instances.get(id);
    if (setters === undefined) {
      setters = new Set();
      this.#instances.set(id, setters);
    }
    setters.add(setter);
    const mock = this.#mocks.get(id);
    exportSetters().set(namespace, (key, value) => {
      if (mock.exports && typeof mock.exports === "object") {
        try {
          mock.exports[key] = value;
        } catch {
          // a frozen or namespace object: the bindings still change
        }
      }
      let found = false;
      for (const set of setters) found = set(key, value) || found;
      return found;
    });
  }

  /**
   * Updates the exports of mock modules from their factory's result where
   * it is not kept live (getters, or properties a test redefined).
   */
  syncGetters() {
    for (const mock of this.#mocks.values()) {
      const exports = mock.exports;
      if (exports === null || typeof exports !== "object") continue;
      // Properties the mock keeps live itself need no update.
      const keys = Object.keys(exports).filter((key) =>
        !liveGetters.has(Object.getOwnPropertyDescriptor(exports, key)?.get)
      );
      const setters = this.#instances.get(mock.id);
      if (keys.length === 0 || setters === undefined) continue;
      for (const key of keys) {
        let value;
        try {
          value = exports[key];
        } catch {
          continue;
        }
        for (const setter of setters) setter(key, value);
      }
    }
  }

  errorFor(requestId) {
    const error = this.#errors.get(requestId);
    this.#errors.delete(requestId);
    return error;
  }

  async #provide(requestId, url) {
    let source;
    const params = new URL(url).searchParams;
    const names = params.getAll("name");
    try {
      const id = Number(params.get("id"));
      const mock = this.#mocks.get(id);
      if (mock === undefined) throw new Error(`Unknown Vitest mock ${url}`);
      const keys = new Set(names);
      let deferred = false;
      if (mock.running) {
        // Another importer of the mock while its factory runs (it waits
        // for the factory), or a module the factory imports itself: a cycle
        // that cannot wait for the factory. Like Vitest, such a module gets
        // the mock's exports once the factory returns (with the names it
        // imports and those of the original module, if the factory loaded
        // it). A factory still running after a while is taken for a cycle.
        let timer;
        await Promise.race([
          mock.promise.catch(() => {}),
          new Promise((resolve) => {
            timer = realSetTimeout(resolve, FACTORY_CYCLE_TIMEOUT);
          }),
        ]);
        realClearTimeout(timer);
        deferred = mock.running;
      }
      if (deferred) {
        for (const key of mock.actualKeys ?? []) keys.add(key);
      } else {
        for (const key of Object.keys(await this.moduleFor(id))) keys.add(key);
      }
      source = moduleSource(id, [...keys]);
    } catch (error) {
      this.#errors.set(requestId, error);
      // The importers' names are exported so the module links, and then
      // throws the error when evaluated.
      source =
        `throw globalThis[Symbol.for("deno.vitest.mocker")].errorFor(${requestId});\n` +
        names.map((name, index) =>
          `let __${index};\nexport { __${index} as ${JSON.stringify(name)} };\n`
        ).join("");
    }
    this.#ops.respond(requestId, source, null);
  }
}

function exportSetters() {
  return (globalThis[EXPORT_SETTERS] ??= new WeakMap());
}

/**
 * The source of a mock module exporting `keys` of mock `id` (the loader
 * generates the same once it knows the keys, see mocks.rs).
 */
function moduleSource(id, keys) {
  let code =
    `const __vitest_mocker__ = globalThis[Symbol.for("deno.vitest.mocker")];\n` +
    `const __vitest_module__ = __vitest_mocker__.peek(${id});\n`;
  const cases = [];
  keys.forEach((key, index) => {
    const name = JSON.stringify(key);
    code += `let __${index} = __vitest_module__[${name}];\n` +
      `export { __${index} as ${name} };\n`;
    cases.push(`case ${name}: __${index} = value; return true;`);
  });
  code += `import * as __vitest_self__ from "deno-vitest:///self";\n` +
    `__vitest_mocker__.bind(${id}, __vitest_self__, (name, value) => { switch (name) { ${
      cases.join(" ")
    } } return false; });\n`;
  return code;
}

/**
 * Installs module mocking for the test file's worker: `vi.mock()` & co.,
 * and `vi.spyOn()` on module namespaces.
 */
export function installModuleMocker(vi, root) {
  const ops = globalThis.Deno[globalThis.Deno.internal].vitestMocks;
  const mocker = new ModuleMocker(ops, root);
  globalThis[MOCKER] = mocker;
  Object.defineProperty(globalThis, "__vitest_mocker__", {
    value: mocker,
    configurable: true,
    writable: true,
    enumerable: false,
  });

  const checkPath = (api, path) => {
    if (typeof path !== "string") {
      throw new TypeError(
        `vi.${api}() expects a string path, but received a ${typeof path}`,
      );
    }
  };
  vi.mock = function mock(path, factory) {
    checkPath("mock", path);
    mocker.queueMock(path, callerUrl(mock), factory);
  };
  vi.unmock = function unmock(path) {
    checkPath("unmock", path);
    mocker.queueUnmock(path, callerUrl(unmock));
  };
  vi.doMock = function doMock(path, factory) {
    checkPath("doMock", path);
    const importer = callerUrl(doMock);
    mocker.queueMock(path, importer, factory);
    const result = {};
    if (Symbol.dispose) {
      result[Symbol.dispose] = () => mocker.queueUnmock(path, importer);
    }
    return result;
  };
  vi.doUnmock = function doUnmock(path) {
    checkPath("doUnmock", path);
    mocker.queueUnmock(path, callerUrl(doUnmock));
  };
  vi.importActual = async function importActual(path) {
    return await mocker.importActual(path, callerUrl(importActual));
  };
  vi.importMock = async function importMock(path) {
    return await mocker.importMock(path, callerUrl(importMock));
  };
  vi.resetModules = function resetModules() {
    mocker.resetModules();
    return vi;
  };
  vi.dynamicImportSettled = async function dynamicImportSettled() {
    for (let i = 0; i < 3; i++) {
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
  };

  // `vi.spyOn(namespace, "export")` replaces the export for every importer.
  const spyOn = vi.spyOn;
  vi.spyOn = function (object, key, accessType) {
    const setter = object !== null && typeof object === "object"
      ? globalThis[EXPORT_SETTERS]?.get(object)
      : undefined;
    return setter === undefined
      ? spyOn(object, key, accessType)
      : spyOn(namespaceProxy(object, setter), key, accessType);
  };
  return mocker;
}
