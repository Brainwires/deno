// Copyright 2018-2026 the Deno authors. MIT license.

// The `vitest/config` module: config helpers are identities, so a
// vitest.config.* / vite.config.* file can be evaluated without Vite.

export {
  a as coverageConfigDefaults,
  b as defaultInclude,
  c as configDefaults,
  d as defaultExclude,
} from "./vendor/defaults.js";

import * as nodeFs from "node:fs";
import { join } from "node:path";

export const defaultBrowserPort = 63315;

export function normalizePath(id) {
  return id.replace(/\\/g, "/");
}

// Vite's loadEnv(): .env files of the mode, filtered by the env prefixes.
function parseDotenv(text) {
  const result = {};
  for (const line of text.split(/\r?\n/)) {
    const match = /^\s*(?:export\s+)?([\w.-]+)\s*=\s*(.*)?\s*$/.exec(line);
    if (!match) continue;
    let value = (match[2] ?? "").trim();
    const quote = value[0];
    if (
      (quote === '"' || quote === "'" || quote === "`") &&
      value.endsWith(quote)
    ) {
      value = value.slice(1, -1);
      if (quote === '"') value = value.replace(/\\n/g, "\n");
    } else {
      value = value.replace(/\s+#.*$/, "");
    }
    result[match[1]] = value;
  }
  return result;
}

// `fs` is the runner's own (see config_loader.js) when the runner loads the
// env of a project; a config calling `loadEnv()` reads with its permissions.
export function loadEnv(mode, envDir, prefixes = "VITE_", fs = nodeFs) {
  prefixes = Array.isArray(prefixes) ? prefixes : [prefixes];
  const env = {};
  for (
    const file of [".env", ".env.local", `.env.${mode}`, `.env.${mode}.local`]
  ) {
    const path = join(envDir, file);
    if (!fs.existsSync(path)) continue;
    Object.assign(env, parseDotenv(fs.readFileSync(path, "utf8")));
  }
  const result = {};
  for (const [key, value] of Object.entries(env)) {
    if (prefixes.some((p) => key.startsWith(p))) result[key] = value;
  }
  let processEnv;
  try {
    processEnv = { ...process.env };
  } catch {
    // no --allow-env: only the .env files
    processEnv = {};
  }
  for (const key in processEnv) {
    if (prefixes.some((p) => key.startsWith(p))) result[key] = processEnv[key];
  }
  return result;
}

export function defineConfig(config) {
  return config;
}

export function defineProject(config) {
  return config;
}

export function defineWorkspace(config) {
  return config;
}

// `mergeConfig` follows Vite's semantics (MIT, Copyright (c) 2019-present,
// VoidZero Inc. and Vite contributors): arrays are concatenated, objects
// merged recursively and `resolve.alias` entries combined.
function isObject(value) {
  return Object.prototype.toString.call(value) === "[object Object]";
}

function arraify(value) {
  return Array.isArray(value) ? value : [value];
}

function normalizeAlias(alias) {
  if (Array.isArray(alias)) return alias;
  return Object.entries(alias ?? {}).map(([find, replacement]) => ({
    find,
    replacement,
  }));
}

function mergeAlias(a, b) {
  if (!a) return b;
  if (!b) return a;
  if (isObject(a) && isObject(b)) return { ...a, ...b };
  return [...normalizeAlias(b), ...normalizeAlias(a)];
}

function mergeConfigRecursively(defaults, overrides, rootPath) {
  const merged = { ...defaults };
  for (const key of Object.keys(overrides)) {
    const value = overrides[key];
    if (value == null) continue;
    const existing = merged[key];
    if (existing == null) {
      merged[key] = value;
      continue;
    }
    if (key === "alias" && (rootPath === "resolve" || rootPath === "")) {
      merged[key] = mergeAlias(existing, value);
      continue;
    } else if (key === "assetsInclude" && rootPath === "") {
      merged[key] = [].concat(existing, value);
      continue;
    } else if (
      key === "noExternal" && (rootPath === "ssr" || rootPath === "resolve") &&
      (existing === true || value === true)
    ) {
      merged[key] = true;
      continue;
    }
    if (Array.isArray(existing) || Array.isArray(value)) {
      merged[key] = [...arraify(existing), ...arraify(value)];
      continue;
    }
    if (isObject(existing) && isObject(value)) {
      merged[key] = mergeConfigRecursively(
        existing,
        value,
        rootPath ? `${rootPath}.${key}` : key,
      );
      continue;
    }
    merged[key] = value;
  }
  return merged;
}

export function mergeConfig(defaults, overrides, isRoot = true) {
  if (typeof defaults === "function" || typeof overrides === "function") {
    throw new Error("Cannot merge config in form of callback");
  }
  return mergeConfigRecursively(defaults, overrides, isRoot ? "" : ".");
}
