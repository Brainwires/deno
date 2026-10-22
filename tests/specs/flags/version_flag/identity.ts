// denext (DENEXT.md item 99): `Deno.build.denext` and the identity line of
// `deno --version` name the same runtime version.
const { stdout } = await new Deno.Command(Deno.execPath(), {
  args: ["--version"],
}).output();
const lines = new TextDecoder().decode(stdout).trimEnd().split("\n");
console.log(typeof Deno.build.denext);
console.log(lines.at(-1) === `denext ${Deno.build.denext} (Brainwires)`);
console.log(lines[0].startsWith(`deno ${Deno.version.deno} (`));
