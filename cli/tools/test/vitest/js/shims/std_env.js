// Copyright 2018-2026 the Deno authors. MIT license.

// The two `std-env` flags Vitest's config defaults read.

function env(name) {
  try {
    return Deno.env.get(name);
  } catch {
    return undefined;
  }
}

export const isCI = !!(env("CI") && env("CI") !== "false") ||
  !!env("GITHUB_ACTIONS") || !!env("BUILDKITE") || !!env("GITLAB_CI");

export const isAgent = !!env("CLAUDECODE") || !!env("CODEX_SANDBOX") ||
  !!env("AI_AGENT");
