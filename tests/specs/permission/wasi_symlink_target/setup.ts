Deno.mkdirSync("base/a/b", { recursive: true });
Deno.writeTextFileSync("base/a/b/prog", "packaged");
Deno.mkdirSync("base/other");
Deno.mkdirSync("outside");
Deno.writeTextFileSync("outside/secret", "secret");
console.log("setup done");
