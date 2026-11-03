// A rename moves the whole tree under its old path and puts a tree in place
// of its new path: either one holding a denied path is refused, as a recursive
// remove is, so the denied files can't be moved away and another tree can't be
// put in their place or in the place of a denied path that doesn't exist yet.
import { renameSync as nodeRenameSync } from "node:fs";
import { rename as nodeRename } from "node:fs/promises";

function attempt(name: string, fn: () => void) {
  try {
    fn();
    console.log(`${name}: renamed (BUG)`);
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
async function attemptAsync(name: string, fn: () => Promise<void>) {
  try {
    await fn();
    console.log(`${name}: renamed (BUG)`);
  } catch (e) {
    console.log(
      `${name}: ${e instanceof Deno.errors.NotCapable ? "denied" : e}`,
    );
  }
}

attempt(
  "Deno.renameSync parent",
  () => Deno.renameSync("base/a", "base/moved"),
);
await attemptAsync(
  "Deno.rename parent",
  () => Deno.rename("base/a", "base/moved"),
);
// Deny scopes compare paths case-insensitively where the file system does
// (Windows, macOS), so another spelling of the parent is refused too.
if (Deno.build.os === "linux") {
  console.log("Deno.renameSync parent, other case: denied");
} else {
  attempt(
    "Deno.renameSync parent, other case",
    () => Deno.renameSync("base/A", "base/moved"),
  );
}
attempt(
  "Deno.renameSync denied",
  () => Deno.renameSync("base/a/b", "base/moved"),
);
attempt("node renameSync parent", () => nodeRenameSync("base/a", "base/moved"));
await attemptAsync(
  "node rename parent",
  () => nodeRename("base/a", "base/moved"),
);
// base/c/b is denied but absent, base/e/b is denied and base/e is empty: a
// tree moved to base/c or base/e would create the denied path.
attempt(
  "Deno.renameSync onto absent parent",
  () => Deno.renameSync("base/evil", "base/c"),
);
await attemptAsync(
  "Deno.rename onto absent parent",
  () => Deno.rename("base/evil", "base/c"),
);
attempt(
  "Deno.renameSync onto empty parent",
  () => Deno.renameSync("base/evil", "base/e"),
);
attempt(
  "node renameSync onto absent parent",
  () => nodeRenameSync("base/evil", "base/c"),
);
await attemptAsync(
  "node rename onto empty parent",
  () => nodeRename("base/evil", "base/e"),
);
console.log("denied paths absent:", !exists("base/c/b") && !exists("base/e/b"));
console.log(
  "still packaged:",
  Deno.readTextFileSync("base/a/b/prog") === "packaged",
);

// A tree with nothing denied in it still moves.
Deno.renameSync("base/other", "base/other2");
await Deno.rename("base/other2", "base/other");
console.log("other tree moved:", Deno.statSync("base/other/x").isDirectory);
