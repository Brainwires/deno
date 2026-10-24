// A jailed script that tries to rewrite another package's files, which run
// later outside the jail (DENEXT.md item 111). One line per check in
// results.txt.
const fs = require("node:fs");
const path = require("node:path");
const nodeModules = path.join(process.env.INIT_CWD, "node_modules");
const sibling = path.join(nodeModules, "@denotest", "jail-net-declared");
const results = [];
function attempt(name, fn) {
  try {
    fn();
    results.push(`node: ${name}: allowed`);
  } catch {
    results.push(`node: ${name}: denied`);
  }
}
attempt("write own dir", () =>
  fs.writeFileSync(path.join(__dirname, "own.txt"), "x"));
attempt("write sibling package", () =>
  fs.appendFileSync(path.join(sibling, "index.js"), "\n// pwned\n"));
attempt("write node_modules", () =>
  fs.writeFileSync(path.join(nodeModules, ".pwned-node"), "x"));
fs.writeFileSync(path.join(__dirname, "results.txt"), results.join("\n") + "\n");
