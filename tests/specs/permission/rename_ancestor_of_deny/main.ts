// A rename moves the whole tree under its old path: one that holds a denied
// path is refused, as a recursive remove is, so the denied files can't be
// moved away and another tree put in their place.
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
attempt(
  "Deno.renameSync denied",
  () => Deno.renameSync("base/a/b", "base/moved"),
);
attempt("node renameSync parent", () => nodeRenameSync("base/a", "base/moved"));
await attemptAsync(
  "node rename parent",
  () => nodeRename("base/a", "base/moved"),
);
console.log(
  "still packaged:",
  Deno.readTextFileSync("base/a/b/prog") === "packaged",
);

// A tree with nothing denied in it still moves.
Deno.renameSync("base/other", "base/other2");
await Deno.rename("base/other2", "base/other");
console.log("other tree moved:", Deno.statSync("base/other/x").isDirectory);
