// --cpu-prof-dir under enforce: the runtime writes the profile for the user
const files = [...Deno.readDirSync("prof")].filter((e) =>
  e.name.endsWith(".cpuprofile")
);
console.log("cpu profiles:", files.length > 0);
