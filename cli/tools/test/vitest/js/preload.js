// Copyright 2018-2026 the Deno authors. MIT license.

// Loaded by `deno test` before a Vitest test module: resolves the file's
// Vitest config, then collects the module with Vitest's runner and registers
// its tests with `Deno.test()`.
//
// Served as `deno-vitest:///preload.js?file=<url>&project=<name>&environment=<name>`.

import { startFile } from "./runtime.js";
import resolvedConfig from "./resolved_config.json" with { type: "json" };

// The runner's own reads (see cli/tools/test/vitest/runner_reads.rs): its
// `__mocks__` lookups and the modules it imports itself (setup files,
// `__mocks__` files, automocked modules) need no read permission within the
// run's root. Taken before the test module runs, and only handed to the
// runner: the test's own reads and imports stay checked.
const runnerFs = globalThis.Deno[globalThis.Deno.internal]
  .takeVitestRunnerFs?.();

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
// Like Vitest's and Jest's reporters, a named project's tests carry its
// name when several projects run (a file can run in each that includes it).
if ((resolvedConfig.projects?.length ?? 0) > 1 && project.label) {
  config.testNamePrefix = `|${project.label}| `;
}
const environment = params.get("environment");
if (environment) config.environment = environment;

await startFile(file, config, runnerFs);
