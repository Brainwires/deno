// Windows: the runtime's pty is not available there yet (DENEXT.md item
// 259), so node-pty's spawn throws the runtime's NotSupported error.
const pty = require("node-pty");

try {
  pty.spawn("cmd.exe", ["/d", "/c", "echo hi"], {});
  console.log("spawned");
} catch (err) {
  console.log(err instanceof Deno.errors.NotSupported, err.message);
}
