// runOnMainThread's op checks --allow-ffi first (NotCapable without it);
// with it, a null function is a TypeError and a real one is refused outside
// a desktop app, which has no UI thread to run it on.
const ops = Deno[Deno.internal].core.ops;
const granted = Deno.permissions.querySync({ name: "ffi" }).state ===
  "granted";
const fns = granted ? [Deno.UnsafePointer.create(1n), null] : [null];
for (const fn of fns) {
  try {
    await ops.op_desktop_run_on_main_thread(fn, null);
    console.log("resolved");
  } catch (e) {
    console.log(`${e.name}: ${e.message.split("\n")[0]}`);
  }
}
