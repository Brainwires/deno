// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// With --allow-run=sh,./bin/helper: each form of an exec call, made by the
// shell and by the helper, for a program on the list and for one that is
// not (bin/other). A started program prints "ok" (the helper) or
// "ran other"; a refused call prints "refused".
const text = (bytes: Uint8Array) => new TextDecoder().decode(bytes).trim();

function run(program: string, args: string[]): string {
  const out = new Deno.Command(program, {
    args,
    stdout: "piped",
    stderr: "null",
  }).outputSync();
  const said = text(out.stdout);
  if (said !== "") return said;
  // a shell whose exec was refused: "permission denied", 126
  return out.code === 126 ? "refused" : `exit ${out.code}`;
}

const sh = (script: string, ...args: string[]) =>
  run("sh", ["-c", script, "sh", ...args]);
const helper = (...args: string[]) => run("./bin/helper", args);

// the listed programs, in every form
console.log(`sh: ${sh("echo ok")}`);
console.log(`sh, helper: ${sh("./bin/helper ok")}`);
console.log(`helper: ${helper("ok")}`);
console.log(
  `helper, itself by descriptor: ${helper("descriptor", "bin/helper")}`,
);
console.log(`helper, itself again: ${helper("again")}`);

// a program that is not listed, in every form
const loader = helper("interp", "bin/other");
console.log(`sh, path: ${sh("./bin/other")}`);
console.log(`sh, loader: ${sh('"$1" ./bin/other', loader)}`);
console.log(`helper, path: ${helper("path", "bin/other")}`);
console.log(`helper, loader: ${helper("loader", "bin/other")}`);
console.log(`helper, memory file: ${helper("memory", "bin/other")}`);
console.log(
  `helper, memory file by name: ${helper("memory-name", "bin/other")}`,
);
console.log(`helper, descriptor: ${helper("descriptor", "bin/other")}`);
