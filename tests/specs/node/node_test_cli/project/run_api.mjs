import { run } from "node:test";
const names = [];
const stream = run({ files: ["test/a.js"] });
stream.on("test:pass", (data) => names.push(data.name));
for await (const event of stream) {
  if (event.type === "test:summary" && event.data.file === undefined) {
    console.log(names, event.data.success, event.data.counts.passed);
  }
}
