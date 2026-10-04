// Copyright 2018-2026 the Deno authors. MIT license.
// e2e: a packaged app launched with the development switches set
// (DENO_DESKTOP_HMR, DENO_DESKTOP_DEV_URL, DENO_DESKTOP_INSPECT_INTERNAL_PORT)
// must ignore them: no hot reload of its modules from the given directory,
// no inspector on the given address, its window at its own origin. Only a
// `deno desktop --hmr` / `--inspect` development build honours them. Results
// go to the "origin" area.

import { answer } from "./answer.ts";
import { describeError, page, Report, sleep } from "../_shared/e2e.ts";

const r = new Report("origin");
r.set("app", "devswitch");
const ORIGIN = r.params.origin ?? "denexte2e://app";
const hmrDir = Deno.env.get("DENO_DESKTOP_HMR") ?? null;
const inspectAddr = Deno.env.get("DENO_DESKTOP_INSPECT_INTERNAL_PORT") ?? null;

const SCRIPT = `
await fetch("/result", { method: "POST", body: location.origin });
`;

let pageOrigin: string | null = null;
Deno.serve((req) => {
  const url = new URL(req.url);
  if (url.pathname === "/result") {
    return req.text().then((t) => {
      pageOrigin = t;
      return new Response("ok");
    });
  }
  return new Response(page("e2e devswitch", "", SCRIPT), {
    headers: { "content-type": "text/html; charset=utf-8" },
  });
});

async function run() {
  r.check(
    "devswitch: the switches reached the app's environment",
    hmrDir !== null && inspectAddr !== null,
    { hmrDir, inspectAddr },
  );
  // What a hot reload would load: the same module path under the watched
  // directory, both as the source tree lays it out and at its root.
  const evil = `export function answer(): string {\n  return "reloaded";\n}\n`;
  if (hmrDir) {
    for (const rel of ["devswitch/answer.ts", "answer.ts"]) {
      const file = `${hmrDir}/${rel}`;
      await Deno.mkdir(file.slice(0, file.lastIndexOf("/")), {
        recursive: true,
      });
      await Deno.writeTextFile(file, evil);
    }
    await sleep(1500);
    // Touch them again: a watcher may skip the first write.
    for (const rel of ["devswitch/answer.ts", "answer.ts"]) {
      await Deno.writeTextFile(`${hmrDir}/${rel}`, evil + "// again\n");
    }
  }
  await sleep(3000);
  r.check(
    "devswitch: DENO_DESKTOP_HMR does not hot-reload the packaged app's code",
    answer() === "packaged",
    answer(),
  );
  let inspector = "closed";
  if (inspectAddr) {
    const [host, port] = [
      inspectAddr.slice(0, inspectAddr.lastIndexOf(":")),
      Number(inspectAddr.slice(inspectAddr.lastIndexOf(":") + 1)),
    ];
    try {
      const c = await Deno.connect({ hostname: host, port });
      c.close();
      inspector = "open";
    } catch {
      inspector = "closed";
    }
  }
  r.check(
    "devswitch: DENO_DESKTOP_INSPECT_INTERNAL_PORT opens no inspector",
    inspector === "closed",
    inspector,
  );
  const end = Date.now() + 20_000;
  while (pageOrigin === null && Date.now() < end) await sleep(100);
  r.check(
    "devswitch: DENO_DESKTOP_DEV_URL does not move the window off the app origin",
    pageOrigin === ORIGIN,
    pageOrigin,
  );
  r.finish();
}

run().catch((e) => {
  r.fail("devswitch threw", describeError(e));
  r.finish();
});
