const [libPrefix, libSuffix] = {
  darwin: ["lib", "dylib"],
  linux: ["lib", "so"],
  windows: ["", "dll"],
}[Deno.build.os];
const libFileName = `${libPrefix}test_ffi.${libSuffix}`;
const sep = Deno.build.os === "windows" ? "\\" : "/";

// The unscoped --allow-ffi audit records of main.ts's loads.
const ffi = Deno.readTextFileSync("audit.jsonl")
  .split("\n")
  .filter((line) => line.length > 0)
  .map((line) => JSON.parse(line))
  .filter((record) => record.permission === "ffi")
  .map((record) => record.value as string);

const cwd = Deno.cwd();
console.log(
  "audit records the opened path of ./lib/<lib>:",
  ffi.includes(`${cwd}${sep}lib${sep}${libFileName}`),
);
console.log(
  "audit records no relative path:",
  !ffi.some((value) => value.startsWith(".")),
);
