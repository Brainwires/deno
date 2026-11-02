Deno.mkdirSync("base/Ärger/b", { recursive: true });
Deno.writeTextFileSync("base/Ärger/b/prog", "packaged");
Deno.mkdirSync("base/Ärger/other");
Deno.mkdirSync("base/Папка/b", { recursive: true });
Deno.writeTextFileSync("base/Папка/b/prog", "packaged");
console.log("setup done");
