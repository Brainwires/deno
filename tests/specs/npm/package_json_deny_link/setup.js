// node_modules/pkg is a link to `denied`, whose package.json maps
// `pkg/sub` to a file whose name stands for a value read from it.
const linkType = Deno.build.os === "windows" ? "junction" : "dir";
Deno.mkdirSync("node_modules", { recursive: true });
Deno.symlinkSync(`${Deno.cwd()}/denied`, "node_modules/pkg", {
  type: linkType,
});
