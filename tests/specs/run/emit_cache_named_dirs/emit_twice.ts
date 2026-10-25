// Runs mod.ts with its usual options and with verbatimModuleSyntax
// forced off, sharing one DENO_DIR, then lists the emits of mod.ts in it.
const denoDir = `${Deno.cwd()}/deno_dir`;
for (const env of [{}, { DENO_DISABLE_VERBATIM_MODULE_SYNTAX: "1" }, {}]) {
  const { code, stdout } = await new Deno.Command(Deno.execPath(), {
    args: ["run", "mod.ts"],
    env: { ...env, DENO_DIR: denoDir },
  }).output();
  console.log(code, new TextDecoder().decode(stdout).trim());
}

const found: string[] = [];
async function walk(dir: string, rel: string[]) {
  for await (const entry of Deno.readDir(dir)) {
    const path = `${dir}/${entry.name}`;
    if (entry.isDirectory) await walk(path, [...rel, entry.name]);
    else if (entry.name === "mod.ts.js") found.push(rel[0]);
  }
}
await walk(`${denoDir}/gen`, []);
console.log(found.sort().join("\n"));
