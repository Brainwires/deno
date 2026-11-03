Deno.mkdirSync("base/a/b", { recursive: true });
Deno.writeTextFileSync("base/a/b/prog", "packaged");
Deno.mkdirSync("base/inner");
Deno.mkdirSync("outside");
// Dangling links: nothing exists where they point yet.
Deno.symlinkSync("../outside/new", "base/dangling");
Deno.symlinkSync(`${Deno.cwd()}/outside/new2`, "base/dangling_abs");
Deno.symlinkSync("dangling", "base/chain");
Deno.symlinkSync("inner/new3", "base/inner_dangling");
Deno.symlinkSync("a/b/new4", "base/to_denied");
console.log("setup done");
