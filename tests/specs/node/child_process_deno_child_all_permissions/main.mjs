import { fork, spawnSync } from "node:child_process";

function run(label, args) {
  const result = spawnSync(process.execPath, args, { encoding: "utf8" });
  if (result.error) {
    console.log(`${label}: ${result.error.name}`);
  } else {
    console.log(`${label}: ${result.stdout.trim()}`);
  }
}

// No permission flag, with Node's arguments or Deno's: the child gets -A.
run("node args", ["child.js"]);
run("deno args", ["run", "child.js"]);
// A permission flag of the caller's own: exactly that.
run("deno args with a flag", ["run", "--allow-read", "child.js"]);
// fork() passes process.execArgv: the parent's grants.
const child = fork("child.js");
child.on("message", (line) => console.log(`fork: ${line}`));
child.on("error", (err) => console.log(`fork: ${err.name}`));
