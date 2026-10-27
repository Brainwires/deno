// what approve-scripts wrote, where, and whether the approved script ran
const pkgJson = JSON.parse(Deno.readTextFileSync("package.json"));
console.log(JSON.stringify(pkgJson.deno));
let denoJson = "absent";
try {
  Deno.statSync("deno.json");
  denoJson = "present";
} catch {
  // absent
}
console.log(`deno.json: ${denoJson}`);
console.log(
  Deno.readTextFileSync(
    "node_modules/@denotest/lifecycle-scripts-simple/message.js",
  ).trim(),
);
