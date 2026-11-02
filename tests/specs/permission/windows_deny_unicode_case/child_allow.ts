function outcome(fn: () => void) {
  try {
    fn();
    return "done";
  } catch (e) {
    return e instanceof Deno.errors.NotCapable ? "refused" : `error ${e}`;
  }
}
console.log(
  "grant in one case, write in the other:",
  outcome(() => Deno.writeTextFileSync("base/Ärger/other/granted.txt", "x")),
);
