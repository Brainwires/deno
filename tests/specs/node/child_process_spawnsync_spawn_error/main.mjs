// A child that can't be started is spawnSync()'s `error`, and
// execFileSync() throws it, as in Node: here a scoped --allow-run refuses an
// LD_PRELOAD in the child's environment.
import { execFileSync, spawnSync } from "node:child_process";

const env = { ...process.env, LD_PRELOAD: "/nonexistent.so" };
const result = spawnSync("true", [], { env });
console.log("error:", result.error?.name, result.error?.message);
try {
  execFileSync("true", [], { env });
} catch (err) {
  console.log("execFileSync:", err.name, err.message);
}
