// Vendored from vitest@5.0.3 (dist/chunks/constants.-juJ8b_4.js) by tools/vendor_vitest.ts. Do not edit.
// Copyright (c) 2021-Present VoidZero Inc. and Vitest contributors. MIT License, see LICENSE-vitest.md (which also lists bundled code).

// if changed, update also jsdocs and docs
const defaultPort = 51204;
const defaultBrowserPort = 63315;
const defaultInspectPort = 9229;
const API_PATH = "/__vitest_api__";
const CONFIG_NAMES = ["vitest.config", "vite.config"];
const CONFIG_EXTENSIONS = [
	".ts",
	".mts",
	".cts",
	".js",
	".mjs",
	".cjs"
];
const configFiles = CONFIG_NAMES.flatMap((name) => CONFIG_EXTENSIONS.map((ext) => name + ext));
const globalApis = [
	"suite",
	"test",
	"describe",
	"it",
	"chai",
	"expect",
	"assert",
	"expectTypeOf",
	"assertType",
	"vitest",
	"vi",
	"beforeAll",
	"afterAll",
	"beforeEach",
	"afterEach",
	"onTestFinished",
	"onTestFailed",
	"aroundEach",
	"aroundAll"
];

export { API_PATH as A, defaultPort as a, defaultInspectPort as b, configFiles as c, defaultBrowserPort as d, globalApis as g };
