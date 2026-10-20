// The parent may only listen on 127.0.0.1 (directional model). A worker
// inheriting `net` gets exactly that: listen allowed, connect denied. The
// single `net` option asks for both directions, so a list or `true` is
// refused unless the parent has both for those hosts (it has no connect).
async function run(name: string, net: Deno.PermissionOptionsObject["net"]) {
  let worker: Worker;
  try {
    worker = new Worker(import.meta.resolve("./worker.ts"), {
      type: "module",
      deno: { permissions: { net, read: true } },
    });
  } catch (e) {
    console.log(`${name}: ${(e as Error).name}: ${(e as Error).message}`);
    return;
  }
  await new Promise<void>((resolve) => {
    worker.onmessage = (e) => {
      console.log(`${name}: ${e.data}`);
      worker.terminate();
      resolve();
    };
  });
}

await run("inherit", "inherit");
await run("same host", ["127.0.0.1"]);
await run("all hosts", true);
await run("other host", ["example.com"]);
