// Copyright 2018-2026 the Deno authors. MIT license.

// Loaded by `deno test` before a Vitest test module: resolves the file's
// Vitest config, then collects the module with Vitest's runner and registers
// its tests with `Deno.test()`.
//
// Served as `deno-vitest:///preload.js?file=<url>&project=<name>&environment=<name>`.

import { startFile } from "./runtime.js";
import resolvedConfig from "./resolved_config.json" with { type: "json" };

const params = new URL(import.meta.url).searchParams;
const file = params.get("file");
const projectName = params.get("project");
const project =
  resolvedConfig.projects?.find((p) =>
    projectName === null ? true : p.name === projectName
  ) ?? {};
const config = {
  ...project,
  update: resolvedConfig.update,
  ci: resolvedConfig.ci,
  filter: resolvedConfig.filter ?? undefined,
};
const environment = params.get("environment");
if (environment) config.environment = environment;

await startFile(file, config);
