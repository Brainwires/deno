// Copyright 2018-2026 the Deno authors. MIT license.

// Evaluates a vitest.config.* / vite.config.* file and reduces it to what
// `deno test` needs: one entry per test project with its file globs, aliases
// and the runtime options the per-file preload hands to Vitest's runner.
//
// Served as `deno-vitest:///config_loader.js?config=<file url>`; the result is
// stored as JSON under `globalThis[Symbol.for("deno.vitest.loadedConfig")]`.

import { dirname, isAbsolute, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { loadEnv, mergeConfig } from "./config.js";
import { b as defaultInclude, d as defaultExclude } from "./vendor/defaults.js";

const RESULT = Symbol.for("deno.vitest.loadedConfig");

const configUrl = new URL(import.meta.url).searchParams.get("config");
const configPath = fileURLToPath(configUrl);

/** Mirrors Vite's `ConfigEnv` for `vitest run`. */
const configEnv = {
  command: "serve",
  mode: "test",
  isSsrBuild: false,
  isPreview: false,
};

async function evaluateConfigFile(path) {
  const mod = await import(pathToFileURL(path).href);
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

function resolveProject(config, configDir, fallbackName) {
  const root = resolve(configDir, config.root ?? ".");
  const test = config.test ?? {};
  const projectRoot = test.root ? resolve(root, test.root) : root;
  const dir = test.dir ? resolve(projectRoot, test.dir) : undefined;
  const mode = config.mode ?? "test";
  const envDir = config.envDir ? resolve(root, config.envDir) : root;
  const env = {
    ...loadEnv(mode, envDir, config.envPrefix ?? "VITE_"),
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
    alias: [
      ...normalizeAliases(test.alias),
      ...normalizeAliases(config.resolve?.alias),
    ],
    config: {
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

const rootConfig = await evaluateConfigFile(configPath);
const projects = await expandProjects(rootConfig, dirname(configPath));
globalThis[RESULT] = JSON.stringify({ configPath, projects });
