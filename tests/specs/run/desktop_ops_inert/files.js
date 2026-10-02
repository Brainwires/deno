// main.js ran with no permissions in this directory (a copy of the spec's
// scripts): it left nothing behind.
console.log([...Deno.readDirSync(".")].map((e) => e.name).sort().join(" "));
