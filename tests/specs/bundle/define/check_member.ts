const bundle = await Deno.readTextFile("out.js");
if (bundle.includes("process.env") || bundle.includes("import.meta")) {
  console.log("a define was not replaced:\n" + bundle);
  Deno.exit(1);
}
console.log("ok");
