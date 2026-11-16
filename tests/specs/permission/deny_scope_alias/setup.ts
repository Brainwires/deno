// real/secret and real/secret2 are the protected directories. The deny
// scopes are written under other names: `rootlink/secret` goes through a
// symlinked directory (as `/tmp/x` does on macOS, where `/tmp` ->
// `/private/tmp`), and `dlink` is itself a symlink to real/secret2.
Deno.mkdirSync("real/secret", { recursive: true });
Deno.mkdirSync("real/secret2", { recursive: true });
Deno.writeTextFileSync("real/secret/file.txt", "SECRET");
Deno.writeTextFileSync("real/secret/remove.txt", "KEEP");
Deno.writeTextFileSync("real/secret2/file.txt", "SECRET2");
Deno.writeTextFileSync("real/ok.txt", "OK");
Deno.symlinkSync("real", "rootlink");
Deno.symlinkSync("real/secret2", "dlink");
// another way in: a link elsewhere that leads into the real location
Deno.mkdirSync("other");
Deno.symlinkSync("../real/secret", "other/via");
console.log("setup done");
