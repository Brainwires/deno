import process from "node:process";

const state = (name) => Deno.permissions.querySync({ name }).state;
process.send({
  // The parent's grants, not `-A`.
  read: state("read"),
  env: state("env"),
  net: state("net"),
  kv: typeof Deno.openKv,
  inherited: ["--allow-read", "--unstable-kv", "--v8-flags=--expose-gc"]
    .every((flag) => process.execArgv.includes(flag)),
});
