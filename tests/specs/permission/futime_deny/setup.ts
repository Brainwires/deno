Deno.mkdirSync("base/a/b", { recursive: true });
Deno.writeTextFileSync("base/a/b/prog", "packaged");
Deno.mkdirSync("base/other");
Deno.writeTextFileSync("base/other/file", "");
console.log("setup done");
