// A name or skip pattern rides to each file's child in NODE_OPTIONS. Quotes
// and spaces in it must not add flags there (here `--import`).
import { run } from "node:test";
import { existsSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const mark = join(tmpdir(), `denext-inject-${process.pid}`);
const stream = run({
  files: ["test/a.js"],
  testNamePatterns: ["a --import=./evil.mjs", 'a" --import=./evil.mjs "'],
  testSkipPatterns: ['zz" --import=./evil.mjs "'],
  env: { ...process.env, INJECT_MARK: mark },
});
for await (const _event of stream);
console.log("injected:", existsSync(mark));
rmSync(mark, { force: true });
