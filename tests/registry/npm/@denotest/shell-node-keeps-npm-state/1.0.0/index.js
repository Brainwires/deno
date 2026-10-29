import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

// as @nestjs/cli runs @angular-devkit/schematics-cli: `node "<script>"`
// through a shell
const script = fileURLToPath(import.meta.resolve("./spawned.js"));
const result = spawnSync(`node "${script}"`, { shell: true, stdio: "inherit" });
if (result.error) {
  console.error("Failed:", result.error);
  process.exit(1);
}
process.exitCode = result.status;
