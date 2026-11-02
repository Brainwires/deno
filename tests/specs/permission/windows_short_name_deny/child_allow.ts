function outcome(fn: () => void) {
  try {
    fn();
    return "done";
  } catch (e) {
    return e instanceof Deno.errors.NotCapable ? "refused" : `error ${e}`;
  }
}
console.log(
  "grant written as a short name, write into the long name:",
  outcome(() => Deno.writeTextFileSync("base/longname/other/granted.txt", "x")),
);
