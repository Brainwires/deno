// A module registry like jest-runtime's: modules are found on disk and
// evaluated in a vm context by the registry itself, never by the host's
// CommonJS loader, and a `.node` file is handed to the host's `require`.
// better-sqlite3's own `lib/database.js` then loads its addon through
// `bindings`.
"use strict";
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { builtinModules } = require("node:module");

const context = vm.createContext({
  process,
  Buffer,
  console,
  setImmediate,
  clearImmediate,
  setTimeout,
  clearTimeout,
});
const registry = new Map();

function notFound(request, from) {
  const err = new Error(`Cannot find module '${request}' from '${from}'`);
  err.code = "MODULE_NOT_FOUND";
  return err;
}

function resolveFile(candidate) {
  for (const file of [candidate, `${candidate}.js`, `${candidate}.json`]) {
    if (fs.existsSync(file) && fs.statSync(file).isFile()) return file;
  }
  return resolveDirectory(candidate);
}

function resolveDirectory(dir) {
  const manifest = path.join(dir, "package.json");
  if (fs.existsSync(manifest)) {
    const main = JSON.parse(fs.readFileSync(manifest, "utf8")).main;
    if (main) {
      const file = resolveFile(path.resolve(dir, main));
      if (file) return file;
    }
  }
  const index = path.join(dir, "index.js");
  return fs.existsSync(index) ? index : undefined;
}

function resolve(from, request) {
  if (path.isAbsolute(request) || request.startsWith(".")) {
    const file = resolveFile(path.resolve(path.dirname(from), request));
    if (file) return file;
    throw notFound(request, from);
  }
  for (let dir = path.dirname(from);; dir = path.dirname(dir)) {
    const file = resolveFile(path.join(dir, "node_modules", request));
    if (file) return file;
    if (path.dirname(dir) === dir) throw notFound(request, from);
  }
}

function requireModule(from, request) {
  const name = request.startsWith("node:") ? request.slice(5) : request;
  if (builtinModules.includes(name)) return require(request);
  const filename = resolve(from, request);
  const cached = registry.get(filename);
  if (cached) return cached.exports;
  const module = { exports: {}, filename, id: filename, loaded: false };
  registry.set(filename, module);
  if (filename.endsWith(".node")) {
    module.exports = require(filename);
  } else if (filename.endsWith(".json")) {
    module.exports = JSON.parse(fs.readFileSync(filename, "utf8"));
  } else {
    const wrapper = new vm.Script(
      '({"Object.<anonymous>":function(module,exports,require,__dirname,__filename){' +
        fs.readFileSync(filename, "utf8") + "\n}});",
      { filename },
    ).runInContext(context)["Object.<anonymous>"];
    const localRequire = (request) => requireModule(filename, request);
    wrapper.call(
      module.exports,
      module,
      module.exports,
      localRequire,
      path.dirname(filename),
      filename,
    );
  }
  module.loaded = true;
  return module.exports;
}

// `fresh()` is another test file's registry: its own module instances, the
// host's require cache shared
module.exports = (request) =>
  requireModule(path.join(__dirname, "main.cjs"), request);
module.exports.fresh = () => registry.clear();
