import { run } from "node:test";
// Each test's tags as its events carry them, then a filtered run.
const seen = new Map();
const stream = run({ files: ["tagged.js"] });
for (
  const kind of [
    "test:enqueue",
    "test:dequeue",
    "test:start",
    "test:pass",
    "test:complete",
  ]
) {
  stream.on(kind, (data) => {
    const key = `${data.name} ${JSON.stringify(data.tags)}`;
    seen.set(key, (seen.get(key) ?? 0) + 1);
  });
}
for await (const _ of stream);
for (const key of [...seen.keys()].sort()) console.log(key, seen.get(key));
const passed = [];
const filtered = run({ files: ["tagged.js"], testTagFilters: ["DB"] });
filtered.on("test:pass", (data) => passed.push(data.name));
for await (const _ of filtered);
console.log("filtered:", passed);
