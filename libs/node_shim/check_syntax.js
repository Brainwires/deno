// Copyright 2018-2026 the Deno authors. MIT license.

// denext: `node --check <file>`. Compiles the file the way Node loads it
// (an ES module or a CommonJS module, the latter retried as a module when its
// error is module syntax) without running it. A syntax error ends the program
// as an uncaught error, which Node mode reports as Node does.
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { compileFunction, SourceTextModule } from "node:vm";

// The error reports the file's position, not this module's frames.
Error.stackTraceLimit = 0;

const file = resolve(Deno.args[0]);
// A hashbang line is a comment to Node; keep the columns.
const source = readFileSync(file, "utf8").replace(/^#!/, "//");

function packageType(dir) {
  for (;;) {
    try {
      return JSON.parse(readFileSync(join(dir, "package.json"), "utf8")).type;
    } catch {
      const parent = dirname(dir);
      if (parent === dir) return undefined;
      dir = parent;
    }
  }
}

function checkModule() {
  new SourceTextModule(source, { identifier: pathToFileURL(file).href });
}

function checkCommonJs() {
  compileFunction(
    source,
    ["exports", "require", "module", "__filename", "__dirname"],
    { filename: file },
  );
}

const moduleSyntax =
  /Cannot use import statement|Unexpected token 'export'|Cannot use 'import\.meta'|await is only valid/;

if (
  /\.m[jt]s$/.test(file) ||
  (!/\.c[jt]s$/.test(file) && packageType(dirname(file)) === "module")
) {
  checkModule();
} else {
  try {
    checkCommonJs();
  } catch (error) {
    if (!(error instanceof SyntaxError) || !moduleSyntax.test(error.message)) {
      throw error;
    }
    checkModule();
  }
}
