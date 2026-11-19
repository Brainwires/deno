const out = await new Deno.Command("git", { args: ["--version"] }).output();
console.log("git ran:", out.success);
