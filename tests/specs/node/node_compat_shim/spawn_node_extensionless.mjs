// `node dist/main` (no extension), as `nest start` and `npm run start:prod`
// run it, through the `node` Deno puts on PATH when no real one exists. The
// entrypoint is resolved by a child deno process, which must not re-enter
// Node.js argument translation itself.
import { spawnSync } from "node:child_process";

const result = spawnSync("node", ["dist/main", "a", "b"], {
  encoding: "utf8",
});

if (result.error) {
  console.log("spawn error:", result.error.code);
} else {
  console.log("status:", result.status);
  console.log("stdout:", result.stdout.trim());
  if (result.stderr.trim()) console.log("stderr:", result.stderr.trim());
}
