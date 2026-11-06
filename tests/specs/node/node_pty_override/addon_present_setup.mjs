// Puts an (empty) addon file where node-pty's loader looks for a prebuild, so
// the stand-in counts as a package that carries its addon (DENEXT.md item
// 268): its own entry then runs instead of the runtime's implementation.
import { mkdirSync, writeFileSync } from "node:fs";
import process from "node:process";

const dir =
  `node_modules/node-pty/prebuilds/${process.platform}-${process.arch}`;
mkdirSync(dir, { recursive: true });
writeFileSync(`${dir}/pty.node`, "");
