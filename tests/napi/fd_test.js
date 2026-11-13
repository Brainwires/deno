// Copyright 2018-2026 the Deno authors. MIT license.

// node:fs reaches a descriptor a native addon opened (node-pty's pty master
// is one), in the main thread and in workers, as Node's fs reaches every
// descriptor of the process (denext, DENEXT.md item 370).

import { Worker } from "node:worker_threads";
import { assert, assertEquals, libPrefix, libSuffix } from "./common.js";
import { loadTestLibrary } from "./common.js";
import { expected, readAddonPipe } from "./fd_common.js";

const unix = Deno.build.os !== "windows";

Deno.test({
  name: "node:fs reads and closes a descriptor the addon opened",
  ignore: !unix,
}, () => {
  assertEquals(
    readAddonPipe(loadTestLibrary(), "from-addon"),
    expected("from-addon"),
  );
});

Deno.test({
  name: "node:fs reaches an addon's descriptor in a node:worker_threads Worker",
  ignore: !unix,
}, async () => {
  const worker = new Worker(new URL("./fd_worker.js", import.meta.url));
  const { promise, resolve, reject } = Promise.withResolvers();
  worker.once("message", resolve);
  worker.once("error", reject);
  assertEquals(await promise, expected("from-worker-addon"));
  await worker.terminate();
});

Deno.test({
  name: "node:fs reaches an addon's descriptor in a web Worker",
  ignore: !unix,
}, async () => {
  const worker = new globalThis.Worker(
    new URL("./fd_worker.js", import.meta.url),
    { type: "module" },
  );
  const { promise, resolve, reject } = Promise.withResolvers();
  worker.onmessage = (e) => resolve(e.data);
  worker.onerror = (e) => {
    e.preventDefault();
    reject(new Error(e.message));
  };
  assertEquals(await promise, expected("from-worker-addon"));
  worker.terminate();
});

Deno.test({
  name: "an addon's descriptor needs unscoped --allow-ffi",
  ignore: !unix,
}, async () => {
  const lib = Deno.execPath().replace(/[^\/\\]+$/, "") +
    `${libPrefix}test_napi.${libSuffix}`;
  const { code, stdout, stderr } = await new Deno.Command(Deno.execPath(), {
    args: [
      "run",
      "--no-config",
      "--allow-read",
      `--allow-ffi=${lib}`,
      new URL("./fd_scoped_ffi.js", import.meta.url).pathname,
      lib,
    ],
  }).output();
  const out = new TextDecoder().decode(stdout);
  assertEquals(code, 0, new TextDecoder().decode(stderr));
  assert(out.startsWith("refused NotCapable"), out);
});
