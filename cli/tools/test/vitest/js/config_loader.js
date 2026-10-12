// Copyright 2018-2026 the Deno authors. MIT license.

// Evaluates a vitest.config.* / vite.config.* file (or, with `flavor=jest`, a
// Jest config, see jest_config.js) and reduces it to what
// `deno test` needs: one entry per test project with its file globs, aliases
// and the runtime options the per-file preload hands to Vitest's runner.
//
// Served as `deno-vitest:///config_loader.js?config=<file url>`; the result is
// stored as JSON under `globalThis[Symbol.for("deno.vitest.loadedConfig")]`.

import { dirname, isAbsolute, resolve } from "node:path";
import * as nodeFs from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import { loadEnv, mergeConfig } from "./config.js";
import { b as defaultInclude, d as defaultExclude } from "./vendor/defaults.js";
import { loadJestProjects, parseJsonc } from "./jest_config.js";

const RESULT = Symbol.for("deno.vitest.loadedConfig");

// The runner's own reads (see cli/tools/test/vitest/runner_reads.rs): the
// config files it imports and the files it reduces to options (tsconfig,
// `.env`, Jest presets and Babel configs) need no read permission within
// the run's root. Taken before the config's code runs, and only handed to
// the functions doing those reads: what the config's code itself does at
// runtime stays checked. `node:fs` when not available.
const fs = globalThis.Deno[globalThis.Deno.internal].takeVitestRunnerFs?.() ??
  nodeFs;

const params = new URL(import.meta.url).searchParams;
const configUrl = params.get("config");
const flavor = params.get("flavor") ?? "vitest";
const configPath = fileURLToPath(configUrl);

/** Mirrors Vite's `ConfigEnv` for `vitest run`. */
const configEnv = {
  command: "serve",
  mode: "test",
  isSsrBuild: false,
  isPreview: false,
};

async function evaluateConfigFile(path) {
  const url = pathToFileURL(path).href;
  fs.allowImport?.(url);
  const mod = await import(url);
  let config = mod.default ?? mod;
  if (typeof config === "function") config = config(configEnv);
  config = await config;
  if (config === null || typeof config !== "object") {
    throw new TypeError(`${path} must export a config object`);
  }
  return config;
}

function normalizeAliases(alias) {
  if (!alias) return [];
  const entries = Array.isArray(alias)
    ? alias
    : Object.entries(alias).map(([find, replacement]) => ({
      find,
      replacement,
    }));
  return entries.map(({ find, replacement }) => ({
    find: find instanceof RegExp
      ? { regex: find.source, flags: find.flags }
      : String(find),
    replacement: String(replacement),
  }));
}

/** `compilerOptions.paths` of a tsconfig, following `extends`. */
function readTsconfigPaths(path, seen = new Set()) {
  if (seen.has(path) || !fs.existsSync(path)) return undefined;
  seen.add(path);
  const tsconfig = parseJsonc(fs.readFileSync(path, "utf8"));
  const options = tsconfig.compilerOptions ?? {};
  if (options.paths) {
    const base = resolve(dirname(path), options.baseUrl ?? ".");
    return { paths: options.paths, base };
  }
  for (const parent of toArray(tsconfig.extends)) {
    if (typeof parent !== "string" || !parent.startsWith(".")) continue;
    let parentPath = resolve(dirname(path), parent);
    if (!parentPath.endsWith(".json")) parentPath += ".json";
    const found = readTsconfigPaths(parentPath, seen);
    if (found) return found;
  }
  return undefined;
}

function escapeRegExp(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Vite's `resolve.tsconfigPaths` (and the vite-tsconfig-paths plugin):
 * tsconfig `paths` become aliases.
 */
function tsconfigPathAliases(config, root) {
  const plugins = (config.plugins ?? []).flat(Infinity);
  const enabled = config.resolve?.tsconfigPaths === true ||
    plugins.some((p) =>
      p && typeof p === "object" &&
      /tsconfig-paths/.test(String(p.name ?? ""))
    );
  if (!enabled) return [];
  const found = readTsconfigPaths(resolve(root, "tsconfig.json"));
  if (!found) return [];
  const aliases = [];
  for (const [pattern, targets] of Object.entries(found.paths)) {
    const target = toArray(targets)[0];
    if (typeof target !== "string") continue;
    const star = pattern.indexOf("*");
    if (star === -1) {
      aliases.push({
        find: { regex: `^${escapeRegExp(pattern)}$`, flags: "" },
        replacement: resolve(found.base, target),
      });
    } else {
      const prefix = escapeRegExp(pattern.slice(0, star));
      const suffix = escapeRegExp(pattern.slice(star + 1));
      const [before, after = ""] = target.split("*");
      aliases.push({
        find: { regex: `^${prefix}(.*)${suffix}$`, flags: "" },
        replacement: resolve(found.base, before) +
          (before.endsWith("/") ? "/" : "") +
          "$1" + after,
      });
    }
  }
  return aliases;
}

function toArray(value) {
  if (value === undefined || value === null) return [];
  return Array.isArray(value) ? value : [value];
}

function resolvePaths(root, paths) {
  return toArray(paths).map((p) =>
    p.startsWith("file:")
      ? fileURLToPath(p)
      : isAbsolute(p)
      ? p
      : resolve(root, p)
  );
}

/** Keeps the JSON-safe options Vitest's runtime reads. */
function runtimeOptions(test, root) {
  const pick = {};
  for (
    const key of [
      "globals",
      "environment",
      "environmentOptions",
      "testTimeout",
      "hookTimeout",
      "teardownTimeout",
      "retry",
      "maxConcurrency",
      "clearMocks",
      "mockReset",
      "restoreMocks",
      "unstubGlobals",
      "unstubEnvs",
      "allowOnly",
      "passWithNoTests",
      "fakeTimers",
      "snapshotFormat",
      "expandSnapshotDiff",
      "taskTitleValueFormatTruncate",
      "strictTags",
    ]
  ) {
    if (test[key] !== undefined) pick[key] = test[key];
  }
  if (test.sequence) {
    pick.sequence = {
      concurrent: test.sequence.concurrent,
      hooks: test.sequence.hooks,
      setupFiles: test.sequence.setupFiles,
      seed: test.sequence.seed,
    };
  }
  if (test.expect) {
    pick.expect = {
      requireAssertions: test.expect.requireAssertions,
      poll: test.expect.poll,
    };
  }
  if (test.tags) pick.tags = test.tags;
  if (typeof test.diff === "object" && test.diff !== null) {
    pick.diff = test.diff;
  }
  pick.setupFiles = resolvePaths(root, test.setupFiles);
  return pick;
}

/**
 * Vite's JSX-in-`.js` setup: `esbuild: { loader: "jsx", include }` (or
 * `oxc: { lang: "jsx", include }`) makes the transform parse the included
 * files as JSX. `include` / `exclude` are Vite filters (regexes, or globs
 * resolved against the root); the default include has no `.js`.
 */
function viteJsxInJs(config, root) {
  const options = config.esbuild?.loader === "jsx"
    ? config.esbuild
    : config.oxc?.lang === "jsx"
    ? config.oxc
    : undefined;
  if (!options || typeof options !== "object") return undefined;
  const filters = (value) =>
    toArray(value).map((f) =>
      f instanceof RegExp
        ? { regex: f.source, flags: f.flags }
        : String(f).startsWith("**") || isAbsolute(String(f))
        ? String(f)
        : resolve(root, String(f))
    );
  return {
    root,
    mode: "always",
    include: filters(options.include ?? /\.(m?ts|[jt]sx)$/),
    exclude: filters(options.exclude),
  };
}

/**
 * The dependencies Vitest inlines (`server.deps.inline`, formerly
 * `deps.inline`): Vite's aliases apply to their imports too.
 */
function inlinedDeps(test) {
  const inline = test.server?.deps?.inline ?? test.deps?.inline;
  if (inline === true) return true;
  if (!Array.isArray(inline)) return undefined;
  return inline.map((entry) =>
    entry instanceof RegExp
      ? { regex: entry.source, flags: entry.flags }
      : String(entry)
  );
}

function resolveProject(config, configDir, fallbackName) {
  const root = resolve(configDir, config.root ?? ".");
  const test = config.test ?? {};
  const projectRoot = test.root ? resolve(root, test.root) : root;
  const dir = test.dir ? resolve(projectRoot, test.dir) : undefined;
  const mode = config.mode ?? "test";
  const envDir = config.envDir ? resolve(root, config.envDir) : root;
  const env = {
    ...loadEnv(mode, envDir, config.envPrefix ?? "VITE_", fs),
    ...(test.env ?? {}),
  };
  const define = {};
  for (const [key, value] of Object.entries(config.define ?? {})) {
    define[key] = typeof value === "string" ? value : JSON.stringify(value);
  }
  return {
    name: test.name?.label ?? test.name ?? fallbackName,
    root: projectRoot,
    dir,
    include: toArray(test.include ?? defaultInclude),
    exclude: toArray(test.exclude ?? defaultExclude),
    jsxInJs: viteJsxInJs(config, root),
    alias: [
      ...normalizeAliases(test.alias),
      ...normalizeAliases(config.resolve?.alias),
      ...tsconfigPathAliases(config, root),
    ].map((alias) => ({ ...alias, deps: inlinedDeps(test) })),
    config: {
      // a named project's tests show its name when several projects run
      label: test.name?.label ?? test.name ?? undefined,
      root: projectRoot,
      mode,
      base: config.base ?? "/",
      env,
      define,
      ...runtimeOptions(test, projectRoot),
    },
  };
}

function withoutProjects(config) {
  const test = { ...(config.test ?? {}) };
  delete test.projects;
  delete test.workspace;
  return { ...config, test };
}

async function expandProjects(rootConfig, configDir) {
  const test = rootConfig.test ?? {};
  const entries = toArray(test.projects ?? test.workspace);
  if (entries.length === 0) {
    return [resolveProject(rootConfig, configDir, "")];
  }
  const base = withoutProjects(rootConfig);
  const projects = [];
  for (let entry of entries) {
    if (typeof entry === "function") entry = await entry(configEnv);
    entry = await entry;
    if (typeof entry === "string") {
      const path = resolve(configDir, entry);
      const projectConfig = await evaluateConfigFile(path);
      projects.push(
        resolveProject(projectConfig, dirname(path), dirname(path)),
      );
      continue;
    }
    const { extends: extend, ...projectConfig } = entry ?? {};
    let config = projectConfig;
    if (extend === true) {
      config = mergeConfig(base, projectConfig);
    } else if (typeof extend === "string") {
      const parentPath = resolve(configDir, extend);
      config = mergeConfig(await evaluateConfigFile(parentPath), projectConfig);
    }
    projects.push(resolveProject(config, configDir, String(projects.length)));
  }
  return projects;
}

let projects;
if (flavor === "jest") {
  projects = await loadJestProjects(configPath, fs);
} else {
  const rootConfig = await evaluateConfigFile(configPath);
  projects = await expandProjects(rootConfig, dirname(configPath));
}
globalThis[RESULT] = JSON.stringify({ configPath, projects });
