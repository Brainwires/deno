Deno.mkdirSync("safe");
Deno.mkdirSync("outside/dir", { recursive: true });
Deno.writeTextFileSync("safe/file.txt", "ok");
Deno.writeTextFileSync("safe/rename.txt", "ok");
Deno.writeTextFileSync("outside/secret.txt", "SECRET");
// a link to a file outside the scope, to a directory outside it, a dangling
// link whose target would be created outside it, and a link inside it
Deno.symlinkSync("../outside/secret.txt", "safe/link");
Deno.symlinkSync("../outside", "safe/dirlink");
Deno.symlinkSync("../outside/new.txt", "safe/dangling");
Deno.symlinkSync("file.txt", "safe/inner");
// a root reached through a symlink (as macOS's /tmp -> /private/tmp)
Deno.symlinkSync(".", "rootlink");
console.log("setup done");
