// clearEnv: a GitHub-hosted macOS runner sets DYLD_FALLBACK_LIBRARY_PATH,
// which a scoped --allow-run refuses to hand a child. PATH is set again so
// `cat` still resolves.
const out = await new Deno.Command("cat", {
  args: ["outside.txt"],
  clearEnv: true,
  env: { PATH: "/usr/bin:/bin" },
}).output();
console.log("the child read it:", out.success);
