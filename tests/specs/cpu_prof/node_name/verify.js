const files = [...Deno.readDirSync(".")]
  .map((f) => f.name)
  .filter((name) => name.endsWith(".cpuprofile"));
if (
  files.length !== 1 ||
  !/^CPU\.\d{8}\.\d{6}\.\d+\.0\.001\.cpuprofile$/.test(files[0])
) {
  throw new Error(`unexpected profiles ${files}`);
}
console.log("CPU.<date>.<time>.<pid>.0.001.cpuprofile");
