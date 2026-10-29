const config = JSON.parse(Deno.readTextFileSync("deno.json"));
delete config.patchedDependencies;
Deno.writeTextFileSync("deno.json", JSON.stringify(config, null, 2));
