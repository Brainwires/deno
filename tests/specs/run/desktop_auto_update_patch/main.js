// The patch op is reachable on `core.ops` in a plain `deno run` (the desktop
// runtime's update script takes it off once it holds it). It refuses without
// --allow-ffi; with it, there is no update state outside a desktop app. A
// worker has no such op.
const ops = Deno[Deno.internal].core.ops;

// A bsdiff patch that is all extra block needs nothing of the original.
try {
  ops.op_desktop_apply_patch(new Uint8Array(64), "{}", "AAAA");
  console.log("applied");
} catch (e) {
  console.log(`${e.name}: ${e.message.split("\n")[0]}`);
}

const worker = new Worker(
  `data:application/javascript,${
    encodeURIComponent(`
      const ops = Deno[Deno.internal].core.ops;
      postMessage([
        typeof ops.op_desktop_apply_patch,
        typeof ops.op_desktop_confirm_update,
        typeof ops.op_desktop_verify_ed25519,
      ].join(" "));
    `)
  }`,
  { type: "module" },
);
const inWorker = await new Promise((resolve, reject) => {
  worker.onmessage = (e) => resolve(e.data);
  worker.onerror = (e) => reject(e.error);
});
worker.terminate();
console.log(`worker: ${inWorker}`);

// Nothing staged.
const staged = [...Deno.readDirSync(".")]
  .filter((e) => e.name.endsWith(".update")).length;
console.log(`staged: ${staged}`);
