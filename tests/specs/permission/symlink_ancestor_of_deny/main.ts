// Unscoped --allow-write still leaves its --deny-write scopes. A link made at
// a denied path that doesn't exist yet, or at a parent of one, would put
// another tree in the denied path's place, so it is refused.
import { symlinkSync as nodeSymlinkSync } from "node:fs";
import { symlink as nodeSymlink } from "node:fs/promises";

const evil = `${Deno.cwd()}/evil`;

function attempt(name: string, fn: () => void) {
  try {
    fn();
    console.log(`${name}: linked (BUG)`);
  } catch (e) {
    console.log(
      `${name}: ${e instanceof Deno.errors.NotCapable ? "denied" : e}`,
    );
  }
}
async function attemptAsync(name: string, fn: () => Promise<void>) {
  try {
    await fn();
    console.log(`${name}: linked (BUG)`);
  } catch (e) {
    console.log(
      `${name}: ${e instanceof Deno.errors.NotCapable ? "denied" : e}`,
    );
  }
}
function exists(path: string) {
  try {
    Deno.lstatSync(path);
    return true;
  } catch {
    return false;
  }
}

attempt("Deno.symlinkSync parent", () => Deno.symlinkSync(evil, "base/c"));
await attemptAsync("Deno.symlink parent", () => Deno.symlink(evil, "base/c"));
attempt(
  "Deno.symlinkSync denied",
  () => Deno.symlinkSync(`${evil}/b`, "base/d/b"),
);
attempt("node symlinkSync parent", () => nodeSymlinkSync(evil, "base/c"));
await attemptAsync("node symlink parent", () => nodeSymlink(evil, "base/c"));
console.log("denied paths absent:", !exists("base/c") && !exists("base/d/b"));

// A link anywhere else is still made.
Deno.symlinkSync(evil, "base/other");
console.log("other link made:", Deno.lstatSync("base/other").isSymlink);
