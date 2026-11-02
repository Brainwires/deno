// base/c is a link to a folder holding the denied base/c/b: a symlink, or on
// Windows a junction (which needs no privilege to make).
Deno.mkdirSync("base", { recursive: true });
Deno.mkdirSync("evil/b", { recursive: true });
Deno.writeTextFileSync("evil/b/prog", "packaged");
const evil = `${Deno.cwd()}/evil`;
function link(path: string) {
  if (Deno.build.os === "windows") {
    const { success } = new Deno.Command("cmd", {
      args: ["/c", "mklink", "/J", path.replaceAll("/", "\\"), evil],
      stdout: "null",
    }).outputSync();
    if (!success) throw new Error(`mklink /J ${path} failed`);
  } else {
    Deno.symlinkSync(evil, path);
  }
}
link("base/c");
link("base/other");
console.log("setup done");
