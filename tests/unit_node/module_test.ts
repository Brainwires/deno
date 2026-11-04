// Copyright 2018-2026 the Deno authors. MIT license.

import {
  builtinModules,
  createRequire,
  // @ts-ignore Our internal @types/node is at v18.16.19 which predates
  // this change.
  findPackageJSON,
  findSourceMap,
  isBuiltin,
  Module,
  // @ts-ignore Our internal @types/node is at v18.16.19 which predates
  // this change. Updating it is difficult due to different types in Node
  // for `import.meta.filename` and `import.meta.dirname` that Deno
  // provides.
  register,
  // @ts-ignore Our internal @types/node is at v18.16.19 which predates
  // this change.
  runMain,
  SourceMap,
  stripTypeScriptTypes,
  syncBuiltinESMExports,
} from "node:module";
import { assert, assertEquals, assertThrows } from "@std/assert";
import process from "node:process";
import * as path from "node:path";
import { pathToFileURL } from "node:url";

Deno.test("[node/module _preloadModules] has internal require hook", () => {
  // Check if it's there
  // deno-lint-ignore no-explicit-any
  (Module as any)._preloadModules([
    "./tests/unit_node/testdata/add_global_property.js",
  ]);
  // deno-lint-ignore no-explicit-any
  assertEquals((globalThis as any).foo, "Hello");
});

Deno.test("[node/module runMain] loads module using the current process.argv", () => {
  process.argv = [
    process.argv[0],
    "./tests/unit_node/testdata/add_global_property_run_main.js",
  ];

  // deno-lint-ignore no-explicit-any
  (Module as any).runMain();
  // deno-lint-ignore no-explicit-any
  assertEquals((globalThis as any).calledViaRunMain, true);
});

Deno.test("[node/module _nodeModulePaths] prevents duplicate /node_modules/node_modules suffix", () => {
  // deno-lint-ignore no-explicit-any
  const actual: string[] = (Module as any)._nodeModulePaths(
    path.join(process.cwd(), "testdata", "node_modules", "foo"),
  );

  assert(
    !actual.some((dir) => /node_modules[/\\]node_modules/g.test(dir)),
    "Duplicate 'node_modules/node_modules' suffix found",
  );
});

Deno.test("[node/module _nodeModulePaths] prevents duplicate root /node_modules", () => {
  // deno-lint-ignore no-explicit-any
  const actual: string[] = (Module as any)._nodeModulePaths(
    path.join(process.cwd(), "testdata", "node_modules", "foo"),
  );

  assert(
    new Set(actual).size === actual.length,
    "Duplicate path entries found",
  );
  const root = path.parse(actual[0]).root;
  assert(
    actual.includes(path.join(root, "node_modules")),
    "Missing root 'node_modules' directory",
  );
});

Deno.test("Built-in Node modules have `node:` prefix", () => {
  let thrown = false;
  try {
    // @ts-ignore We want to explicitly test wrong call signature
    createRequire();
  } catch (e) {
    thrown = true;
    const stackLines = (e as Error).stack!.split("\n");
    // Assert that built-in node modules have `node:<mod_name>` specifiers.
    assert(stackLines.some((line: string) => line.includes("(node:module:")));
  }

  assert(thrown);
});

Deno.test("[node/module isBuiltin] recognizes node builtins", () => {
  assert(isBuiltin("node:fs"));
  assert(isBuiltin("node:test"));
  assert(isBuiltin("fs"));
  assert(isBuiltin("buffer"));

  assert(!isBuiltin("internal/errors"));
  assert(!isBuiltin("test"));
  assert(!isBuiltin(""));
  // deno-lint-ignore no-explicit-any
  assert(!isBuiltin(undefined as any));
});

// https://github.com/denoland/deno/issues/22731
Deno.test("[node/module builtinModules] has 'module' in builtins", () => {
  assert(builtinModules.includes("module"));
});

// https://github.com/denoland/deno/issues/34812
Deno.test("[node/module runMain] is exposed as a named export", () => {
  assertEquals(typeof runMain, "function");
  assertEquals(runMain, Module.runMain);
});

// https://github.com/denoland/deno/issues/18666
Deno.test("[node/module findSourceMap] is a function", () => {
  assertEquals(findSourceMap("foo"), undefined);
});

Deno.test("[node/module SourceMap] is exposed as a named export", () => {
  assertEquals(typeof SourceMap, "function");
  // @ts-ignore Not in all supported @types/node versions.
  assertEquals(SourceMap, Module.SourceMap);
});

// https://github.com/denoland/deno/issues/31039
Deno.test("[node/module findPackageJSON] is exported and finds package.json", async () => {
  const tempDir = await Deno.makeTempDir();
  try {
    const packageJsonPath = path.join(tempDir, "package.json");
    const entryPath = path.join(tempDir, "src", "main.js");
    await Deno.mkdir(path.dirname(entryPath), { recursive: true });
    await Deno.writeTextFile(packageJsonPath, '{"name":"fixture"}');
    await Deno.writeTextFile(entryPath, "export default 1;");
    const realPackageJsonPath = await Deno.realPath(packageJsonPath);

    assertEquals(typeof findPackageJSON, "function");
    assertEquals(
      findPackageJSON(pathToFileURL(entryPath).href),
      realPackageJsonPath,
    );
    assertEquals(
      findPackageJSON(
        "./src/main.js",
        pathToFileURL(
          path.join(tempDir, "mod.mjs"),
        ),
      ),
      realPackageJsonPath,
    );
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});

Deno.test("[node/module findPackageJSON] returns package root for bare package subpaths", async () => {
  const tempDir = await Deno.makeTempDir();
  try {
    const packageRoot = path.join(tempDir, "node_modules", "foo");
    const packageJsonPath = path.join(packageRoot, "package.json");
    await Deno.mkdir(packageRoot, { recursive: true });
    await Deno.writeTextFile(
      packageJsonPath,
      '{"name":"foo","exports":{".":"./index.js"}}',
    );
    await Deno.writeTextFile(path.join(packageRoot, "index.js"), "");
    const realPackageJsonPath = await Deno.realPath(packageJsonPath);

    assertEquals(
      findPackageJSON(
        "foo/not-exported",
        pathToFileURL(
          path.join(tempDir, "mod.mjs"),
        ),
      ),
      realPackageJsonPath,
    );
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});

Deno.test("[node/module findPackageJSON] absolute specifiers use closest package.json", async () => {
  const tempDir = await Deno.makeTempDir();
  try {
    const packageRoot = path.join(tempDir, "node_modules", "foo");
    const nestedDir = path.join(packageRoot, "lib");
    const packageJsonPath = path.join(packageRoot, "package.json");
    const nestedPackageJsonPath = path.join(nestedDir, "package.json");
    const entryPath = path.join(nestedDir, "index.js");
    await Deno.mkdir(nestedDir, { recursive: true });
    await Deno.writeTextFile(
      packageJsonPath,
      '{"name":"foo","exports":"./lib/index.js"}',
    );
    await Deno.writeTextFile(nestedPackageJsonPath, '{"name":"nested"}');
    await Deno.writeTextFile(entryPath, "");
    const realPackageJsonPath = await Deno.realPath(packageJsonPath);
    const realNestedPackageJsonPath = await Deno.realPath(
      nestedPackageJsonPath,
    );

    assertEquals(
      findPackageJSON("foo", pathToFileURL(path.join(tempDir, "mod.mjs"))),
      realPackageJsonPath,
    );
    assertEquals(
      findPackageJSON(pathToFileURL(entryPath).href),
      realNestedPackageJsonPath,
    );
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});

Deno.test("[node/module require.resolve] checks runtime-selected node_modules paths", async () => {
  const tempDir = await Deno.makeTempDir();
  try {
    const nodeModulesDir = path.join(tempDir, "project", "node_modules");
    await Deno.mkdir(nodeModulesDir, { recursive: true });
    await Deno.writeTextFile(
      path.join(nodeModulesDir, "package.json"),
      '{"main":"runtime-selected-entry.js"}',
    );

    const childPath = path.join(tempDir, "resolve.mjs");
    await Deno.writeTextFile(
      childPath,
      `
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
try {
  require.resolve(${JSON.stringify(nodeModulesDir + path.sep)});
  console.error("unexpected success");
  Deno.exit(1);
} catch (error) {
  if (!(error instanceof Error) || error.name !== "NotCapable") {
    console.error(error);
    Deno.exit(2);
  }
  if (String(error).includes("runtime-selected-entry.js")) {
    console.error(error);
    Deno.exit(3);
  }
}
`,
    );

    const command = new Deno.Command(Deno.execPath(), {
      args: ["run", childPath],
      stdout: "piped",
      stderr: "piped",
    });
    const { code, stdout, stderr } = await command.output();
    assertEquals(
      code,
      0,
      new TextDecoder().decode(stdout) + new TextDecoder().decode(stderr),
    );
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});

Deno.test("[node/module findPackageJSON] checks package metadata reads", async () => {
  const tempDir = await Deno.makeTempDir();
  try {
    await Deno.writeTextFile(
      path.join(tempDir, "package.json"),
      '{"name":"runtime-selected-package"}',
    );
    await Deno.writeTextFile(
      path.join(tempDir, "static-entry.mjs"),
      "export const value = 1;",
    );

    const childPath = path.join(tempDir, "find-package.mjs");
    await Deno.writeTextFile(
      childPath,
      `
import { findPackageJSON } from "node:module";
import { value } from "./static-entry.mjs";

if (value !== 1) Deno.exit(1);
try {
  findPackageJSON(import.meta.resolve("./static-entry.mjs"));
  console.error("unexpected success");
  Deno.exit(2);
} catch (error) {
  if (
    !(error instanceof Error) || error.name !== "NotCapable" ||
    !String(error).includes("package.json")
  ) {
    console.error(error);
    Deno.exit(3);
  }
}
`,
    );

    const command = new Deno.Command(Deno.execPath(), {
      args: ["run", childPath],
      stdout: "piped",
      stderr: "piped",
    });
    const { code, stdout, stderr } = await command.output();
    assertEquals(
      code,
      0,
      new TextDecoder().decode(stdout) + new TextDecoder().decode(stderr),
    );
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});

// https://github.com/denoland/deno/issues/24902
Deno.test({
  name: "[node/module register] runs the hooks module's initialize",
  // The hooks worker and its message port live as long as the process, as in
  // Node: hooks can't be unregistered.
  sanitizeResources: false,
  fn() {
    const hooks = "data:text/javascript,export function initialize(data) " +
      "{ return data.n * 2; }";
    // Node returns what `initialize` returned, but @types/node declares
    // `register()` as returning void, so read the value as `unknown`.
    const result: unknown = register(hooks, { data: { n: 21 } });
    assertEquals(result, 42);
  },
});

Deno.test("[node/module stripTypeScriptTypes] strips TypeScript types", () => {
  assertEquals(
    stripTypeScriptTypes("const a: number = 1;"),
    "const a         = 1;",
  );
  assertEquals(
    stripTypeScriptTypes("const a: number = 1;", { sourceUrl: "source.ts" }),
    "const a         = 1;\n\n//# sourceURL=source.ts",
  );
});

Deno.test("[node/module stripTypeScriptTypes] transforms TypeScript syntax", () => {
  const output = stripTypeScriptTypes("enum Color { Red }\nColor.Red;", {
    mode: "transform",
  });
  assert(output.includes("Color"));
  assert(output.includes("Red"));
});

Deno.test("[node/module stripTypeScriptTypes] rejects unsupported strip-only syntax", () => {
  const err = assertThrows(
    () => stripTypeScriptTypes("enum Color { Red }"),
    SyntaxError,
  );
  assertEquals(
    (err as SyntaxError & { code?: string }).code,
    "ERR_UNSUPPORTED_TYPESCRIPT_SYNTAX",
  );
});

// denoland/deno#34868 hid `Module.register` while `register()` was a stub
// that never answered a loader's `initialize` handshake (Playwright and tsx
// probe the static). It is implemented now, so the static is back, as in Node.
Deno.test("[node/module] Module.register and Module.registerHooks are exposed", () => {
  // @ts-ignore Not in our bundled @types/node yet.
  assertEquals(Module.register, register);
  // @ts-ignore Not in our bundled @types/node yet.
  assertEquals(typeof Module.registerHooks, "function");
});

Deno.test("[node/module syncBuiltinESMExports] is exposed", () => {
  assertEquals(typeof syncBuiltinESMExports, "function");
  assertEquals(syncBuiltinESMExports(), undefined);
  assertEquals(Module.syncBuiltinESMExports, syncBuiltinESMExports);
});

Deno.test("[node/module] overriding Module._compile is possible and Node globals work", () => {
  // @ts-ignore Not documented but available
  const originalCompile = Module.prototype._compile;

  // @ts-ignore Not documented but available
  Module.prototype._compile = function (content: string, filename: string) {
    // deno-lint-ignore no-this-alias
    const mod = this;
    function require(id: string) {
      return mod.require(id);
    }
    require.resolve = function (request: string) {
      // @ts-ignore Not documented but available
      return Module._resolveFilename(request, mod);
    };
    require.main = process.mainModule;

    // Enable support to add extra extension types
    // @ts-ignore Not documented but available
    require.extensions = Module._extensions;
    // @ts-ignore Not documented but available
    require.cache = Module._cache;

    const dirname = path.dirname(filename);

    const wrapper = Module.wrap(content);
    // @ts-ignore can't index by a symbol
    const [f, err] = Deno[Deno.internal].core.evalContext(
      wrapper,
      `file:///${filename}`,
      [true],
    );
    assertEquals(err, null);
    const args = [
      mod.exports,
      require,
      mod,
      filename,
      dirname,
      process,
    ];
    f.apply(mod.exports, args);
    return mod;
  };

  const src = `module.exports = {
  clearImmediate: typeof clearImmediate,
  clearTimeout: typeof clearTimeout, 
  setImmediate: typeof setImmediate, 
  global: typeof global
};`;
  // @ts-ignore Not documented but available
  const ret = Module.prototype._compile(src, "file.js");
  const exports = ret.exports;
  assertEquals(exports.clearImmediate, "function");
  assertEquals(exports.clearTimeout, "function");
  assertEquals(exports.setImmediate, "function");
  assertEquals(exports.global, "object");
  // @ts-ignore Not documented but available
  Module.prototype._compile = originalCompile;
});

Deno.test("[node/module require] throws ERR_INVALID_ARG_TYPE for non-string id", () => {
  const require = createRequire(import.meta.url);
  const err = assertThrows(
    // @ts-expect-error testing invalid input
    () => require(123),
    TypeError,
  );
  assertEquals(
    (err as TypeError & { code?: string }).code,
    "ERR_INVALID_ARG_TYPE",
  );
});

Deno.test("[node/module require] throws ERR_INVALID_ARG_VALUE for empty string id", () => {
  const require = createRequire(import.meta.url);
  const err = assertThrows(
    () => require(""),
    TypeError,
    "must be a non-empty string",
  );
  assertEquals(
    (err as TypeError & { code?: string }).code,
    "ERR_INVALID_ARG_VALUE",
  );
});

Deno.test("[node/module require] loads .node files through CJS addon path", async () => {
  const tempDir = await Deno.makeTempDir();
  try {
    const addonPath = path.join(tempDir, "addon.node");
    await Deno.writeFile(addonPath, new Uint8Array());

    const require = createRequire(import.meta.url);
    const err = assertThrows(() => require(addonPath));
    assert(err instanceof Error);
    assert(!err.message.includes("Cannot import native Node.js addon"));
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});

Deno.test("[node/module require] loads more than 128 local TypeScript modules", async () => {
  const tempDir = await Deno.makeTempDir();
  try {
    await Promise.all(
      Array.from({ length: 200 }, (_, i) =>
        Deno.writeTextFile(
          path.join(tempDir, `mod_${i}.ts`),
          `export const value = ${i};\n`,
        )),
    );

    const require = createRequire(import.meta.url);
    for (let i = 0; i < 200; i++) {
      const mod = require(path.join(tempDir, `mod_${i}.ts`)) as {
        value: number;
      };
      assertEquals(mod.value, i);
    }
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});

Deno.test("[node/module Module._stat] default returns 0/1/<0 for file/dir/missing", async () => {
  const tmpDir = await Deno.makeTempDir();
  try {
    const file = path.join(tmpDir, "exists.js");
    const dir = path.join(tmpDir, "subdir");
    const missing = path.join(tmpDir, "missing");
    await Deno.writeTextFile(file, "module.exports = {};");
    await Deno.mkdir(dir);

    // @ts-ignore Not documented but available
    assertEquals(Module._stat(file), 0);
    // @ts-ignore Not documented but available
    assertEquals(Module._stat(dir), 1);
    // @ts-ignore Not documented but available
    assert(Module._stat(missing) < 0);
  } finally {
    await Deno.remove(tmpDir, { recursive: true });
  }
});

Deno.test("[node/module Module._stat] override is honored by require resolution", async () => {
  const tmpDir = await Deno.makeTempDir();
  try {
    const file = path.join(tmpDir, "patched-target.cjs");
    await Deno.writeTextFile(file, "module.exports = { value: 42 };");

    const require = createRequire(path.join(tmpDir, "noop.js"));

    // @ts-ignore Not documented but available
    const originalStat = Module._stat;

    // Patch _stat to claim the file does not exist; the CJS loader uses
    // the same internal stat that _stat delegates to, so require() must
    // observe the override.
    // @ts-ignore Not documented but available
    Module._stat = (filename: string) => {
      if (filename === file) return -2;
      return originalStat(filename);
    };

    try {
      const err = assertThrows(() => require(file));
      assertEquals(
        (err as Error & { code?: string }).code,
        "MODULE_NOT_FOUND",
      );
    } finally {
      // @ts-ignore Not documented but available
      Module._stat = originalStat;
    }

    // After restoration the same require() call must succeed, proving the
    // failure above was driven by the override and not the file going
    // missing.
    const mod = require(file);
    assertEquals(mod.value, 42);
  } finally {
    await Deno.remove(tmpDir, { recursive: true });
  }
});

Deno.test("[node/module] the compile cache API is exposed with Node's constants", () => {
  const mod = Module as unknown as {
    enableCompileCache: unknown;
    getCompileCacheDir: unknown;
    flushCompileCache: unknown;
    constants: { compileCacheStatus: Record<string, number> };
  };
  assertEquals(typeof mod.enableCompileCache, "function");
  assertEquals(typeof mod.getCompileCacheDir, "function");
  assertEquals(typeof mod.flushCompileCache, "function");
  assertEquals({ ...mod.constants.compileCacheStatus }, {
    FAILED: 0,
    ENABLED: 1,
    ALREADY_ENABLED: 2,
    DISABLED: 3,
  });
  assert(Object.isFrozen(mod.constants.compileCacheStatus));
  for (const invalid of [0, null, false, 1, NaN, true, Symbol(0)]) {
    assertThrows(
      () => (mod.enableCompileCache as (o: unknown) => unknown)(invalid),
      TypeError,
    );
  }
});

Deno.test("[node/module enableCompileCache] stores CommonJS code caches and reuses them", async () => {
  const tempDir = await Deno.makeTempDir();
  try {
    const cacheDir = path.join(tempDir, "cache");
    const entry = path.join(tempDir, "entry.cjs");
    await Deno.writeTextFile(
      path.join(tempDir, "dep.cjs"),
      "module.exports = function add(a, b) { return a + b; };\n",
    );
    await Deno.writeTextFile(
      entry,
      `const { enableCompileCache, getCompileCacheDir, constants } = require("node:module");
const before = getCompileCacheDir();
const result = enableCompileCache(${JSON.stringify(cacheDir)});
const add = require("./dep.cjs");
console.log(JSON.stringify({
  before: before ?? null,
  status: result.status,
  enabled: result.status === constants.compileCacheStatus.ENABLED,
  directory: result.directory,
  dir: getCompileCacheDir(),
  sum: add(2, 3),
}));
`,
    );
    const run = async () => {
      const { code, stdout, stderr } = await new Deno.Command(
        Deno.execPath(),
        {
          args: ["run", "-A", entry],
          env: { NODE_DEBUG_NATIVE: "COMPILE_CACHE" },
          stdout: "piped",
          stderr: "piped",
        },
      ).output();
      const err = new TextDecoder().decode(stderr);
      assertEquals(code, 0, err);
      return { out: JSON.parse(new TextDecoder().decode(stdout)), err };
    };

    const first = await run();
    assertEquals(first.out.before, null);
    assertEquals(first.out.enabled, true);
    assertEquals(first.out.directory, cacheDir);
    assert(first.out.dir.startsWith(cacheDir + path.sep));
    assertEquals(first.out.sum, 5);
    assert(
      /dep\.cjs was not initialized, initializing the in-memory entry/.test(
        first.err,
      ),
      first.err,
    );
    assert(/writing cache for CommonJS .*dep\.cjs.*success/.test(first.err));
    const tagged = [...Deno.readDirSync(cacheDir)];
    assertEquals(tagged.length, 1);
    const files = [...Deno.readDirSync(path.join(cacheDir, tagged[0].name))];
    assertEquals(files.length, 1);
    const bytes = Deno.readFileSync(
      path.join(cacheDir, tagged[0].name, files[0].name),
    );
    // Node's header: the magic number first, as a native-endian uint32.
    assertEquals(
      new DataView(bytes.buffer).getUint32(0, true),
      0x8adfdbb2,
    );

    const second = await run();
    assertEquals(second.out.sum, 5);
    assert(
      /cache for CommonJS .*dep\.cjs was accepted, keeping the in-memory entry/
        .test(second.err),
      second.err,
    );
    assert(/skip persisting CommonJS .*dep\.cjs because cache was the same/
      .test(second.err));

    // A changed file is a code hash mismatch and is cached again.
    await Deno.writeTextFile(
      path.join(tempDir, "dep.cjs"),
      "module.exports = function add(a, b) { return b + a; };\n",
    );
    const third = await run();
    assertEquals(third.out.sum, 5);
    assert(
      /reading cache from .* for CommonJS .*dep\.cjs.*code hash mismatch/
        .test(third.err),
      third.err,
    );
    assert(/writing cache for CommonJS .*dep\.cjs.*success/.test(third.err));
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});

Deno.test("[node/module enableCompileCache] NODE_DISABLE_COMPILE_CACHE turns it off", async () => {
  const tempDir = await Deno.makeTempDir();
  try {
    const entry = path.join(tempDir, "entry.cjs");
    await Deno.writeTextFile(
      entry,
      `const { enableCompileCache, getCompileCacheDir } = require("node:module");
const result = enableCompileCache(${JSON.stringify(path.join(tempDir, "c"))});
console.log(JSON.stringify({ result, dir: getCompileCacheDir() ?? null }));
`,
    );
    const { code, stdout, stderr } = await new Deno.Command(Deno.execPath(), {
      args: ["run", "-A", entry],
      env: { NODE_DISABLE_COMPILE_CACHE: "1" },
      stdout: "piped",
      stderr: "piped",
    }).output();
    assertEquals(code, 0, new TextDecoder().decode(stderr));
    assertEquals(JSON.parse(new TextDecoder().decode(stdout)), {
      result: { status: 3, message: "Disabled by NODE_DISABLE_COMPILE_CACHE" },
      dir: null,
    });
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});

// denext: V8 runs whatever code cache it is given once the source length and
// flags match, so the compile cache refuses a directory other users can write
// to and a cache entry that isn't a regular file the current user owns.
async function runCompileCacheEntry(
  entry: string,
  env: Record<string, string> = {},
) {
  const { code, stdout, stderr } = await new Deno.Command(Deno.execPath(), {
    args: ["run", "-A", entry],
    env: { NODE_DEBUG_NATIVE: "COMPILE_CACHE", ...env },
    stdout: "piped",
    stderr: "piped",
  }).output();
  const err = new TextDecoder().decode(stderr);
  assertEquals(code, 0, err);
  return { out: JSON.parse(new TextDecoder().decode(stdout)), err };
}

async function writeCompileCacheEntry(tempDir: string, cacheDir: string) {
  const entry = path.join(tempDir, "entry.cjs");
  await Deno.writeTextFile(
    path.join(tempDir, "dep.cjs"),
    "module.exports = function add(a, b) { return a + b; };\n",
  );
  await Deno.writeTextFile(
    entry,
    `const { enableCompileCache, getCompileCacheDir } = require("node:module");
const result = enableCompileCache(${JSON.stringify(cacheDir)});
const add = require("./dep.cjs");
console.log(JSON.stringify({
  status: result.status,
  message: result.message ?? null,
  dir: getCompileCacheDir() ?? null,
  sum: add(2, 3),
}));
`,
  );
  return entry;
}

Deno.test({
  name:
    "[node/module enableCompileCache] refuses a cache directory others can write to",
  ignore: Deno.build.os === "windows",
}, async () => {
  const tempDir = await Deno.makeTempDir();
  try {
    const cacheDir = path.join(tempDir, "cache");
    const entry = await writeCompileCacheEntry(tempDir, cacheDir);

    // A world-writable parent without the sticky bit: another user could
    // swap the version-tagged directory for their own.
    await Deno.mkdir(cacheDir);
    await Deno.chmod(cacheDir, 0o777);
    const open = await runCompileCacheEntry(entry);
    assertEquals(open.out.status, 0, open.err);
    assert(/writable by other users/.test(open.out.message), open.out.message);
    assertEquals(open.out.dir, null);
    assertEquals(open.out.sum, 5);
    assertEquals([...Deno.readDirSync(cacheDir)].length, 1);
    const tagged = path.join(cacheDir, [...Deno.readDirSync(cacheDir)][0].name);
    assertEquals([...Deno.readDirSync(tagged)].length, 0);

    // With the sticky bit (as /tmp has) it is used.
    await Deno.chmod(cacheDir, 0o1777);
    const sticky = await runCompileCacheEntry(entry);
    assertEquals(sticky.out.status, 1, sticky.err);
    assertEquals([...Deno.readDirSync(tagged)].length, 1);

    // A version-tagged directory that other users can write to is refused,
    // even with the sticky bit.
    await Deno.chmod(tagged, 0o1777);
    const taggedOpen = await runCompileCacheEntry(entry);
    assertEquals(taggedOpen.out.status, 0, taggedOpen.err);
    assert(
      /writable by other users/.test(taggedOpen.out.message),
      taggedOpen.out.message,
    );
    assert(!/reading cache from/.test(taggedOpen.err), taggedOpen.err);
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});

Deno.test({
  name:
    "[node/module enableCompileCache] doesn't load a cache entry that is a symlink",
  ignore: Deno.build.os === "windows",
}, async () => {
  const tempDir = await Deno.makeTempDir();
  try {
    const cacheDir = path.join(tempDir, "cache");
    const entry = await writeCompileCacheEntry(tempDir, cacheDir);
    const first = await runCompileCacheEntry(entry);
    assertEquals(first.out.status, 1, first.err);
    const tagged = first.out.dir as string;
    const [file] = [...Deno.readDirSync(tagged)];
    const cacheFile = path.join(tagged, file.name);
    const elsewhere = path.join(tempDir, "planted");
    await Deno.rename(cacheFile, elsewhere);
    await Deno.symlink(elsewhere, cacheFile);

    const second = await runCompileCacheEntry(entry);
    assertEquals(second.out.sum, 5);
    assert(
      /not a regular file owned by the current user/.test(second.err),
      second.err,
    );
    assert(!/was accepted/.test(second.err), second.err);
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});

Deno.test(
  "[node/module enableCompileCache] the cache directory is created for the current user only",
  {
    ignore: Deno.build.os === "windows",
  },
  async () => {
    const tempDir = await Deno.makeTempDir();
    try {
      const cacheDir = path.join(tempDir, "cache");
      const entry = await writeCompileCacheEntry(tempDir, cacheDir);
      const { out, err } = await runCompileCacheEntry(entry);
      assertEquals(out.status, 1, err);
      assertEquals(Deno.statSync(out.dir).mode! & 0o077, 0);
    } finally {
      await Deno.remove(tempDir, { recursive: true });
    }
  },
);
