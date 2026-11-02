// node:wasi's path_open with O_CREAT through a dangling symlink creates the
// file where the link points. Before, a dangling link's path was checked as
// the link's own place (it doesn't exist, so the lookup fell back to its
// folder), and a link pointing out of the preopen made the file outside it.
// The final component is now followed to where the kernel would create the
// file: outside the preopen is ENOTCAPABLE, a denied place EACCES.
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
const LOOKUPFLAGS_SYMLINK_FOLLOW = 1;
const OFLAGS_CREAT = 1;
const RIGHTS_FD_READ = 1 << 1;
const RIGHTS_FD_WRITE = 1 << 6;
const FD_PTR = 512;
const FILESTAT_PTR = 640;

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
  return code === ERRNO_SUCCESS
    ? "created"
    : code === ERRNO_ACCES
    ? "denied"
    : code === ERRNO_NOTCAPABLE
    ? "outside the preopen"
    : `errno ${code}`;
}
function create(path: string) {
  const code = imports.path_open(
    PREOPEN_FD,
    LOOKUPFLAGS_SYMLINK_FOLLOW,
    ...put(path),
    OFLAGS_CREAT,
    RIGHTS_FD_READ | RIGHTS_FD_WRITE,
    0,
    0,
    FD_PTR,
  );
  if (code === ERRNO_SUCCESS) {
    const fd = new DataView(memory.buffer).getInt32(FD_PTR, true);
    imports.fd_close(fd);
  }
  return errno(code);
}
function stat(path: string) {
  const code = imports.path_filestat_get(
    PREOPEN_FD,
    LOOKUPFLAGS_SYMLINK_FOLLOW,
    ...put(path),
    FILESTAT_PTR,
  );
  return code === ERRNO_SUCCESS ? "found" : errno(code);
}
function exists(path: string) {
  try {
    Deno.lstatSync(path);
    return true;
  } catch {
    return false;
  }
}

console.log("create through a link out (relative):", create("dangling"));
console.log("create through a link out (absolute):", create("dangling_abs"));
console.log("create through a link to such a link:", create("chain"));
console.log("stat through a link out:", stat("dangling"));
console.log(
  "nothing made outside:",
  [...Deno.readDirSync("outside")].length === 0,
);
console.log("create through a link into a denied path:", create("to_denied"));
console.log("nothing made in the denied path:", !exists("base/a/b/new4"));

// Inside the preopen the file is made where the link points, as before.
console.log("create through a link inside:", create("inner_dangling"));
console.log("  made at the target:", exists("base/inner/new3"));
console.log("create a plain new file:", create("inner/new5"));
console.log("  made:", exists("base/inner/new5"));
