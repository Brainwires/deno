// NTFS names are case-insensitive for every letter, not only ASCII ones:
// base/ärger and base/Ärger are one folder. The deny scopes compared with an
// ASCII-only fold, so the no-follow checks (rename, remove), which keep the
// final name as given, let a denied path's parent be moved or removed under
// another spelling of a non-ASCII letter, and a grant written in one
// spelling didn't cover the other.
function outcome(fn: () => void) {
  try {
    fn();
    return "done";
  } catch (e) {
    return e instanceof Deno.errors.NotCapable ? "refused" : `error ${e}`;
  }
}
console.log(
  "rename a parent of a denied path, other case (Latin):",
  outcome(() => Deno.renameSync("base/ärger", "base/moved")),
);
console.log(
  "rename a parent of a denied path, other case (Cyrillic):",
  outcome(() => Deno.renameSync("base/папка", "base/moved2")),
);
console.log(
  "remove a parent of a denied path, other case:",
  outcome(() => Deno.removeSync("base/ärger", { recursive: true })),
);
console.log(
  "write into the denied path, other case:",
  outcome(() => Deno.writeTextFileSync("base/ärger/b/new.txt", "x")),
);
console.log(
  "still packaged:",
  Deno.readTextFileSync("base/Ärger/b/prog") === "packaged" &&
    Deno.readTextFileSync("base/Папка/b/prog") === "packaged",
);
console.log(
  "write beside the denied path, other case:",
  outcome(() => Deno.writeTextFileSync("base/ärger/other/new.txt", "x")),
);

// A grant written in one spelling covers the other.
const { stdout, stderr } = new Deno.Command(Deno.execPath(), {
  args: [
    "run",
    "--allow-read",
    "--allow-write=base/ärger/other",
    "child_allow.ts",
  ],
}).outputSync();
const decoder = new TextDecoder();
console.log((decoder.decode(stdout) + decoder.decode(stderr)).trim());
