// Loaded without --allow-read: the config, its own imports, the tsconfig
// it takes `paths` from and the `.env` files are the runner's reads.
import { readFileSync } from "node:fs";
import { defineConfig } from "vitest/config";
import { srcDir } from "./config/dirs.ts";

// What the config's code does itself stays checked.
function outcome(fn: () => unknown): string {
  try {
    fn();
    return "allowed";
  } catch (error) {
    return (error as Error).name;
  }
}
const configRead = outcome(() =>
  readFileSync(new URL("./src/data.txt", import.meta.url))
);

export default defineConfig({
  resolve: {
    alias: { "@": srcDir },
    tsconfigPaths: true,
  },
  test: {
    include: ["src/**/*.test.ts"],
    globals: true,
    setupFiles: ["./setup.ts"],
    env: { CONFIG_READ: configRead },
  },
});
