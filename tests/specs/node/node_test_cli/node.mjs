// Runs the rest of the command line as `node` would: a child on the execPath
// takes Node's flags.
import { spawnSync } from "node:child_process";
const { status } = spawnSync(process.execPath, process.argv.slice(2), {
  stdio: "inherit",
});
process.exit(status);
