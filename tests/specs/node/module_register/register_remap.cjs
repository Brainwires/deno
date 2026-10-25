const { register } = require("node:module");
const { pathToFileURL } = require("node:url");

register("./hooks_remap.mjs", pathToFileURL(__filename));
