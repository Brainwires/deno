// watched/ is the watched tree. watched/secret is a --deny-read scope inside
// it, and watched/link is a symlink to it (a backend that follows links while
// walking the tree reports its events under the link's name too).
Deno.mkdirSync("watched/secret", { recursive: true });
Deno.mkdirSync("watched/ok", { recursive: true });
// `nolink`: no symlink (Windows, where creating one needs a privilege)
if (Deno.args[0] !== "nolink") Deno.symlinkSync("secret", "watched/link");
console.log("setup done");
