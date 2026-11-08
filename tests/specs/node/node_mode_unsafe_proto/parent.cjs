// Runs as `node parent.cjs` (the deno executable under the name `node`).
const { fork, spawnSync } = require("node:child_process");
const check = require("./check.cjs");

console.log(`node: ${check()}`);

const spawned = spawnSync(process.execPath, ["child.cjs", "node spawn"], {
  encoding: "utf8",
});
process.stdout.write(spawned.stdout + spawned.stderr);

const child = fork("child.cjs", ["node fork"]);
child.on("exit", () => {
  // pm2 starts its daemon with an explicit, empty execArgv
  fork("child.cjs", ["node fork, empty execArgv"], { execArgv: [] });
});
