// Copyright 2018-2026 the Deno authors. MIT license.

// Writes an npm package with files nested deeper than the 2 MiB (release) /
// 8 MiB (debug) blocking-pool threads could analyze (denoland/deno#36746).
// The program imports only index.cjs; `deno compile` still analyzes every
// embedded file:
// - deep.cjs: module.exports = [[[…]]], DEPTH deep. Nothing imports it, as
//   with the generated files in the issue's node_modules.
// - types.d.ts: a generic nested far deeper than any analysis stack.
//   Declaration files never run, so `deno compile` must not analyze them.
// The package has an `import` condition and no "type", so its `.d.ts`
// counts as CJS (#35166).
const DEPTH = 5000;
const DTS_DEPTH = 200_000;
const dir = "node_modules/deep";
Deno.mkdirSync(dir, { recursive: true });
Deno.writeTextFileSync(
  `${dir}/package.json`,
  JSON.stringify({
    name: "deep",
    version: "1.0.0",
    main: "./index.cjs",
    types: "./types.d.ts",
    exports: {
      ".": {
        types: "./types.d.ts",
        import: "./index.cjs",
        require: "./index.cjs",
      },
    },
  }),
);
Deno.writeTextFileSync(`${dir}/index.cjs`, "module.exports = { ok: 1 };\n");
Deno.writeTextFileSync(
  `${dir}/deep.cjs`,
  `module.exports = ${"[".repeat(DEPTH)}${"]".repeat(DEPTH)};\n`,
);
Deno.writeTextFileSync(
  `${dir}/types.d.ts`,
  `export type T = ${"Array<".repeat(DTS_DEPTH)}string${
    ">".repeat(DTS_DEPTH)
  };\n`,
);
