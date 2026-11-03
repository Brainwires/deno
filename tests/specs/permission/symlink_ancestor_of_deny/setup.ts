Deno.mkdirSync("base/d", { recursive: true });
Deno.mkdirSync("evil/b", { recursive: true });
Deno.writeTextFileSync("evil/b/prog", "replaced");
console.log("setup done");
