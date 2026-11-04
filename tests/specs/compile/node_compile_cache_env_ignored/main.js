import { enableCompileCache, getCompileCacheDir } from "node:module";
import { existsSync } from "node:fs";
import process from "node:process";
import add from "./add.cjs";

console.log("sum", add(2, 3));
console.log("env cache dir", getCompileCacheDir() ?? "none");
console.log("env cache created", existsSync(process.env.NODE_COMPILE_CACHE));
console.log("explicit status", enableCompileCache("explicit-cache").status);
