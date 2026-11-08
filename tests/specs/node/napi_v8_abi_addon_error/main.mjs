// Copyright 2018-2026 the Deno authors. MIT license.

import { createRequire } from "node:module";
import process from "node:process";

const macos = Deno.build.os === "darwin";
const dir = "node_modules/v8-abi-fixture/build/Release";
Deno.mkdirSync(dir, { recursive: true });
Deno.writeTextFileSync(
  "node_modules/v8-abi-fixture/package.json",
  JSON.stringify({ name: "v8-abi-fixture", version: "1.0.0" }),
);

// Lazy binding for the first two (as node-gyp builds them), load-time binding
// for the third.
const lazy = macos
  ? ["-undefined", "dynamic_lookup", "-Wl,-no_fixup_chains"]
  : [];
const now = macos
  ? ["-undefined", "dynamic_lookup", "-Wl,-fixup_chains"]
  : ["-Wl,-z,now"];
const builds = [
  ["register", lazy],
  ["init", lazy],
  ["symbol", now],
];
for (const [name, flags] of builds) {
  const out = new Deno.Command("cc", {
    args: [
      `${name}.c`,
      "-shared",
      "-fPIC",
      ...flags,
      "-o",
      `${dir}/${name}.node`,
    ],
  }).outputSync();
  if (!out.success) {
    throw new Error(
      `cc ${name}.c failed: ${new TextDecoder().decode(out.stderr)}`,
    );
  }
}

const require = createRequire(import.meta.url);
function report(name, load) {
  try {
    load();
    console.log(`${name}: loaded (unexpected)`);
  } catch (err) {
    const m = err.message;
    console.log(`${name}:`, {
      plainError: err instanceof Error && !(err instanceof TypeError),
      code: err.code,
      namesFile: m.includes(`${name}.node`),
      namesPackage: m.includes('npm package "v8-abi-fixture"'),
      saysV8Abi: m.includes("legacy Node.js native addon API") &&
        m.includes("V8 C++ ABI") && m.includes("Only Node-API (N-API) addons"),
      reason: m.includes("node_module_register")
        ? "node_module_register"
        : m.includes("node_register_module_v147")
        ? "node_register_module_v147"
        : m.includes("`_ZN2v87Isolate10GetCurrentEv`")
        ? "v8 symbol"
        : m,
    });
  }
}

report("register", () => require(`./${dir}/register.node`));
report("init", () => require(`./${dir}/init.node`));
report("symbol", () => require(`./${dir}/symbol.node`));
report(
  "register",
  () => process.dlopen({ exports: {} }, `${Deno.cwd()}/${dir}/register.node`),
);
console.log("still running");
