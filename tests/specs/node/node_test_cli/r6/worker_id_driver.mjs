// `NODE_TEST_WORKER_ID` shares the files out evenly whatever their durations:
// with 9 files and `--test-concurrency=3` each ID runs 3 of them, even when the
// first file takes far longer than the rest (a freed ID handed to the next
// file would serve most of them), and an ID runs one file at a time.
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const dir = mkdtempSync(join(tmpdir(), "worker-id-"));
const usage = join(dir, "usage.txt");
const files = [];
for (let i = 1; i <= 9; i++) {
  const file = join(dir, `file-${i}.test.mjs`);
  writeFileSync(
    file,
    `import { test } from "node:test";
import { appendFileSync } from "node:fs";
test("file ${i}", async () => {
  const id = process.env.NODE_TEST_WORKER_ID;
  appendFileSync(process.env.WORKER_USAGE_FILE, "start " + id + "\\n");
  await new Promise((resolve) => setTimeout(resolve, ${i === 1 ? 1500 : 0}));
  appendFileSync(process.env.WORKER_USAGE_FILE, "end " + id + "\\n");
});
`,
  );
  files.push(file);
}
const { status } = spawnSync(
  process.execPath,
  ["--test", "--test-concurrency=3", "--test-reporter=dot", ...files],
  { env: { ...process.env, WORKER_USAGE_FILE: usage }, stdio: "ignore" },
);
const counts = {};
const busy = new Set();
let overlapped = false;
for (const line of readFileSync(usage, "utf8").trim().split("\n")) {
  const [event, id] = line.split(" ");
  if (event === "start") {
    if (busy.has(id)) overlapped = true;
    busy.add(id);
    counts[id] = (counts[id] ?? 0) + 1;
  } else {
    busy.delete(id);
  }
}
rmSync(dir, { recursive: true, force: true });
const sorted = Object.fromEntries(
  Object.entries(counts).sort(([a], [b]) => Number(a) - Number(b)),
);
console.log({ status, counts: sorted, overlapped });
