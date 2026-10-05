// Copyright 2018-2026 the Deno authors. MIT license.
// Requests that carry cookies complete whatever the Secret Service can do
// (laufey API 45; apps/keyring/main.ts). On Linux the session has either no
// secret service (e2e.sh masks it: activatable, never startable) or, with
// E2E_SECRET_SERVICE=locked, a gnome-keyring whose login keyring is locked
// with no one to unlock it; either way CEF must not wait for the cookie key
// and uses --password-store=basic.

import {
  type AreaReport,
  clearResults,
  type Env,
  launchAndCollect,
  packageApp,
  writeParams,
} from "../lib/runner.ts";

export async function run(env: Env, rep: AreaReport) {
  const linux = env.target.endsWith("-linux-gnu");
  const params: Record<string, unknown> = {
    // CEF keeps the cookie key with the OS (Keychain, DPAPI, the Secret
    // Service) unless no one could unlock it; WebKit reports none.
    cookieEncryption: env.backend === "cef" ? (linux ? "basic" : "os") : null,
  };
  if (linux) {
    params.secretService = Deno.env.get("E2E_SECRET_SERVICE") === "locked"
      ? "locked"
      : "activatable";
  }
  const p = await packageApp(env, {
    app: "keyring",
    name: "E2EKeyring",
    identifier: `dev.denext.e2e${env.nonce}.keyring`,
  });
  await clearResults("keyring");
  await writeParams("keyring", params);
  await launchAndCollect(env, rep, "cookies under this secret service", p, {
    ms: 120_000,
  });
}
