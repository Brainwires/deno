// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// denext runtime smoke app. Packaged by .github/denext-runtime/smoke.sh with
// the stock `deno desktop` CLI against a prebuilt runtime archive, and (where
// the runner can show a window) launched by launch.sh.
//
// The page POSTs what it sees back to the server. The server records the
// request in $SMOKE_RESULT_FILE and exits the process, so a launch succeeds
// only when the whole path works: the runtime starts, the webview loads the
// page from the app origin, and a request with a body reaches Deno.serve.
//
// When smoke.sh built the Node-API probe addon (napi-probe/) into the app, the
// app also loads it through `require` and records what its functions return:
// the runtime library's Node-API symbols must resolve from inside the host
// executable, both linked and looked up at run time.

import { createRequire } from "node:module";

const resultFile = Deno.env.get("SMOKE_RESULT_FILE");

function loadNapiProbe(): Record<string, unknown> | null {
  if (Deno.env.get("SMOKE_NAPI") !== "1") return null;
  try {
    const require = createRequire(import.meta.url);
    const probe = require("./probe.node");
    return { add: probe.add(2, 40), lookup: probe.lookup() };
  } catch (e) {
    return { error: String((e as Error)?.stack ?? e) };
  }
}

const napi = loadNapiProbe();
console.log("[smoke] napi", JSON.stringify(napi));

const page = `<!doctype html>
<html>
  <head><meta charset="utf-8"><title>denext smoke</title></head>
  <body>
    <h1>denext runtime smoke</h1>
    <script>
      fetch("/result", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          href: location.href,
          origin: location.origin,
          secureContext: globalThis.isSecureContext,
          userAgent: navigator.userAgent,
        }),
      }).catch((e) => console.error(e));
    </script>
  </body>
</html>
`;

Deno.serve(async (req) => {
  const url = new URL(req.url);
  if (req.method === "POST" && url.pathname === "/result") {
    const page = await req.json();
    const record = {
      ok: true,
      page,
      requestUrl: req.url,
      originHeader: req.headers.get("origin"),
      appOrigin: Deno.env.get("DENO_DESKTOP_APP_ORIGIN") ?? null,
      napi,
    };
    console.log("[smoke] result", JSON.stringify(record));
    if (resultFile) {
      await Deno.writeTextFile(resultFile, JSON.stringify(record, null, 2));
    }
    setTimeout(() => Deno.exit(0), 250);
    return new Response("ok");
  }
  return new Response(page, {
    headers: { "content-type": "text/html; charset=utf-8" },
  });
});
