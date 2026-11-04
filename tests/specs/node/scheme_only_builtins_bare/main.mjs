import { builtinModules, createRequire, isBuiltin } from "node:module";
import vfsPackage from "vfs";
import * as vfs from "node:vfs";
import user from "user";

const require = createRequire(import.meta.url);

console.log(vfsPackage);
console.log(user);
console.log(require("vfs"));
console.log(require("test"));
console.log(typeof require("node:vfs").create, typeof vfs.create);
console.log(isBuiltin("vfs"), isBuiltin("node:vfs"), isBuiltin("test"));
console.log(
  builtinModules.includes("node:vfs"),
  builtinModules.includes("vfs"),
);
console.log(require.resolve("vfs").endsWith("index.js"));
