import { spawnSync } from "node:child_process";

// Node's --experimental-loader / --import / --require reach the child.
for (
  const args of [
    ["--experimental-loader", "./hooks_remap.mjs", "child_remap.mjs"],
    ["--import", "./register_remap.mjs", "child_remap.mjs"],
    ["--require", "./register_remap.cjs", "child_remap.mjs"],
  ]
) {
  const child = spawnSync(process.execPath, args, { encoding: "utf8" });
  console.log(args[0], child.status, child.stdout.trim());
  if (child.status !== 0) console.log(child.stderr);
}
