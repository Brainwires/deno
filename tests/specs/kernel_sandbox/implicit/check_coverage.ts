// --coverage=cov under enforce: the runtime writes the profile for the
// user, with no write grant
const files = [...Deno.readDirSync("cov")].filter((e) =>
  e.name.endsWith(".json")
);
console.log("coverage files:", files.length > 0);
