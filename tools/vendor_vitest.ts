#!/usr/bin/env -S deno run --allow-net --allow-read --allow-write --allow-run=tar
// Copyright 2018-2026 the Deno authors. MIT license.

// Vendors the parts of Vitest's runtime that `deno test` serves to test
// modules importing `vitest` (see cli/tools/test/vitest/). The output goes to
// cli/tools/test/vitest/js/vendor/ and must not be edited by hand: change the
// pins or the patches below and re-run this script instead.
//
//   ./tools/vendor_vitest.ts
//
// Vitest publishes its runtime as hashed chunks; when bumping the version,
// update the chunk names in `CHUNKS` (the script fails loudly when a chunk,
// an import or a patch target is missing).

// deno-lint-ignore-file no-console

// Plain path helpers: this script must not depend on the std submodule.
const ROOT_PATH = new URL("..", import.meta.url).pathname.replace(/\/$/, "");
const join = (...parts: string[]) => parts.join("/");

const VITEST = "5.0.3";
const VITEST_MOCKER = "5.0.3";
const CHAI = "6.2.2";
const MAGIC_STRING = "1.4.3";
const SOURCEMAP_CODEC = "1.6.0";
const EXPECT_TYPE = "1.4.0";

const OUT_DIR = join(ROOT_PATH, "cli/tools/test/vitest/js/vendor");

/** Vitest chunk (relative to the package's `dist/`) => vendored file name. */
const CHUNKS: Record<string, string> = {
  "chunks/run.BTlFXlnv.js": "runner.js",
  "chunks/index.C4tKZ0yW.js": "vitest_core.js",
  "chunks/index.cx-_w29y.js": "pretty_format.js",
  "chunks/display.CXBnkt9a.js": "display.js",
  "chunks/pathe.M-eThtNZ.CknpM8Or.js": "pathe.js",
  "chunks/source-map.DUxvo12T.js": "source_map.js",
  "chunks/spy.DgMTfhrK.js": "spy.js",
  "chunks/plugins.D2ut1V9b.js": "plugins.js",
  "chunks/offset.Dy-5Fdfn.js": "offset.js",
  "chunks/tinyrainbow.Ht9iggcq.js": "tinyrainbow.js",
  "chunks/utils.DYj33du9.js": "utils.js",
  "chunks/constants.-juJ8b_4.js": "constants.js",
  "chunks/defaults.D2ip7f-X.js": "defaults.js",
  "task-utils.js": "task_utils.js",
};

/** Specifiers rewritten in every vendored file. */
const IMPORT_MAP: Record<string, string> = {
  "chai": "./chai.js",
  "tinybench": "../shims/tinybench.js",
  "expect-type": "./expect_type.js",
  "magic-string": "./magic_string.js",
  "@jridgewell/sourcemap-codec": "./sourcemap_codec.js",
  // Vitest's RPC channel to its main process; `deno test` reports through
  // its own runner instead.
  "./rpc.BTmCtPly.js": "../shims/rpc.js",
  "std-env": "../shims/std_env.js",
  "./env.DzFJjrmK.js": "../shims/empty.js",
};

interface Patch {
  file: string;
  find: string;
  replace: string;
  why: string;
}

/** Vitest's own environment reads, which need no --allow-env. */
const INTERNAL_ENV = "globalThis.Deno[globalThis.Deno.internal].vitestEnv()";

const PATCHES: Patch[] = [
  {
    file: "tinyrainbow.js",
    why:
      "read the color environment variables without an --allow-env check (Deno.internal.vitestEnv, see cli/js/40_test.js)",
    find: `r = (e == null ? void 0 : e.env) || {}`,
    replace: `r = ${INTERNAL_ENV}`,
  },
  {
    file: "pretty_format.js",
    why:
      "read NODE_ENV without an --allow-env check (Deno.internal.vitestEnv, see cli/js/40_test.js)",
    find: `"production" !== process.env.NODE_ENV &&`,
    replace: `"production" !== ${INTERNAL_ENV}.NODE_ENV &&`,
  },
  {
    file: "pretty_format.js",
    why:
      "read NODE_ENV without an --allow-env check (Deno.internal.vitestEnv, see cli/js/40_test.js)",
    find:
      `if (process.env.NODE_ENV === 'production') {\n\t  reactIs$1.exports = requireReactIs_production();`,
    replace:
      `if (${INTERNAL_ENV}.NODE_ENV === 'production') {\n\t  reactIs$1.exports = requireReactIs_production();`,
  },
  {
    file: "pretty_format.js",
    why:
      "read NODE_ENV without an --allow-env check (Deno.internal.vitestEnv, see cli/js/40_test.js)",
    find: `if (process.env.NODE_ENV !== "production") {`,
    replace: `if (${INTERNAL_ENV}.NODE_ENV !== "production") {`,
  },
  {
    file: "pretty_format.js",
    why:
      "read NODE_ENV without an --allow-env check (Deno.internal.vitestEnv, see cli/js/40_test.js)",
    find:
      `if (process.env.NODE_ENV === 'production') {\n\t  reactIs.exports = requireReactIs_production_min();`,
    replace:
      `if (${INTERNAL_ENV}.NODE_ENV === 'production') {\n\t  reactIs.exports = requireReactIs_production_min();`,
  },
  {
    file: "defaults.js",
    why:
      "read GITHUB_ACTIONS without an --allow-env check (Deno.internal.vitestEnv, see cli/js/40_test.js)",
    find: `...process.env.GITHUB_ACTIONS === "true"`,
    replace: `...${INTERNAL_ENV}.GITHUB_ACTIONS === "true"`,
  },
  {
    file: "runner.js",
    why:
      "keep the original error next to its serialized copy so the Deno test " +
      "reporter can show the real stack trace",
    find: `	try {
		return serializeValue(err);
	} catch (e) {`,
    replace: `	try {
		const serialized = serializeValue(err);
		if (serialized && typeof serialized === "object") Object.defineProperty(serialized, Symbol.for("deno.vitest.originalError"), { value: err });
		return serialized;
	} catch (e) {`,
  },
  {
    file: "runner.js",
    why: "tell the Deno bridge synchronously when a task settles (the runner " +
      "only reports task updates through a throttled RPC otherwise)",
    find: `function updateTask(event, task, runner) {
	eventsPacks.push([`,
    replace: `function updateTask(event, task, runner) {
	runner.onDenoTaskUpdate?.(event, task);
	eventsPacks.push([`,
  },
  {
    file: "environments.js",
    why: "the vendored module is not a file: URL, resolve jsdom from the root",
    find: "const _require = createRequire(import.meta.url);",
    replace: "const _require = createRequire(denoVitestRootRequireBase());",
  },
  {
    file: "task_utils.js",
    why: "Jest projects name tests (expect's currentTestName) " +
      '"describe test", not "describe > test" (see jest.js)',
    find: `function getTestName(task, separator = " > ") {`,
    replace:
      `function getTestName(task, separator = globalThis[Symbol.for("deno.vitest.testNameSeparator")] ?? " > ") {`,
  },
  {
    file: "task_utils.js",
    why: "Jest projects name tests (a task's fullTestName) " +
      '"describe test", not "describe > test" (see jest.js)',
    find: `function createTaskName(names, separator = " > ") {`,
    replace:
      `function createTaskName(names, separator = globalThis[Symbol.for("deno.vitest.testNameSeparator")] ?? " > ") {`,
  },
  {
    file: "vitest_core.js",
    why: 'Jest projects key snapshots by "describe test 1" (see jest.js)',
    find: `		name: getNames(test).slice(1).join(" > "),`,
    replace:
      `		name: getNames(test).slice(1).join(globalThis[Symbol.for("deno.vitest.testNameSeparator")] ?? " > "),`,
  },
  {
    file: "vitest_core.js",
    why: "Jest does not fail a test whose .resolves / .rejects assertion " +
      "is not awaited (the assertion still settles before the test ends)",
    find: `		test.onFinished.push(() => {
			if (!resolved) {`,
    replace: `		test.onFinished.push(() => {
			if (!resolved && !globalThis[Symbol.for("deno.vitest.jestAsyncExpect")]) {`,
  },
  {
    file: "vitest_core.js",
    why: "Jest's non-strict equality (toEqual, toHaveBeenCalledWith) " +
      "ignores undefined array items, so [1, undefined] equals [1]",
    find:
      `	if (className === "[object Array]" && a.length !== b.length) return false;`,
    replace:
      `	if (className === "[object Array]" && a.length !== b.length && (hasKey !== hasDefinedKey || !globalThis[Symbol.for("deno.vitest.jestEquality")])) return false;`,
  },
  {
    file: "runner.js",
    why: "Jest projects format test.each titles like jest-each (%p, " +
      "$variable, no truncation; see jest.js)",
    find: `function formatTitle(template, items, idx) {`,
    replace: `function formatTitle(template, items, idx) {
	const denoFormatTitle = globalThis[Symbol.for("deno.vitest.formatTitle")];
	if (denoFormatTitle) return denoFormatTitle(template, items, idx);`,
  },
  {
    file: "environments.js",
    why: "surface uncaught window errors through Deno's error reporting",
    find: `process.emit("uncaughtException", e.error);`,
    replace: `denoVitestReportUnhandledError(e.error);`,
  },
];

async function fetchPackage(name: string, version: string, tmp: string) {
  const base = name.split("/").pop();
  const url = `https://registry.npmjs.org/${name}/-/${base}-${version}.tgz`;
  const res = await fetch(url);
  if (!res.ok) throw new Error(`Failed to download ${url}: ${res.status}`);
  const dir = join(tmp, name.replace("/", "__"));
  await Deno.mkdir(dir, { recursive: true });
  const tgz = join(dir, "pkg.tgz");
  await Deno.writeFile(tgz, new Uint8Array(await res.arrayBuffer()));
  const out = await new Deno.Command("tar", {
    args: ["xzf", tgz, "-C", dir],
  }).output();
  if (!out.success) throw new Error(`Failed to extract ${tgz}`);
  return join(dir, "package");
}

function header(source: string, license: string) {
  return `// Vendored from ${source} by tools/vendor_vitest.ts. Do not edit.\n` +
    `// ${license}\n\n`;
}

const IMPORT_RE = /(\bfrom\s*|\bimport\s*\(\s*|\bimport\s+)(['"])([^'"]+)\2/g;

/** Resolves a relative `spec` against the dist-relative `importer` path. */
function resolveDistPath(importer: string, spec: string) {
  const parts = importer.split("/").slice(0, -1);
  for (const part of spec.split("/")) {
    if (part === "..") parts.pop();
    else if (part !== ".") parts.push(part);
  }
  return parts.join("/");
}

function rewriteImports(file: string, code: string, importer = "") {
  // Doc comments carry example imports; leave those lines alone.
  return code.split("\n").map((line) =>
    /^\s*(\*|\/\/)/.test(line)
      ? line
      : rewriteImportsInLine(file, line, importer)
  ).join("\n");
}

function rewriteImportsInLine(file: string, code: string, importer: string) {
  return code.replace(IMPORT_RE, (match, prefix, quote, spec) => {
    // Template strings in error messages, e.g. `from "${provider}"`.
    if (spec.startsWith("node:") || spec.includes("${")) return match;
    let mapped = IMPORT_MAP[spec];
    if (mapped === undefined && spec.startsWith(".")) {
      const name = CHUNKS[resolveDistPath(importer, spec)];
      if (name !== undefined) mapped = `./${name}`;
    }
    if (mapped === undefined) {
      throw new Error(`${file}: unmapped import ${JSON.stringify(spec)}`);
    }
    return `${prefix}${quote}${mapped}${quote}`;
  });
}

function applyPatches(name: string, code: string) {
  for (const patch of PATCHES.filter((p) => p.file === name)) {
    const count = code.split(patch.find).length - 1;
    if (count !== 1) {
      throw new Error(
        `${name}: patch (${patch.why}) matched ${count} times, expected 1`,
      );
    }
    code = code.replace(patch.find, () => patch.replace);
  }
  return code;
}

function slice(code: string, start: string, end: string, file: string) {
  const from = code.indexOf(start);
  const to = code.indexOf(end, from);
  if (from === -1 || to === -1) {
    throw new Error(`${file}: markers ${start} .. ${end} not found`);
  }
  return code.slice(from, to + end.length);
}

const VITEST_LICENSE =
  "Copyright (c) 2021-Present VoidZero Inc. and Vitest contributors. " +
  "MIT License, see LICENSE-vitest.md (which also lists bundled code).";

async function main() {
  const tmp = await Deno.makeTempDir({ prefix: "vendor_vitest_" });
  try {
    const vitest = await fetchPackage("vitest", VITEST, tmp);
    const mocker = await fetchPackage("@vitest/mocker", VITEST_MOCKER, tmp);
    const chai = await fetchPackage("chai", CHAI, tmp);
    const magic = await fetchPackage("magic-string", MAGIC_STRING, tmp);
    const codec = await fetchPackage(
      "@jridgewell/sourcemap-codec",
      SOURCEMAP_CODEC,
      tmp,
    );
    const expectType = await fetchPackage("expect-type", EXPECT_TYPE, tmp);

    await Deno.remove(OUT_DIR, { recursive: true }).catch(() => {});
    await Deno.mkdir(OUT_DIR, { recursive: true });
    const write = (name: string, code: string) =>
      Deno.writeTextFile(join(OUT_DIR, name), applyPatches(name, code));

    for (const [chunk, name] of Object.entries(CHUNKS)) {
      const code = await Deno.readTextFile(join(vitest, "dist", chunk));
      await write(
        name,
        header(`vitest@${VITEST} (dist/${chunk})`, VITEST_LICENSE) +
          rewriteImports(name, code, chunk),
      );
    }

    // Vitest's test environments (jsdom, happy-dom, node), without the
    // module runner they share a chunk with.
    {
      const chunk = "chunks/index.k8L_ZRzq.js";
      const code = await Deno.readTextFile(join(vitest, "dist", chunk));
      const body = slice(
        code,
        "const LIVING_KEYS = [",
        '\t"edge-runtime": edge\n};',
        chunk,
      );
      await write(
        "environments.js",
        header(
          `vitest@${VITEST} (dist/${chunk}, environments)`,
          VITEST_LICENSE,
        ) +
          `import { createRequire } from "node:module";\n` +
          `import { URL as URL$1 } from "node:url";\n` +
          `import { Console } from "node:console";\n` +
          `import { denoVitestReportUnhandledError, denoVitestRootRequireBase } from "../shims/environment_support.js";\n\n` +
          body + "\n\nexport { environments, populateGlobal };\n",
      );
    }

    // `mockObject()` (automocking) from @vitest/mocker, without its module
    // registry (module mocking is implemented by `deno test` itself).
    {
      const code = await Deno.readTextFile(join(mocker, "dist/index.js"));
      const body = slice(
        code,
        "function mockObject(",
        "\nexport { mockObject };",
        "@vitest/mocker",
      ).replace(/\nexport \{ mockObject \};$/, "");
      await write(
        "mocker.js",
        header(
          `@vitest/mocker@${VITEST_MOCKER} (dist/index.js, mockObject)`,
          VITEST_LICENSE,
        ) + body + "\n\nexport { mockObject };\n",
      );
    }

    await write(
      "chai.js",
      header(
        `chai@${CHAI} (index.js)`,
        "Copyright (c) 2017 Chai.js Assertion Library. MIT License, see LICENSE-chai.",
      ) +
        await Deno.readTextFile(join(chai, "index.js")),
    );
    await write(
      "magic_string.js",
      header(
        `magic-string@${MAGIC_STRING} (dist/index.mjs)`,
        "Copyright 2018 Rich Harris. MIT License, see LICENSE-magic-string.",
      ) +
        rewriteImports(
          "magic_string.js",
          await Deno.readTextFile(join(magic, "dist/index.mjs")),
        ),
    );
    await write(
      "sourcemap_codec.js",
      header(
        `@jridgewell/sourcemap-codec@${SOURCEMAP_CODEC} (dist/sourcemap-codec.mjs)`,
        "Copyright 2024 Justin Ridgewell. MIT License, see LICENSE-sourcemap-codec.",
      ) +
        (await Deno.readTextFile(join(codec, "dist/sourcemap-codec.mjs")))
          .replace(/\n\/\/# sourceMappingURL=.*\n?$/, "\n"),
    );
    {
      const code = await Deno.readTextFile(join(expectType, "dist/index.js"));
      const body = slice(code, "const fn = () => true;", "\n};", "expect-type")
        .replaceAll("(0, exports.expectTypeOf)", "expectTypeOf")
        .replaceAll("exports.expectTypeOf", "expectTypeOf");
      await write(
        "expect_type.js",
        header(
          `expect-type@${EXPECT_TYPE} (dist/index.js, runtime only)`,
          "Copyright 2024 Misha Kaletsky. Apache-2.0 License, see LICENSE-expect-type.",
        ) +
          body + "\n\nexport { expectTypeOf };\n",
      );
    }

    await Deno.copyFile(
      join(vitest, "LICENSE.md"),
      join(OUT_DIR, "LICENSE-vitest.md"),
    );
    await Deno.copyFile(join(chai, "LICENSE"), join(OUT_DIR, "LICENSE-chai"));
    await Deno.copyFile(
      join(magic, "LICENSE"),
      join(OUT_DIR, "LICENSE-magic-string"),
    );
    await Deno.copyFile(
      join(codec, "LICENSE"),
      join(OUT_DIR, "LICENSE-sourcemap-codec"),
    );
    await Deno.copyFile(
      join(expectType, "LICENSE"),
      join(OUT_DIR, "LICENSE-expect-type"),
    );
    await Deno.writeTextFile(
      join(OUT_DIR, "VERSIONS.json"),
      JSON.stringify(
        {
          "vitest": VITEST,
          "@vitest/mocker": VITEST_MOCKER,
          "chai": CHAI,
          "magic-string": MAGIC_STRING,
          "@jridgewell/sourcemap-codec": SOURCEMAP_CODEC,
          "expect-type": EXPECT_TYPE,
        },
        null,
        2,
      ) + "\n",
    );
    console.log(`Vendored vitest@${VITEST} runtime into ${OUT_DIR}`);
  } finally {
    await Deno.remove(tmp, { recursive: true });
  }
}

await main();
