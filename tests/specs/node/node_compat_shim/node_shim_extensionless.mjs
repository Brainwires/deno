// The deno executable under the name `node`, as `deno x --install-node-shims`
// writes it: a symlink on unix, a hardlink (or copy) named `node.exe` on
// Windows. `node dist/main` (no extension) must resolve `dist/main.js`.
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

const result = spawnSync(shim, ["dist/main", "a", "b"], { encoding: "utf8" });

if (result.error) {
  console.log("spawn error:", result.error.code);
} else {
  console.log("status:", result.status);
  console.log("stdout:", result.stdout.trim());
  if (result.stderr.trim()) console.log("stderr:", result.stderr.trim());
}
