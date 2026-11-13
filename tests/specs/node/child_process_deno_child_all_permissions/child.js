const names = ["read", "write", "net", "env", "run", "ffi", "sys"];
const states = names.map((name) =>
  `${name}=${Deno.permissions.querySync({ name }).state}`
);
const line = states.join(" ");
if (process.send) {
  process.send(line, () => process.disconnect());
} else {
  console.log(line);
}
