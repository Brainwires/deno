// Copyright 2018-2026 the Deno authors. MIT license.
import { assert } from "./test_util.ts";

Deno.test(function buildInfo() {
  // Deno.build is injected by rollup at compile time. Here
  // we check it has been properly transformed.
  const { arch, os } = Deno.build;
  assert(arch.length > 0);
  assert(os === "darwin" || os === "windows" || os === "linux");
});

// denext (DENEXT.md item 99): every build of the fork carries its identity
// marker, the runtime's version, which upstream Deno does not have.
Deno.test(function buildInfoDenextMarker() {
  assert(typeof Deno.build.denext === "string");
  assert(Deno.build.denext === Deno.version.deno);
  assert(Object.isFrozen(Deno.build));
});

Deno.test(async function buildInfoDenextMarkerInWorker() {
  const worker = new Worker(
    `data:application/javascript,postMessage(Deno.build.denext)`,
    { type: "module" },
  );
  const marker = await new Promise((resolve, reject) => {
    worker.onmessage = (e) => resolve(e.data);
    worker.onerror = (e) => reject(e);
  });
  worker.terminate();
  assert(marker === Deno.build.denext);
});
