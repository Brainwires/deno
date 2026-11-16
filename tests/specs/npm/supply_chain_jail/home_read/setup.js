// The fixture's home directory gets a credential file, a shell history, a
// version pin a tool looks for in parent directories and a sibling project
// next to the two projects that install the package (one granting it a
// host, one not); each project has a secret file and a link to the sibling.
// Written here so no dotfile is checked in.
const home = `${Deno.cwd()}/home`;
Deno.mkdirSync(`${home}/.fly`, { recursive: true });
Deno.mkdirSync(`${home}/sibling`, { recursive: true });
Deno.writeTextFileSync(`${home}/.fly/config.yml`, "access_token: fly-secret\n");
Deno.writeTextFileSync(`${home}/.zsh_history`, "export TOKEN=secret\n");
Deno.writeTextFileSync(`${home}/.nvmrc`, "22\n");
Deno.writeTextFileSync(`${home}/sibling/.env`, "SECRET=sibling\n");
Deno.writeTextFileSync(`${home}/sibling/notes.txt`, "notes\n");
for (const proj of ["proj_net", "proj_offline"]) {
  Deno.writeTextFileSync(`${home}/${proj}/.env`, "SECRET=project\n");
  Deno.symlinkSync("../sibling", `${home}/${proj}/to_sibling`);
}
