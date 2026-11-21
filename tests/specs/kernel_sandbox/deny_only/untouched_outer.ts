// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// Linux: a deny ceiling (-A with a --deny-read, auto at R4) leaves the
// process as it was: no seccomp filter and no no-new-privileges beyond
// what this (unconfined) process already has.
const field = (status: string, key: string) =>
  new RegExp(`^${key}:\\s*(\\S+)`, "m").exec(status)?.[1];
const own = Deno.readTextFileSync("/proc/self/status");
const out = new Deno.Command(Deno.execPath(), {
  args: ["run", "--quiet", "-A", "--deny-read=secret", "status.ts"],
  env: { DENO_INTERNAL_KERNEL_SANDBOX_STAGE: "r4" },
}).outputSync();
const child = new TextDecoder().decode(out.stdout);
for (const key of ["NoNewPrivs", "Seccomp", "Seccomp_filters"]) {
  const [was, now] = [field(own, key), field(child, key)];
  // (the values, when they differ: a child that didn't run has none)
  console.log(
    `${key} unchanged: ${was === now}${
      was === now ? "" : ` (${was} -> ${now})`
    }`,
  );
}
if (!out.success) {
  console.log(`the child failed (${out.code}):`);
  console.log(new TextDecoder().decode(out.stderr));
}
