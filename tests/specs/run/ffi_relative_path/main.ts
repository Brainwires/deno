const [libPrefix, libSuffix] = {
  darwin: ["lib", "dylib"],
  linux: ["lib", "so"],
  windows: ["", "dll"],
}[Deno.build.os];
const libFileName = `${libPrefix}test_ffi.${libSuffix}`;
const sep = Deno.build.os === "windows" ? "\\" : "/";
const symbols = {
  print_something: { parameters: [], result: "void" },
} as const;

function loadMessage(path: string): string {
  try {
    Deno.dlopen(path, symbols).close();
    return "loaded";
  } catch (error) {
    return (error as Error).message;
  }
}

// A path with a separator opens the file under the current directory.
const nested = Deno.dlopen(`./lib/${libFileName}`, symbols);
nested.symbols.print_something();
nested.close();

// The current directory is read at load time, so it follows Deno.chdir().
const cwd = Deno.cwd();
Deno.chdir("lib");
const sibling = Deno.dlopen(`.${sep}${libFileName}`, symbols);
sibling.symbols.print_something();
sibling.close();
Deno.chdir(cwd);

// A missing relative library is reported by its absolute path. (Windows'
// "module not found" message carries no path, so there is nothing to check.)
const missing = loadMessage(`./missing${sep}${libFileName}`);
console.log(
  "relative miss names the absolute path:",
  Deno.build.os === "windows" ||
    missing.includes(`${cwd}${sep}missing${sep}${libFileName}`),
);

// A bare name is handed to the OS search as is, not joined to the cwd.
const bare = loadMessage(`denext_no_such_lib_${libFileName}`);
console.log("bare miss stays bare:", !bare.includes(cwd));
