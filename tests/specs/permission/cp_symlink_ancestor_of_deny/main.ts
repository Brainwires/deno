// node:fs cp recreates a source symlink at its destination. Like
// Deno.symlink(), a link at a parent of a denied path is refused (unscoped
// --allow-write still leaves its --deny-write scopes), whether the
// destination is absent or another link.
import { cpSync } from "node:fs";
import { cp } from "node:fs/promises";

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
function isLink(path: string) {
  try {
    return Deno.lstatSync(path).isSymlink;
  } catch {
    return false;
  }
}

attempt("cpSync link to absent parent", () => cpSync("src/c", "base/c"));
await attemptAsync("cp link to absent parent", () => cp("src/c", "base/c"));
// Absolute paths: a relative one-component destination trips an unrelated
// empty-parent check in cp's tree walk.
attempt(
  "cpSync tree with a link at a parent",
  () => cpSync(`${Deno.cwd()}/src`, `${Deno.cwd()}/base`, { recursive: true }),
);
attempt("cpSync link onto a linked parent", () => cpSync("src/c", "base/d"));
await attemptAsync(
  "cp link onto a linked parent",
  () => cp("src/c", "base/d"),
);
console.log("no link made:", !isLink("base/c"));
console.log(
  "still packaged:",
  Deno.readTextFileSync("base/d/b/prog") === "packaged",
);

// A link anywhere else is still copied, and a tree whose destination has a
// deny scope inside it is copied entry by entry (no clone to clean up).
cpSync("src/c", "base/other");
console.log("other link made:", isLink("base/other"));
cpSync("tree", "base/t", { recursive: true });
console.log(
  "tree copied:",
  Deno.readTextFileSync("base/t/x/f") === "file",
);
