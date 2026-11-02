// node:wasi's path_rename and path_symlink refuse a path that holds a denied
// path, as Deno.rename and Deno.symlink do: a denied path's parent can't be
// moved away, and another tree can't be put in its place.
import { WASI } from "node:wasi";

// (module (memory 1) (export "memory" (memory 0)))
// deno-fmt-ignore
const MEMONLY_WASM = new Uint8Array([
  0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x03, 0x01, 0x00,
  0x01, 0x07, 0x0a, 0x01, 0x06, 0x6d, 0x65, 0x6d, 0x6f, 0x72, 0x79, 0x02,
  0x00,
]);
const ERRNO_SUCCESS = 0;
const ERRNO_ACCES = 2;
const PREOPEN_FD = 3;

const wasi = new WASI({
  version: "preview1",
  preopens: { "/sandbox": `${Deno.cwd()}/base` },
});
// Let the constructor's ExperimentalWarning print before the results.
await new Promise((resolve) => setTimeout(resolve, 0));
const instance = new WebAssembly.Instance(
  new WebAssembly.Module(MEMONLY_WASM),
  {},
);
wasi.initialize(instance);
const memory = new Uint8Array(
  (instance.exports.memory as WebAssembly.Memory).buffer,
);
// deno-lint-ignore no-explicit-any
const imports = wasi.wasiImport as any;

let offset = 1024;
function put(text: string): [number, number] {
  const bytes = new TextEncoder().encode(text);
  memory.set(bytes, offset);
  const at = offset;
  offset += bytes.length + 8;
  return [at, bytes.length];
}
function errno(code: number) {
  return code === ERRNO_ACCES
    ? "denied"
    : code === ERRNO_SUCCESS
    ? "done (BUG)"
    : `errno ${code}`;
}
function rename(from: string, to: string) {
  return imports.path_rename(PREOPEN_FD, ...put(from), PREOPEN_FD, ...put(to));
}
function exists(path: string) {
  try {
    Deno.lstatSync(path);
    return true;
  } catch {
    return false;
  }
}

console.log("rename parent:", errno(rename("a", "moved")));
console.log("rename onto absent parent:", errno(rename("evil", "c")));
console.log(
  "symlink at absent parent:",
  errno(imports.path_symlink(...put("evil"), PREOPEN_FD, ...put("c"))),
);
console.log(
  "still packaged:",
  Deno.readTextFileSync("base/a/b/prog") === "packaged",
);
console.log("denied path absent:", !exists("base/c"));

// A tree with nothing denied in it still moves.
console.log("rename other:", rename("other", "other2"));
console.log("other moved:", Deno.statSync("base/other2").isDirectory);
