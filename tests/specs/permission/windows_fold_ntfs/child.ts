// Writes a file at the path given; prints whether the permission layer let
// it (a missing folder is `done` too: the check passed, the OS refused).
try {
  Deno.writeTextFileSync(Deno.args[0], "x");
  console.log("done");
} catch (e) {
  console.log(e instanceof Deno.errors.NotCapable ? "refused" : "done");
}
