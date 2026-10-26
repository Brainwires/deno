// The install check of a package that ships a prebuilt Node-API addon
// (node-gyp-build-test): `node` loads the package's own `.node` file. In the
// jail that may load native code from the package's folder only. The file
// here isn't a real addon, so an allowed load fails to open it instead.
const fs = require("node:fs");
const path = require("node:path");

const results = [];
function load(name, file) {
  try {
    process.dlopen({ exports: {} }, file);
    results.push(`${name}: loaded`);
  } catch (err) {
    results.push(
      `${name}: ${err && err.name === "NotCapable" ? "denied" : "allowed"}`,
    );
  }
}

load("own prebuilt addon", path.join(__dirname, "prebuilds", "addon.node"));
const outside = path.join(process.env.INIT_CWD, "outside.node");
load("addon outside the package", outside);
fs.writeFileSync(
  path.join(__dirname, "results.txt"),
  results.join("\n") + "\n",
);
