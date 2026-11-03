// `tool` is found through PATH, which the parent spells with the 8.3 alias.
const { state } = Deno.permissions.querySync({ name: "run", command: "tool" });
console.log(`STATE: ${state}`);
