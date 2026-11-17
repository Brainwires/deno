// the lockfile the first image's graph load updated was written before the
// re-exec (the confined image can't write it)
const lock = JSON.parse(Deno.readTextFileSync("deno.lock"));
console.log(
  "remote locked:",
  Object.keys(lock.remote ?? {}).some((k) => k.includes("print_hello.ts")),
);
console.log(
  "npm locked:",
  Object.keys(lock.npm ?? {}).some((k) => k.startsWith("@denotest/esm-basic")),
);
