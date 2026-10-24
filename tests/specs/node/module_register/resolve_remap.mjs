import { register } from "node:module";

register("./hooks_remap.mjs", import.meta.url);
const mod = await import("virtual-math");
console.log(mod.default);
