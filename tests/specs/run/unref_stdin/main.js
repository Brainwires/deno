// Regression test for https://github.com/denoland/deno/issues/22453: an
// unref'd read on stdin must not keep the process alive.
//
// The read has to stay pending for the whole run, so the program runs in a
// child whose stdin is a pipe this process holds open until the child exits.
// (The harness's own stdin pipe closes as soon as the process starts, so the
// read could finish at EOF and print 2 before the event loop ended.)

if (Deno.args[0] === "child") {
  const { core } = Deno[Deno.internal];
  const opPromise = core.read(Deno.stdin.rid, new Uint8Array(10));
  core.unrefOpPromise(opPromise);

  async function main() {
    console.log(1);
    await opPromise;
    console.log(2);
  }

  main();
} else {
  const child = new Deno.Command(Deno.execPath(), {
    args: ["run", import.meta.filename, "child"],
    stdin: "piped",
    stdout: "inherit",
    stderr: "inherit",
  }).spawn();
  // stdin stays open (no data, no EOF) until the child has exited on its own.
  const status = await child.status;
  await child.stdin.close();
  Deno.exitCode = status.code;
}
