// Copyright 2018-2026 the Deno authors. MIT license.

// Maps a Jest config (jest.config.*, a package.json "jest" key, or Jest's
// defaults) onto the project shape config_loader.js produces for a Vitest
// config, so a Jest project runs on the same layer:
//
// - `roots` + `testMatch` / `testRegex` + `testPathIgnorePatterns` +
//   `moduleFileExtensions` select the test files (matched by the CLI, see
//   `Project::matches_jest` in cli/tools/test/vitest/mod.rs);
// - `moduleNameMapper` becomes regex aliases, `moduleDirectories` /
//   `modulePaths` extra lookup directories for bare specifiers;
// - `setupFiles` then `setupFilesAfterEnv` run as setup files;
//   `testEnvironment` (+ options), `testTimeout`, `clearMocks` /
//   `resetMocks` / `restoreMocks`, `injectGlobals`, `globals`,
//   `snapshotSerializers`, `snapshotFormat` and `fakeTimers` configure the
//   runtime (see js/jest.js);
// - a `transform` deno test cannot honour (anything but the usual
//   TypeScript/JavaScript transformers, which Deno replaces) is a warning.

import {
  basename,
  dirname,
  extname,
  isAbsolute,
  join,
  resolve,
} from "node:path";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";

/** Jest 30's defaults. */
export const JEST_DEFAULTS = {
  testMatch: [
    "**/__tests__/**/*.?([mc])[jt]s?(x)",
    "**/?(*.)+(spec|test).?([mc])[jt]s?(x)",
  ],
  testPathIgnorePatterns: ["/node_modules/"],
  moduleFileExtensions: [
    "js",
    "mjs",
    "cjs",
    "jsx",
    "ts",
    "mts",
    "cts",
    "tsx",
    "json",
    "node",
  ],
  moduleDirectories: ["node_modules"],
  testEnvironment: "node",
};

/** Transformers whose job (TypeScript / JSX / ESM to CJS) Deno does itself. */
const CODE_TRANSFORMERS = [
  /^babel-jest$/,
  /^ts-jest(\/.*)?$/,
  /^@swc\/jest$/,
  /^esbuild-jest$/,
  /^jest-esbuild$/,
  /^@sucrase\/jest-plugin$/,
  /^sucrase\/jest$/,
  /^jest-ts-webcompat-resolver$/,
  /^@esbuild-kit\/jest$/,
  /^esbuild-runner\/jest$/,
];

const CONFIG_EXTENSIONS = [
  ".ts",
  ".mts",
  ".cts",
  ".js",
  ".mjs",
  ".cjs",
  ".json",
];

// tsconfig.json is JSONC: drop comments and trailing commas.
export function parseJsonc(text) {
  let out = "";
  let inString = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (inString) {
      out += c;
      if (c === "\\") out += text[++i] ?? "";
      else if (c === '"') inString = false;
    } else if (c === '"') {
      inString = true;
      out += c;
    } else if (c === "/" && text[i + 1] === "/") {
      while (i < text.length && text[i] !== "\n") i++;
      out += "\n";
    } else if (c === "/" && text[i + 1] === "*") {
      i += 2;
      while (i < text.length && !(text[i] === "*" && text[i + 1] === "/")) i++;
      i++;
    } else {
      out += c;
    }
  }
  return JSON.parse(out.replace(/,(\s*[}\]])/g, "$1"));
}

function toArray(value) {
  if (value === undefined || value === null) return [];
  return Array.isArray(value) ? value : [value];
}

/** Jest's `replaceRootDirInPath`. */
function replaceRootDir(rootDir, value) {
  if (typeof value !== "string") return value;
  if (!value.startsWith("<rootDir>")) return value;
  return resolve(rootDir, "./" + value.slice("<rootDir>".length));
}

/**
 * `<rootDir>` inside a regex or glob pattern. The CLI matches patterns against
 * slash paths, so on Windows the root is spelled with forward slashes (a
 * backslash would be an escape in either syntax).
 */
function replaceRootDirInPattern(rootDir, pattern) {
  return String(pattern).replaceAll(
    "<rootDir>",
    rootDir.replaceAll("\\", "/"),
  );
}

async function requireOrImport(path) {
  const ext = extname(path);
  if (ext === ".json") return JSON.parse(readFileSync(path, "utf8"));
  if (ext === ".cjs" || ext === ".cts") {
    return createRequire(path)(path);
  }
  try {
    const mod = await import(pathToFileURL(path).href);
    return mod.default ?? mod;
  } catch (error) {
    // a CommonJS file deno did not detect as such
    try {
      return createRequire(path)(path);
    } catch {
      throw error;
    }
  }
}

/** Reads a Jest config file (or the `jest` key of a package.json). */
export async function readJestConfig(path) {
  let config;
  if (basename(path) === "package.json") {
    config = JSON.parse(readFileSync(path, "utf8")).jest ?? {};
  } else {
    config = await requireOrImport(path);
    if (config?.__esModule && "default" in config) config = config.default;
  }
  if (typeof config === "function") config = await config();
  config = await config;
  if (config === null || typeof config !== "object") {
    throw new TypeError(`${path} must export a Jest config object`);
  }
  return config;
}

/** Jest's `setupFromPreset`. */
async function applyPreset(options, rootDir, warnings) {
  const name = options.preset;
  if (typeof name !== "string") return options;
  const { preset: _, ...rest } = options;
  // ts-jest's presets configure its transform, which Deno replaces (its
  // tsconfig still decides the JSX runtime).
  if (/^ts-jest(\/|$)/.test(name)) {
    return {
      ...rest,
      transform: { "^.+\\.[tj]sx?$": "ts-jest", ...(rest.transform ?? {}) },
    };
  }
  let preset;
  try {
    const base = replaceRootDir(rootDir, name);
    const require = createRequire(join(rootDir, "__deno_jest_preset__.js"));
    let path;
    if (base.startsWith(".") || isAbsolute(base)) {
      const file = resolve(rootDir, base);
      path = existsSync(file) && statSync(file).isFile()
        ? file
        : require.resolve(join(file, "jest-preset"));
    } else {
      path = require.resolve(`${base}/jest-preset`);
    }
    preset = await requireOrImport(path);
  } catch (error) {
    warnings.push(
      `Jest preset "${name}" could not be loaded (${
        error?.message ?? error
      }); its options are ignored.`,
    );
    return rest;
  }
  const merged = { ...preset, ...rest };
  for (const key of ["setupFiles", "setupFilesAfterEnv"]) {
    if (preset[key] || rest[key]) {
      merged[key] = [...toArray(preset[key]), ...toArray(rest[key])];
    }
  }
  for (const key of ["moduleNameMapper", "transform", "globals"]) {
    if (preset[key] && rest[key]) {
      // the config's entries come first: they are tried in order
      merged[key] = { ...rest[key], ...preset[key], ...rest[key] };
    }
  }
  return merged;
}

/** A `testMatch` glob in the CLI's (picomatch-style) syntax. */
export function normalizeTestMatch(rootDir, pattern) {
  let glob = replaceRootDirInPattern(rootDir, pattern);
  // micromatch treats a bare `(a|b)` as a group, like `@(a|b)`
  glob = glob.replace(/(^|[^?*+@!\\])\((?=[^)]*\|)/g, "$1@(");
  return glob.startsWith("!") ? undefined : glob;
}

function testMatchExclusions(rootDir, patterns) {
  return toArray(patterns)
    .filter((p) => typeof p === "string" && p.startsWith("!"))
    .map((p) => replaceRootDirInPattern(rootDir, p.slice(1)));
}

function resolveModulePath(rootDir, value) {
  const replaced = replaceRootDir(rootDir, value);
  if (replaced.startsWith(".") || isAbsolute(replaced)) {
    return resolve(rootDir, replaced);
  }
  try {
    return createRequire(join(rootDir, "__deno_jest_setup__.js")).resolve(
      replaced,
    );
  } catch {
    return resolve(rootDir, replaced);
  }
}

/** Maps a `testEnvironment` to a Vitest environment. */
export function mapEnvironment(name, warnings) {
  if (name === undefined || name === null || name === "") return "node";
  const value = String(name);
  const base = basename(value).replace(/\.[cm]?[jt]s$/, "");
  if (base === "node" || base === "jest-environment-node") return "node";
  if (/happy-?dom/i.test(value)) return "happy-dom";
  if (/jsdom/i.test(value)) return "jsdom";
  warnings?.push(
    `Jest test environment "${value}" is not supported by deno test; using "node".`,
  );
  return "node";
}

/**
 * Where a DOM environment's package (jsdom, happy-dom) resolves from when
 * the project does not depend on it directly (a strict pnpm layout): from
 * the Jest environment package that does, as Jest loads it. Returns a file
 * path to resolve the package from, or undefined when the root resolves it.
 */
function findPackageBase(rootDir, target, via) {
  const resolvesFrom = (base) => {
    try {
      createRequire(base).resolve(target);
      return true;
    } catch {
      return false;
    }
  };
  const rootBase = join(rootDir, "__deno_jest_root__.js");
  if (resolvesFrom(rootBase)) return undefined;
  let bases = [rootBase];
  for (let depth = 0; depth < 3; depth++) {
    const next = [];
    for (const base of bases) {
      for (const name of via) {
        let found;
        try {
          found = createRequire(base).resolve(name);
        } catch {
          continue;
        }
        if (resolvesFrom(found)) return found;
        next.push(found);
      }
    }
    if (next.length === 0) break;
    bases = next;
  }
  return undefined;
}

/** The DOM packages' bases (see findPackageBase) for a project. */
function environmentPackageBases(rootDir, testEnvironment) {
  const bases = {};
  const environment = typeof testEnvironment === "string"
    ? [replaceRootDir(rootDir, testEnvironment)]
    : [];
  const jsdom = findPackageBase(rootDir, "jsdom", [
    ...environment,
    "jest-environment-jsdom",
  ]);
  if (jsdom) bases.jsdom = jsdom;
  const happyDom = findPackageBase(rootDir, "happy-dom", [
    ...environment,
    "@happy-dom/jest-environment",
  ]);
  if (happyDom) bases["happy-dom"] = happyDom;
  return bases;
}

function mapModuleNameMapper(rootDir, mapper) {
  const aliases = [];
  for (const [regex, mapped] of Object.entries(mapper ?? {})) {
    const candidates = toArray(mapped).map((m) => replaceRootDir(rootDir, m));
    // Jest tries each mapped path in turn; take the first that exists when
    // it is a fixed path.
    const replacement = candidates.find((c) =>
      !isAbsolute(c) || c.includes("$") || existsSync(c)
    ) ?? candidates[0];
    if (replacement === undefined) {
      continue;
    }
    aliases.push({
      find: { regex: String(regex), flags: "" },
      replacement: String(replacement),
      whole: true,
      // Jest maps every module's imports, node_modules' included
      deps: true,
    });
  }
  return aliases;
}

function transformWarnings(transform) {
  const ignored = [];
  for (const [pattern, value] of Object.entries(transform ?? {})) {
    const name = String(Array.isArray(value) ? value[0] : value);
    if (CODE_TRANSFORMERS.some((re) => re.test(name))) continue;
    ignored.push(`"${pattern}": ${name}`);
  }
  if (ignored.length === 0) return [];
  return [
    `deno test does not apply these Jest transforms (it transpiles TypeScript and JSX itself and loads CSS and assets as Vite does): ${
      ignored.join(", ")
    }`,
  ];
}

function readJsonc(path) {
  try {
    return parseJsonc(readFileSync(path, "utf8"));
  } catch {
    return undefined;
  }
}

function automaticJsx(importSource, development = false) {
  return { importSource: importSource || "react", development };
}

/** The JSX runtime of swc options: undefined when they do not say. */
function swcJsx(options) {
  const react = options?.jsc?.transform?.react;
  if (!react?.runtime) return undefined;
  return react.runtime === "automatic"
    ? automaticJsx(react.importSource, !!react.development)
    : null;
}

/** The JSX runtime a tsconfig asks for (following relative `extends`). */
function tsconfigJsx(path, seen = new Set()) {
  if (seen.has(path)) return undefined;
  seen.add(path);
  const tsconfig = readJsonc(path);
  if (!tsconfig) return undefined;
  const options = tsconfig.compilerOptions ?? {};
  if (options.jsx) {
    return /^react-jsx(dev)?$/i.test(options.jsx)
      ? automaticJsx(options.jsxImportSource, /dev$/i.test(options.jsx))
      : null;
  }
  for (const parent of toArray(tsconfig.extends)) {
    if (typeof parent !== "string" || !parent.startsWith(".")) continue;
    let parentPath = resolve(dirname(path), parent);
    if (!parentPath.endsWith(".json")) parentPath += ".json";
    const found = tsconfigJsx(parentPath, seen);
    if (found !== undefined) return found;
  }
  return undefined;
}

const BABEL_CONFIGS = [
  "babel.config.json",
  "babel.config.js",
  "babel.config.cjs",
  "babel.config.mjs",
  "babel.config.cts",
  "babel.config.ts",
  ".babelrc",
  ".babelrc.json",
  ".babelrc.js",
  ".babelrc.cjs",
  ".babelrc.mjs",
];

/**
 * The JSX runtime of the project's Babel config, read from its text (its
 * presets are not evaluated): `runtime: "automatic"` (preset-react) or a
 * preset that defaults to it (next/babel, babel-preset-expo).
 */
function babelJsx(rootDir) {
  const texts = [];
  for (const name of BABEL_CONFIGS) {
    const path = join(rootDir, name);
    if (existsSync(path)) texts.push(readFileSync(path, "utf8"));
  }
  const packageJson = readJsonc(join(rootDir, "package.json"));
  if (packageJson?.babel) texts.push(JSON.stringify(packageJson.babel));
  for (const text of texts) {
    if (
      /runtime['"]?\s*:\s*['"]automatic['"]/.test(text) ||
      /next\/babel|babel-preset-expo/.test(text)
    ) {
      const importSource = /importSource['"]?\s*:\s*['"]([^'"]+)['"]/.exec(
        text,
      )?.[1];
      return automaticJsx(importSource);
    }
  }
  return undefined;
}

/** Transformers that compile JSX in the `.js` files they are given. */
const JSX_IN_JS_TRANSFORMERS =
  /^(babel-jest|@swc\/jest|esbuild-jest|jest-esbuild|@sucrase\/jest|sucrase-jest)$/;

/**
 * How the project's `.js` files get JSX: babel-jest (Jest's default
 * transform) and the other JSX-capable transforms parse it there ("always");
 * otherwise only a file that fails to parse as plain JavaScript is parsed as
 * JSX ("fallback").
 */
function jestJsxInJs(rootDir, transform) {
  const entries = Object.entries(transform ?? {});
  if (entries.length === 0) return { root: rootDir, mode: "always" };
  for (const [pattern, value] of entries) {
    let regex;
    try {
      regex = new RegExp(pattern);
    } catch {
      continue;
    }
    // Jest uses the first transform whose pattern matches the file
    if (!regex.test("/project/file.js")) continue;
    const name = Array.isArray(value) ? value[0] : value;
    return {
      root: rootDir,
      mode: JSX_IN_JS_TRANSFORMERS.test(String(name)) ? "always" : "fallback",
    };
  }
  return { root: rootDir, mode: "fallback" };
}

/**
 * The automatic JSX runtime the project's Jest transform compiles `.tsx` /
 * `.jsx` files with (babel-jest, `@swc/jest`, ts-jest), if it uses one: Deno
 * then compiles JSX the same way instead of with its classic default.
 */
function transformJsx(rootDir, transform) {
  const entries = Object.entries(transform ?? {});
  if (entries.length === 0) return babelJsx(rootDir) ?? undefined;
  for (const [pattern, value] of entries) {
    let regex;
    try {
      regex = new RegExp(pattern);
    } catch {
      continue;
    }
    if (!regex.test("/project/file.tsx") && !regex.test("/project/file.jsx")) {
      continue;
    }
    const [name, options] = Array.isArray(value) ? value : [value, undefined];
    let found;
    if (/^@swc\/jest$/.test(name)) {
      found = swcJsx(options) ?? swcJsx(readJsonc(join(rootDir, ".swcrc")));
    } else if (/^ts-jest(\/|$)/.test(name)) {
      const tsconfig = options?.tsconfig;
      if (tsconfig && typeof tsconfig === "object") {
        const jsx = tsconfig.jsx;
        found = jsx
          ? (/^react-jsx(dev)?$/i.test(jsx)
            ? automaticJsx(tsconfig.jsxImportSource, /dev$/i.test(jsx))
            : null)
          : undefined;
      } else {
        found = tsconfigJsx(
          typeof tsconfig === "string"
            ? resolve(rootDir, replaceRootDir(rootDir, tsconfig))
            : join(rootDir, "tsconfig.json"),
        );
      }
    } else if (/^babel-jest$/.test(name)) {
      found = babelJsx(rootDir);
    }
    if (found !== undefined) return found ?? undefined;
  }
  return undefined;
}

/** The header Jest writes in a new snapshot file (it changed in Jest 30). */
function snapshotHeader(rootDir) {
  let major = 30;
  try {
    const require = createRequire(join(rootDir, "__deno_jest_root__.js"));
    const { version } = require("jest/package.json");
    major = Number.parseInt(String(version), 10);
  } catch {
    // Jest is not installed: the current header
  }
  return major >= 30
    ? "// Jest Snapshot v1, https://jestjs.io/docs/snapshot-testing"
    : "// Jest Snapshot v1, https://goo.gl/fbAQLP";
}

/** Jest's `fakeTimers` (or the older `timers`) option. */
function fakeTimersOption(options) {
  if (options.fakeTimers && typeof options.fakeTimers === "object") {
    return options.fakeTimers.enableGlobally ? options.fakeTimers : undefined;
  }
  if (options.timers === "fake" || options.timers === "modern") {
    return { enableGlobally: true };
  }
  if (options.timers === "legacy") {
    return { enableGlobally: true, legacyFakeTimers: true };
  }
  return undefined;
}

/** The `fakeTimers` options every `jest.useFakeTimers()` call starts from. */
function fakeTimersDefaults(options) {
  if (options.fakeTimers && typeof options.fakeTimers === "object") {
    return options.fakeTimers;
  }
  return options.timers === "legacy" ? { legacyFakeTimers: true } : {};
}

const UNSUPPORTED_OPTIONS = {
  automock: (v) => v === true,
  resetModules: (v) => v === true,
  runner: (v) => typeof v === "string" && v !== "jest-runner",
  testRunner: (v) =>
    typeof v === "string" && !/jest-circus|jest-jasmine2/.test(v),
  moduleLoader: (v) => typeof v === "string",
  resolver: (v) => typeof v === "string",
};

/** Normalizes one Jest project config into the shape config_loader.js uses. */
export async function resolveJestProject(rawOptions, configDir, fallbackName) {
  const warnings = [];
  const rootDir = resolve(configDir, rawOptions.rootDir ?? ".");
  const options = await applyPreset(rawOptions, rootDir, warnings);
  for (const [key, unsupported] of Object.entries(UNSUPPORTED_OPTIONS)) {
    if (unsupported(options[key])) {
      warnings.push(
        `the Jest option ${key}: ${
          JSON.stringify(options[key])
        } is not supported by deno test and is ignored.`,
      );
    }
  }
  const roots = toArray(options.roots ?? ["<rootDir>"]).map((r) =>
    resolve(rootDir, replaceRootDir(rootDir, r))
  );
  const testRegex = toArray(options.testRegex).map(String);
  const testMatch = testRegex.length
    ? []
    : toArray(options.testMatch ?? JEST_DEFAULTS.testMatch);
  const displayName = options.displayName;
  const name = typeof displayName === "string"
    ? displayName
    : displayName?.name ?? fallbackName;
  const moduleDirectories = [
    ...toArray(options.moduleDirectories ?? JEST_DEFAULTS.moduleDirectories)
      .filter((d) => d !== "node_modules")
      .map((d) => replaceRootDir(rootDir, d)),
    ...toArray(options.modulePaths).map((p) =>
      resolve(rootDir, replaceRootDir(rootDir, p))
    ),
  ];
  const environmentOptions = { ...(options.testEnvironmentOptions ?? {}) };
  delete environmentOptions.customExportConditions;
  const packageBases = environmentPackageBases(
    rootDir,
    options.testEnvironment,
  );
  const setupFiles = [
    ...toArray(options.setupFiles),
    ...toArray(options.setupFilesAfterEnv),
  ].map((f) => resolveModulePath(rootDir, f));
  const moduleNameMapper = mapModuleNameMapper(
    rootDir,
    options.moduleNameMapper,
  );
  const config = {
    // a project's displayName shows with its tests when several projects run
    label: typeof displayName === "string" ? displayName : displayName?.name,
    root: rootDir,
    mode: "test",
    base: "/",
    env: {},
    define: {},
    flavor: "jest",
    globals: options.injectGlobals !== false,
    environment: mapEnvironment(
      options.testEnvironment ?? JEST_DEFAULTS.testEnvironment,
      warnings,
    ),
    environmentOptions: {
      jsdom: environmentOptions,
      happyDOM: environmentOptions,
    },
    setupFiles,
    clearMocks: options.clearMocks ?? false,
    mockReset: options.resetMocks ?? false,
    restoreMocks: options.restoreMocks ?? false,
    jestGlobals: options.globals ?? {},
    snapshotSerializers: toArray(options.snapshotSerializers).map((s) =>
      resolveModulePath(rootDir, s)
    ),
    mockRoots: roots,
    // for the `require()` calls of CommonJS dependencies (see jest.js)
    moduleNameMapper,
    jestFakeTimers: fakeTimersOption(options),
    jestFakeTimersConfig: fakeTimersDefaults(options),
    packageBases,
    snapshotHeader: snapshotHeader(rootDir),
  };
  if (typeof options.testTimeout === "number") {
    config.testTimeout = options.testTimeout;
  }
  if (options.snapshotFormat) config.snapshotFormat = options.snapshotFormat;
  if (options.maxConcurrency) config.maxConcurrency = options.maxConcurrency;
  warnings.push(...transformWarnings(options.transform));
  return {
    name: String(name),
    root: rootDir,
    include: testMatch
      .map((p) => normalizeTestMatch(rootDir, p))
      .filter((p) => p !== undefined),
    exclude: testMatchExclusions(rootDir, testMatch),
    alias: moduleNameMapper,
    moduleDirectories,
    packageBases,
    jsx: transformJsx(rootDir, options.transform),
    jsxInJs: jestJsxInJs(rootDir, options.transform),
    jest: {
      roots,
      testRegex: testRegex.map((r) => replaceRootDirInPattern(rootDir, r)),
      ignore: toArray(
        options.testPathIgnorePatterns ?? JEST_DEFAULTS.testPathIgnorePatterns,
      ).map((p) => replaceRootDirInPattern(rootDir, p)),
      extensions: toArray(
        options.moduleFileExtensions ?? JEST_DEFAULTS.moduleFileExtensions,
      ),
    },
    warnings,
    config,
  };
}

/** The Jest config in a directory named by a `projects` entry. */
function findConfigIn(dir) {
  for (const ext of CONFIG_EXTENSIONS) {
    const path = join(dir, `jest.config${ext}`);
    if (existsSync(path)) return path;
  }
  const packageJson = join(dir, "package.json");
  return existsSync(packageJson) ? packageJson : undefined;
}

/** Expands a `projects` path entry (`<rootDir>/packages/*`). */
function expandProjectPath(rootDir, entry) {
  const path = resolve(rootDir, replaceRootDir(rootDir, entry));
  if (!path.includes("*")) return [path];
  const star = path.indexOf("*");
  const parent = dirname(path.slice(0, star + 1));
  const pattern = new RegExp(
    "^" + path.slice(parent.length + 1).split("/")[0]
      .replace(/[.+?^${}()|[\]\\]/g, "\\$&").replaceAll("*", ".*") +
      "$",
  );
  if (!existsSync(parent)) return [];
  return readdirSync(parent)
    .filter((name) => pattern.test(name))
    .map((name) => join(parent, name))
    .filter((p) => statSync(p).isDirectory())
    .sort();
}

/** Every project of a Jest config. */
export async function loadJestProjects(configPath) {
  const config = await readJestConfig(configPath);
  const configDir = dirname(configPath);
  const rootDir = resolve(configDir, config.rootDir ?? ".");
  const entries = toArray(config.projects);
  if (entries.length === 0) {
    return [await resolveJestProject(config, configDir, "")];
  }
  const projects = [];
  for (const entry of entries) {
    if (typeof entry === "string") {
      for (const path of expandProjectPath(rootDir, entry)) {
        const isFile = existsSync(path) && statSync(path).isFile();
        const file = isFile ? path : findConfigIn(path);
        const projectConfig = file ? await readJestConfig(file) : {};
        const dir = isFile ? dirname(path) : path;
        projects.push(
          await resolveJestProject(projectConfig, dir, basename(dir)),
        );
      }
      continue;
    }
    // An inline project's own relative rootDir is relative to the working
    // directory (Jest normalizes it without a config path).
    const projectRootDir = typeof entry?.rootDir === "string"
      ? resolve(process.cwd(), entry.rootDir)
      : rootDir;
    projects.push(
      await resolveJestProject(
        { ...entry, rootDir: projectRootDir },
        configDir,
        String(projects.length),
      ),
    );
  }
  return projects;
}
