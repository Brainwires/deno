Deno.mkdirSync("base/a/b", { recursive: true });
Deno.writeTextFileSync("base/a/b/prog", "packaged");
Deno.mkdirSync("base/evil/b", { recursive: true });
Deno.writeTextFileSync("base/evil/b/prog", "replaced");
Deno.mkdirSync("base/other", { recursive: true });
console.log("setup done");
