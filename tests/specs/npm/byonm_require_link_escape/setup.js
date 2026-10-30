// A bring-your-own-node_modules project whose node_modules links out of
// node_modules: `evil` straight to `outside`, `chain` to `src`, whose `u`
// links on to `outside`, and `lib` (outside node_modules) to `outside` too.
// `pkg` is a pnpm-style link into node_modules/.pnpm, which stays readable.
const root = Deno.cwd();
const linkType = Deno.build.os === "windows" ? "junction" : "dir";
const pnpmPkg = `${root}/node_modules/.pnpm/pkg@1.0.0/node_modules/pkg`;

Deno.mkdirSync(`${root}/outside`, { recursive: true });
Deno.mkdirSync(`${root}/src`, { recursive: true });
Deno.mkdirSync(pnpmPkg, { recursive: true });
Deno.writeTextFileSync(
  `${root}/outside/config.json`,
  '{ "secret": "outside" }\n',
);
Deno.writeTextFileSync(
  `${root}/outside/mod.cjs`,
  "console.log('outside/mod.cjs ran');\nmodule.exports = 'outside';\n",
);
Deno.writeTextFileSync(
  `${pnpmPkg}/package.json`,
  '{ "name": "pkg", "version": "1.0.0", "main": "index.js" }\n',
);
Deno.writeTextFileSync(`${pnpmPkg}/index.js`, "module.exports = 'pkg';\n");

Deno.symlinkSync(`${root}/outside`, `${root}/node_modules/evil`, {
  type: linkType,
});
Deno.symlinkSync(`${root}/src`, `${root}/node_modules/chain`, {
  type: linkType,
});
Deno.symlinkSync(`${root}/outside`, `${root}/src/u`, { type: linkType });
Deno.symlinkSync(`${root}/outside`, `${root}/lib`, { type: linkType });
Deno.symlinkSync(pnpmPkg, `${root}/node_modules/pkg`, { type: linkType });
