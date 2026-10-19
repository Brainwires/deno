const bundle = await Deno.readTextFile("out.js");
if (bundle.includes("NEW_CHECKOUT_MARKER")) {
  console.log("the disabled branch was not eliminated:\n" + bundle);
  Deno.exit(1);
}
if (bundle.includes("FEATURE_NEW")) {
  console.log("FEATURE_NEW was not replaced:\n" + bundle);
  Deno.exit(1);
}
console.log("ok");
