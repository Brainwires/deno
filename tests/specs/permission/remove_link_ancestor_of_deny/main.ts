// A non-recursive remove of a link (a symlink, or a Windows junction) takes
// away every path reached through it, so a link that is a parent of a denied
// path is refused, as a recursive remove of it is.
import { rmdirSync, rmSync, unlinkSync } from "node:fs";
import { unlink } from "node:fs/promises";

function report(name: string, e?: unknown) {
  if (e === undefined) console.log(`${name}: removed (BUG)`);
  else {
    console.log(
      `${name}: ${e instanceof Deno.errors.NotCapable ? "denied" : e}`,
    );
  }
}
function attempt(name: string, fn: () => void) {
  try {
    fn();
    report(name);
  } catch (e) {
    report(name, e);
  }
}
async function attemptAsync(name: string, fn: () => Promise<void>) {
  try {
    await fn();
    report(name);
  } catch (e) {
    report(name, e);
  }
}

attempt("Deno.removeSync link", () => Deno.removeSync("base/c"));
await attemptAsync("Deno.remove link", () => Deno.remove("base/c"));
attempt("node unlinkSync link", () => unlinkSync("base/c"));
await attemptAsync("node unlink link", () => unlink("base/c"));
attempt("node rmSync link", () => rmSync("base/c"));
attempt("node rmdirSync link", () => rmdirSync("base/c"));
console.log(
  "still packaged:",
  Deno.readTextFileSync("base/c/b/prog") === "packaged",
);

// A link with nothing denied under it is still removed.
Deno.removeSync("base/other");
let removed = false;
try {
  Deno.lstatSync("base/other");
} catch {
  removed = true;
}
console.log("other link removed:", removed);
