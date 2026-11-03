// node:wasi's path_unlink_file, path_remove_directory and path_rename act on
// the entry named, never on what a symlink at it points to (as unlink, rmdir
// and rename do, and WASI says). Before, the final link was followed: the
// target was unlinked or moved and the link left dangling, and a link out of
// the preopen couldn't be removed at all. A link that is a parent of a denied
// path gets the strict check a recursive remove has, as Deno.remove gives it.
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
const ERRNO_NOTDIR = 54;
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
  return code === ERRNO_SUCCESS
    ? "done"
    : code === ERRNO_ACCES
    ? "denied"
    : code === ERRNO_NOTDIR
    ? "not a directory"
    : code === ERRNO_NOTCAPABLE
    ? "outside the preopen"
    : `errno ${code}`;
}
function unlink(path: string) {
  return errno(imports.path_unlink_file(PREOPEN_FD, ...put(path)));
}
function rmdir(path: string) {
  return errno(imports.path_remove_directory(PREOPEN_FD, ...put(path)));
}
function rename(from: string, to: string) {
  return errno(
    imports.path_rename(PREOPEN_FD, ...put(from), PREOPEN_FD, ...put(to)),
  );
}
function kind(path: string) {
  try {
    const stat = Deno.lstatSync(path);
    return stat.isSymlink ? "link" : stat.isDirectory ? "folder" : "file";
  } catch {
    return "absent";
  }
}

console.log("unlink a link to a file:", unlink("link"));
console.log("  link:", kind("base/link"), " target:", kind("base/target"));
console.log("unlink a link out of the preopen:", unlink("out"));
console.log("  link:", kind("base/out"), " secret:", kind("outside/secret"));
console.log("rmdir a link to a folder:", rmdir("dirlink"));
console.log(
  "  link:",
  kind("base/dirlink"),
  " folder:",
  kind("base/emptydir"),
);
console.log("rename a link to a file:", rename("link2", "renamed"));
console.log(
  "  link:",
  kind("base/link2"),
  " renamed:",
  kind("base/renamed"),
  " target:",
  kind("base/target2"),
);

// base/alink is a parent of the denied base/alink/b (base/a/b through the
// link): removing the link takes every path under it away, and renaming it
// moves them, so both get the strict check (before, the link was followed:
// the folder base/a itself was unlinked or renamed).
console.log(
  "unlink a link that is a parent of a denied path:",
  unlink("alink"),
);
console.log("  link:", kind("base/alink"));
console.log(
  "rename a link that is a parent of a denied path:",
  rename("alink", "amoved"),
);
console.log(
  "  link:",
  kind("base/alink"),
  " moved:",
  kind("base/amoved"),
  " denied path:",
  kind("base/alink/b/prog"),
);
console.log("unlink the denied file:", unlink("a/b/prog"));
console.log("still packaged:", kind("base/a/b/prog") === "file");
console.log("rmdir a folder:", rmdir("emptydir"));
console.log("  folder:", kind("base/emptydir"));
