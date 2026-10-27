// `deno run -A` inside the jail: Deno.symlink within the package's folder,
// and out of it.
const dir = new URL(".", import.meta.url).pathname;
const lines = [];
function attempt(name, fn) {
  try {
    fn();
    lines.push(`deno run -A: ${name}: allowed`);
  } catch (err) {
    lines.push(`deno run -A: ${name}: denied (${err.name})`);
  }
}
attempt("Deno.symlinkSync in own dir", () =>
  Deno.symlinkSync("index.js", `${dir}deno-link`));
try {
  await Deno.symlink("index.js", `${dir}deno-link-async`);
  lines.push("deno run -A: Deno.symlink in own dir: allowed");
} catch (err) {
  lines.push(`deno run -A: Deno.symlink in own dir: denied (${err.name})`);
}
attempt("Deno.symlinkSync to /", () => Deno.symlinkSync("/", `${dir}root`));
Deno.writeTextFileSync(`${dir}results.txt`, lines.join("\n") + "\n", {
  append: true,
});
