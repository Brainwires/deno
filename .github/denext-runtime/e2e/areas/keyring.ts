// Copyright 2018-2026 the Deno authors. MIT license.
// Requests that carry cookies complete whatever the Secret Service can do
// (laufey API 45; apps/keyring/main.ts). On Linux the session has either no
// secret service (e2e.sh masks it: activatable, never startable) or, with
// E2E_SECRET_SERVICE=locked, a gnome-keyring whose login keyring is locked
// with no one to unlock it (gcr's prompter installed); either way CEF must
// not wait for the cookie key and uses --password-store=basic. Xvfb's
// $DISPLAY never makes the session graphical: with XDG_SESSION_TYPE unset
// (e2e.sh's default) or set but not confirmed by logind (E2E_SESSION_TYPE),
// no one can answer the prompt. With E2E_KWALLET=activatable the session is
// GNOME with kwalletd installed but not running: not Chromium's store there,
// so still basic.

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
    params.secretServicePrompt = false;
    const sessionType = Deno.env.get("E2E_SESSION_TYPE") ?? "unset";
    params.sessionType = sessionType === "unset" ? "unknown" : sessionType;
    // No KDE desktop here (none, or GNOME with kwalletd installed): not
    // Chromium's store, so no KWallet state.
    params.kwallet = null;
    // The secure store (laufey API 47) refuses at once: the keyring is
    // locked with no one to unlock it, or the masked service can't start.
    params.secureStoreReason = params.secretService === "locked"
      ? "no one here can answer its unlock prompt"
      : "did not answer";
    // A fresh profile holds no OS-key cookies: never waits for the key.
    if (env.backend === "cef") params.cookieEncryptionWait = null;
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
