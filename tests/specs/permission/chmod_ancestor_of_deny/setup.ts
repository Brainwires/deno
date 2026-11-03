Deno.mkdirSync("base/a/b", { recursive: true });
Deno.writeTextFileSync("base/a/b/prog", "packaged");
Deno.writeTextFileSync("base/a/sibling", "");
Deno.mkdirSync("base/other");
Deno.chmodSync("base/a", 0o755);
Deno.symlinkSync("a", "base/link");
console.log("setup done");
