// Every value below runs JS (and so ops) while process.send() serializes it.
const json = process.argv[2] === "json";
const msg = {
  // JSON: process.env itself, a proxy whose traps call op_get_env.
  // advanced: V8's serializer can't clone a proxy, so a copy.
  env: json ? process.env : { IPC_PROBE: process.env.IPC_PROBE },
  get viaGetter() {
    return process.env.IPC_PROBE + "-getter";
  },
  viaToJSON: json ? { toJSON: () => process.env.IPC_PROBE + "-toJSON" } : "n/a",
};
process.send(msg, () => process.disconnect());
