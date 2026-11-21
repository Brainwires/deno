// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// A child that applies a kernel sandbox of its own inside a brokered run
// (Linux stacks it under the parent's). The parent's broker answers for the
// parent's grant, which has a write scope; this process's own has none. So
// it must not keep the way to that broker: not the variable, and not the
// descriptors (native code here could ask the broker itself).
const [dial] = Deno.args;
console.log(
  `keeps the variable that names the parent's dial: ${
    Deno.env.get("DENO_INTERNAL_KERNEL_BROKER_DIAL") !== undefined
  }`,
);
// the parent's dial, as `<fd>:<inode>,...`: here each number is closed, or
// was taken by something else of this process (another inode)
const libc = Deno.dlopen("libc.so.6", {
  fstat: { parameters: ["i32", "buffer"], result: "i32" },
});
const parts = dial.split(",").map((part) => part.split(":").map(Number));
let kept = 0;
for (const [fd, inode] of parts) {
  const st = new Uint8Array(256);
  if (libc.symbols.fstat(fd, st) !== 0) continue;
  // struct stat: st_dev, then st_ino (8 bytes each on x86_64 and aarch64)
  if (new DataView(st.buffer).getBigUint64(8, true) === BigInt(inode)) kept++;
}
console.log(`the parent's dial has ${parts.length} descriptors: ${parts.length === 3}`);
console.log(`of them, still open here: ${kept}`);
// its own grant holds: no write, whatever the parent's broker would answer
try {
  Deno.writeTextFileSync("bout/narrow.txt", "x");
  console.log("write: written");
} catch (err) {
  console.log(`write: ${(err as Error).name}`);
}
