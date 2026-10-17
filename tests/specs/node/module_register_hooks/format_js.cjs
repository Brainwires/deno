// A require() load hook sees the format Node gives it: a .js file takes its
// nearest package.json "type" (looked up no further than a node_modules
// directory), and with none the format stays undefined, so the source is
// still compiled with CommonJS-or-ESM detection (an ES module in an untyped
// package keeps loading).
const assert = require("node:assert");
const { registerHooks } = require("node:module");

const formats = {};
registerHooks({
  load(url, context, nextLoad) {
    const result = nextLoad(url, context);
    const at = url.indexOf("/format_js/");
    if (at !== -1) {
      formats[url.slice(at + 11)] = [context.format, result.format];
    }
    return result;
  },
});

assert.strictEqual(
  require("./format_js/typeless/esm.js").default,
  "typeless esm",
);
assert.strictEqual(require("./format_js/typeless/cjs.js"), "typeless cjs");
assert.strictEqual(require("./format_js/commonjs/index.js"), "commonjs");
assert.strictEqual(require("./format_js/module/index.js").default, "module");
// The package.json lookup stops at node_modules, so the "module" type above
// doesn't reach a package that has no package.json of its own.
assert.strictEqual(
  require("./format_js/module/node_modules/untyped/index.js"),
  "untyped in node_modules",
);
for (const name of Object.keys(formats).sort()) {
  console.log(name, String(formats[name][0]), String(formats[name][1]));
}
