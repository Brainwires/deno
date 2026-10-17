// A pass-through load hook leaves WebAssembly imports working: the hook sees
// format "wasm", and the default load hands the file to the module loader as
// bytes instead of reading it as a string.
import { registerHooks } from "node:module";

registerHooks({
  load(url, context, nextLoad) {
    if (url.endsWith("/math.wasm")) console.log("format:", context.format);
    return nextLoad(url, context);
  },
});

const { add } = await import("./math.wasm");
console.log("add(1, 2):", add(1, 2));
