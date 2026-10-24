import { createRequire, register } from "node:module";

register("./hooks_transform.mjs", import.meta.url);
console.log((await import("./original.mjs")).default);
// As in Node, hooks registered with register() do not apply to require().
const require = createRequire(import.meta.url);
console.log(require("./original.cjs"));
