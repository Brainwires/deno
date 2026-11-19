// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// A confined compiled program keeps its code cache in its own cache folder
// (<temp>/deno-compile-<name>.sandboxed/<policy hash>, the user's alone),
// never in the temp folder's shared file an unconfined run of a program
// with the same name reads.
const name = "p9_cache_prog";
const temp = Deno.realPathSync(Deno.env.get("TMPDIR") ?? "/tmp");
const base = `${temp}/deno-compile-${name}.sandboxed`;
const shared = `${temp}/deno-compile-${name}.cache`;
for (const path of [base, shared]) {
  try {
    Deno.removeSync(path, { recursive: true });
  } catch {
    // not there
  }
}
const exists = (path: string) => {
  try {
    Deno.statSync(path);
    return true;
  } catch {
    return false;
  }
};
for (let run = 1; run <= 2; run++) {
  const out = new Deno.Command(`./${name}`, { stdout: "piped" }).outputSync();
  console.log(`run ${run}: ${new TextDecoder().decode(out.stdout).trim()}`);
}
const dirs = [...Deno.readDirSync(base)].filter((e) => e.isDirectory);
console.log(`policy folders: ${dirs.length}`);
const dir = `${base}/${dirs[0].name}`;
console.log(`code cache there: ${exists(`${dir}/code.cache`)}`);
console.log(`shared code cache: ${exists(shared)}`);
const mode = (path: string) => (Deno.statSync(path).mode! & 0o777).toString(8);
console.log(`modes: ${mode(base)} ${mode(dir)}`);
Deno.removeSync(base, { recursive: true });
