// Runs each case through the deno executable under the name `node` (a
// symlink on unix, a hardlink or copy named `node.exe` on Windows) and prints
// its exit code and stderr, which must be Node's text.
import { spawnSync } from "node:child_process";

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

const cases = [
  ["throw.js"],
  ["throw_fn.js"],
  ["reject.js"],
  ["reject_num.js"],
  ["throw.mjs"],
  ["--check", "bad.js"],
  ["--check", "good.js"],
  ["--stack-trace-limit=3", "stack_trace_limit.js"],
];
for (const args of cases) {
  const result = spawnSync(shim, args, { encoding: "utf8" });
  const colour = result.stderr.includes("\x1b[");
  console.log(
    `--- node ${args.join(" ")}: exit ${result.status}, colour ${colour}`,
  );
  if (result.stdout) console.log(result.stdout.trimEnd());
  if (result.stderr) console.log(result.stderr.trimEnd());
}
