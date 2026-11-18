import process from "node:process";

try {
  process.execve("/bin/sh", [Deno.args[0], "-c", "echo ran"], {});
} catch (err) {
  console.log(err.name === "NotCapable" ? "denied" : `error (${err.name})`);
}
