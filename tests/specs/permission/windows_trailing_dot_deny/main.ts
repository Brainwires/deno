// Win32 removes the trailing dots and spaces of a name before the file
// system sees it (`base\a.` and `base\a ` open `base\a`), and `name:stream`
// / `name::$DATA` open a stream of `name` (the default one is the file's
// own data). The permission layer compared such a path as written, so a
// deny scope on `base\a\b` matched neither `base\a.\b` nor `base\a\b::$DATA`
// (DENEXT.md 225). Every spelling is now compared as the entry it opens.
function outcome(fn: () => void) {
  try {
    fn();
    return "done";
  } catch (e) {
    return e instanceof Deno.errors.NotCapable ? "refused" : `error ${e}`;
  }
}
console.log(
  "rename a parent of a denied path, trailing dot:",
  outcome(() => Deno.renameSync("base\\a.", "base\\z")),
);
console.log(
  "rename a parent of a denied path, trailing space:",
  outcome(() => Deno.renameSync("base\\a ", "base\\z")),
);
console.log(
  "rename a parent of a denied path, dots and spaces:",
  outcome(() => Deno.renameSync("base\\a. . .", "base\\z")),
);
console.log(
  "rename onto a parent of a denied path, trailing dot:",
  outcome(() => Deno.renameSync("base\\other", "base\\a.")),
);
console.log(
  "remove a parent of a denied path, trailing dot:",
  outcome(() => Deno.removeSync("base\\a.", { recursive: true })),
);
console.log(
  "remove the denied path, trailing dot:",
  outcome(() => Deno.removeSync("base\\a\\b.", { recursive: true })),
);
console.log(
  "remove inside the denied path through a parent with a trailing dot:",
  outcome(() => Deno.removeSync("base\\a.\\b\\prog")),
);
console.log(
  "write into the denied path through a parent with a trailing dot:",
  outcome(() => Deno.writeTextFileSync("base\\a.\\b\\new.txt", "x")),
);
console.log(
  "symlink at a parent of a denied path, trailing dot:",
  outcome(() => Deno.symlinkSync("base\\other", "base\\a.")),
);
console.log(
  "write the denied file's default stream:",
  outcome(() => Deno.writeTextFileSync("base\\a\\b\\prog::$DATA", "x")),
);
console.log(
  "write a named stream of the denied file:",
  outcome(() => Deno.writeTextFileSync("base\\a\\b\\prog:evil", "x")),
);
console.log(
  "remove the denied file by its default stream:",
  outcome(() => Deno.removeSync("base\\a\\b\\prog::$DATA")),
);
console.log(
  "write a stream of the denied folder:",
  outcome(() => Deno.writeTextFileSync("base\\a\\b:evil", "x")),
);
console.log(
  "write a stream of the denied folder through the index stream:",
  outcome(() =>
    Deno.writeTextFileSync("base\\a\\b::$INDEX_ALLOCATION\\new.txt", "x")
  ),
);
console.log(
  "still packaged:",
  Deno.readTextFileSync("base/a/b/prog") === "packaged" &&
    !Deno.statSync("base/a/b").isSymlink,
);
console.log(
  "write beside the denied path, trailing dot:",
  outcome(() => Deno.writeTextFileSync("base\\other\\x.", "x")),
);
console.log(
  "that wrote the name without the dot:",
  Deno.readTextFileSync("base\\other\\x") === "x",
);
console.log(
  "write a named stream beside the denied path:",
  outcome(() => Deno.writeTextFileSync("base\\other\\x:meta", "m")),
);
console.log(
  "that stream reads back:",
  Deno.readTextFileSync("base\\other\\x:meta") === "m" &&
    Deno.readTextFileSync("base\\other\\x") === "x",
);
