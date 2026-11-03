Deno.mkdirSync("base/a/b", { recursive: true });
Deno.writeTextFileSync("base/a/b/prog", "packaged");
Deno.mkdirSync("base/e", { recursive: true });
Deno.mkdirSync("base/other/x", { recursive: true });
Deno.mkdirSync("base/evil/b", { recursive: true });
Deno.writeTextFileSync("base/evil/b/prog", "replaced");
console.log("setup done");
