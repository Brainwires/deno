// node:wasi's path_symlink stores a target that is resolved only when the
// link is followed. Under the preopen's scoped grant the target, resolved
// from the link's folder, must stay inside the preopen (ENOTCAPABLE) and be
// readable and writable (EACCES), so host code writing under the preopen
// can't be led out of it or into a denied path.
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
const ERRNO_NOTCAPABLE = 76;
const PREOPEN_FD = 3;

const wasi = new WASI({
  version: "preview1",
  preopens: { "/sandbox": `${Deno.cwd()}/base` },
});
// The constructor's ExperimentalWarning goes to stderr before or after the
// results (main.out allows both).
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
    : code === ERRNO_NOTCAPABLE
    ? "outside the preopen"
    : code === ERRNO_SUCCESS
    ? "linked"
    : `errno ${code}`;
}
function symlink(target: string, link: string) {
  return errno(imports.path_symlink(...put(target), PREOPEN_FD, ...put(link)));
}
function exists(path: string) {
  try {
    Deno.lstatSync(path);
    return true;
  } catch {
    return false;
  }
}

console.log("relative escape:", symlink("../outside", "l1"));
console.log("absolute escape:", symlink(`${Deno.cwd()}/outside`, "l2"));
console.log("escape after a name:", symlink("other/../../outside", "l3"));
console.log("denied target:", symlink("a/b", "l4"));
console.log("file in a denied target:", symlink("a/b/prog", "l5"));
console.log(
  "no link made:",
  ["l1", "l2", "l3", "l4", "l5"].every((l) => !exists(`base/${l}`)),
);

// Targets inside the preopen, existing or not, are still linked.
console.log("inside:", symlink("other", "l6"));
console.log("inside, not there yet:", symlink("other/new", "l7"));
console.log("parent of a denied path:", symlink("a", "l8"));
