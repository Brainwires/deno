const files = [...Deno.readDirSync(".")]
  .map((f) => f.name)
  .filter((name) => name.endsWith(".cpuprofile"));
if (files.length !== 1 || !/^CPU\.\d+\.cpuprofile$/.test(files[0])) {
  throw new Error(`unexpected profiles ${files}`);
}
console.log("CPU.<pid>.cpuprofile");
