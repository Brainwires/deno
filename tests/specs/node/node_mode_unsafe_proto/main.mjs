// The deno executable under the name `node`, as `deno x --install-node-shims`
// writes it: a symlink on unix, a hardlink (or copy) named `node.exe` on
// Windows.
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";

const check = createRequire(import.meta.url)("./check.cjs");
console.log(`deno run: ${check()}`);

Deno.mkdirSync("shim_bin", { recursive: true });
const isWindows = Deno.build.os === "windows";
const shim = `shim_bin/${isWindows ? "node.exe" : "node"}`;
if (isWindows) {
  try {
    Deno.linkSync(Deno.execPath(), shim);
  } catch {
    Deno.copyFileSync(Deno.execPath(), shim);
  }
} else {
  Deno.symlinkSync(Deno.execPath(), shim);
}

const encoder = new TextEncoder();

function run(file, args) {
  const result = spawnSync(file, args, { encoding: "utf8" });
  if (result.error) {
    console.log("spawn error:", result.error.code);
    return;
  }
  Deno.stdout.writeSync(encoder.encode(result.stdout + result.stderr));
}

run(shim, ["parent.cjs"]);
run(shim, [
  "-e",
  "console.log('node -e: ' + require('./check.cjs')())",
]);
// `node child.cjs` spawned by Deno code is a Node-mode child too
run(Deno.execPath(), ["child.cjs", "deno run spawning node"]);
