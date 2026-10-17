// a different integrity for the tarball, and a fresh node_modules
const lock = JSON.parse(Deno.readTextFileSync("deno.lock"));
for (const key of Object.keys(lock.npmRemote)) {
  if (key.endsWith(".tgz")) {
    lock.npmRemote[key] = "sha512-" + "A".repeat(86) + "==";
  }
}
Deno.writeTextFileSync("deno.lock", JSON.stringify(lock, null, 2) + "\n");
Deno.removeSync("node_modules", { recursive: true });
