const fs = require("node:fs");
const path = require("node:path");
const gitDep = require("git-dep");
const gitSemver = require("git-semver");
console.log(require("tarball-dep").sum, gitDep.version, gitSemver.version);
// a git or tarball dependency's own dependencies are linked into its
// node_modules
const own = (name) =>
  fs.existsSync(
    path.join(
      fs.realpathSync(`node_modules/${name}`),
      "node_modules/@denotest/add",
    ),
  );
console.log("own deps:", own("git-dep"), own("tarball-dep"));
for (const bin of ["git-dep-cli", "local-tool", "tarball-dep-cli"]) {
  console.log(bin, fs.existsSync(`node_modules/.bin/${bin}`));
}
