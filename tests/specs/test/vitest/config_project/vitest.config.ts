import { defineConfig } from "vitest/config";

export default defineConfig({
  define: {
    "import.meta.env.VITE_DEFINED": JSON.stringify("from-define"),
    __APP_NAME__: JSON.stringify("demo"),
  },
  resolve: {
    alias: {
      "@lib": new URL("./lib", import.meta.url).pathname,
    },
  },
  test: {
    include: ["src/**/*.test.ts"],
    exclude: ["src/sub/excluded.test.ts"],
    globals: true,
    setupFiles: ["./setup.ts"],
    restoreMocks: true,
    testTimeout: 1000,
    env: { FROM_TEST_ENV: "yes" },
  },
});
