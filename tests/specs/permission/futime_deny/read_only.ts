// With no write permission at all, a file opened for reading can't have its
// times changed through the descriptor.
function outcome(fn: () => void) {
  try {
    fn();
    return "done";
  } catch (e) {
    return e instanceof Deno.errors.NotCapable ? "refused" : `error ${e}`;
  }
}
using file = Deno.openSync("base/a/b/prog", { read: true });
console.log("utime:", outcome(() => file.utimeSync(1000, 1000)));
console.log(
  "times kept:",
  Deno.statSync("base/a/b/prog").mtime?.getTime() !== 1000_000,
);
