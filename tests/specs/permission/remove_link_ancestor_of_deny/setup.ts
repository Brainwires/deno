// base/c is a link to a folder holding the denied base/c/b: a symlink, or on
// Windows a junction (which needs no privilege to make).
Deno.mkdirSync("base", { recursive: true });
Deno.mkdirSync("evil/b", { recursive: true });
Deno.writeTextFileSync("evil/b/prog", "packaged");
const evil = `${Deno.cwd()}/evil`;
function link(path: string) {
  if (Deno.build.os === "windows") {
    const { success, stdout, stderr } = new Deno.Command("cmd", {
      args: [
        "/c",
        "mklink",
        "/J",
        path.replaceAll("/", "\\"),
        evil.replaceAll("/", "\\"),
      ],
    }).outputSync();
    if (!success) {
      const decoder = new TextDecoder();
      throw new Error(
        `mklink /J ${path} failed: ${decoder.decode(stdout)}${
          decoder.decode(stderr)
        }`,
      );
    }
  } else {
    Deno.symlinkSync(evil, path);
  }
}
link("base/c");
link("base/other");
console.log("setup done");
