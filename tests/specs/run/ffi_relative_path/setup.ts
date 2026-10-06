const targetDir = Deno.execPath().replace(/[^\/\\]+$/, "");
const [libPrefix, libSuffix] = {
  darwin: ["lib", "dylib"],
  linux: ["lib", "so"],
  windows: ["", "dll"],
}[Deno.build.os];
const libFileName = `${libPrefix}test_ffi.${libSuffix}`;

Deno.mkdirSync("lib", { recursive: true });
Deno.copyFileSync(`${targetDir}/${libFileName}`, `lib/${libFileName}`);
