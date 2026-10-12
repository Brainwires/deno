// Copyright 2018-2026 the Deno authors. MIT license.

// The two `std-env` flags Vitest's config defaults read.

// Read without an --allow-env check (see `vitestEnv` in cli/js/40_test.js).
const internalEnv = globalThis.Deno[globalThis.Deno.internal].vitestEnv();

function env(name) {
  return internalEnv[name];
}

export const isCI = !!(env("CI") && env("CI") !== "false") ||
  !!env("GITHUB_ACTIONS") || !!env("BUILDKITE") || !!env("GITLAB_CI");

export const isAgent = !!env("CLAUDECODE") || !!env("CODEX_SANDBOX") ||
  !!env("AI_AGENT");
