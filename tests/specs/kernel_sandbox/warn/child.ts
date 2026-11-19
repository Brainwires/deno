const out = await new Deno.Command("cat", { args: ["outside.txt"] }).output();
console.log("the child read it:", out.success);
