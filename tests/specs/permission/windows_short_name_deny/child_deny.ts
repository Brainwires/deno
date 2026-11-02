function outcome(fn: () => void) {
  try {
    fn();
    return "done";
  } catch (e) {
    return e instanceof Deno.errors.NotCapable ? "refused" : `error ${e}`;
  }
}
console.log(
  "deny scope written as a short name, write into the long name:",
  outcome(() => Deno.writeTextFileSync("base/longname/b/new.txt", "x")),
);
console.log(
  "deny scope written as a short name, rename the long name:",
  outcome(() => Deno.renameSync("base/longname", "base/moved")),
);
