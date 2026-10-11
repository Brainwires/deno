import { defineConfig, defineProject } from "vitest/config";

export default defineConfig({
  test: {
    testTimeout: 1234,
    projects: [
      defineProject({
        extends: true,
        test: { name: "alpha", include: ["a/**/*.test.ts"] },
      }),
      { test: { name: "beta", include: ["b/**/*.test.ts"], globals: true } },
    ],
  },
});
