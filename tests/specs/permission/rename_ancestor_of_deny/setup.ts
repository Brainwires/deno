Deno.mkdirSync("base/a/b", { recursive: true });
Deno.writeTextFileSync("base/a/b/prog", "packaged");
Deno.mkdirSync("base/other/x", { recursive: true });
Deno.mkdirSync("evil/b", { recursive: true });
Deno.writeTextFileSync("evil/b/prog", "replaced");
console.log("setup done");
