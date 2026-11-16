// Copyright 2018-2026 the Deno authors. MIT license.
// Builds the library twice, under different names (dyld would hand back an
// image already loaded under the same name): inside the read scope (lib/)
// and outside it (other/). Their absolute paths go in lib/paths.json, which
// the confined run can read.
const isMac = Deno.build.os === "darwin";
const ext = isMac ? "dylib" : "so";
const paths: Record<string, string> = {};
for (const [dir, name] of [["lib", "libx"], ["other", "liby"]]) {
  Deno.mkdirSync(dir, { recursive: true });
  const out = `${dir}/${name}.${ext}`;
  const args = isMac
    ? ["-dynamiclib", "-install_name", `@rpath/${name}.dylib`, "-o", out, "x.c"]
    : ["-shared", "-fPIC", "-o", out, "x.c"];
  const o = new Deno.Command("cc", { args }).outputSync();
  if (!o.success) throw new Error(new TextDecoder().decode(o.stderr));
  paths[dir] = Deno.realPathSync(out);
}
Deno.writeTextFileSync("lib/paths.json", JSON.stringify(paths));
