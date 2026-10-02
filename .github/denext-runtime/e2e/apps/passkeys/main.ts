// Copyright 2018-2026 the Deno authors. MIT license.
// e2e: Deno.desktop.passkeys error paths. Every check that needs no person
// at the machine: argument refusals, the envelope of every refusal laufey
// and the OS produce, one ceremony at a time, an explicit window, and the
// worker exclusion. A successful ceremony needs a person (Touch ID / Windows
// Hello) and, on macOS, an associated-domains entitlement, so it is n/a.

// deno-lint-ignore-file no-explicit-any

import {
  BrowserWindow,
  describeError,
  desktop,
  html,
  OS,
  page,
  Report,
  sleep,
} from "../_shared/e2e.ts";

const r = new Report("passkeys");
const CODES = [
  "cancelled",
  "invalid_rp",
  "not_supported",
  "timeout",
  "unknown",
];
const envelope = (s: unknown) => {
  try {
    const v = JSON.parse(String(s));
    return v.ok === true
      ? { ok: true }
      : { ok: false, code: v.error?.code, message: v.error?.message };
  } catch {
    return { ok: false, code: `not JSON: ${s}` };
  }
};
const rejects = async (f: () => Promise<unknown>) => {
  try {
    await f();
    return null;
  } catch (e) {
    return e;
  }
};
const getOpts = (rpId: string, timeout = 2000) =>
  JSON.stringify({
    challenge: "AAECAwQFBgcICQoLDA0ODw",
    rpId,
    timeout,
    userVerification: "preferred",
  });

Deno.serve(() => html(page("e2e passkeys")));
await sleep(2500);

await r.step("passkeys", async () => {
  const p = desktop.passkeys;
  r.check(
    "Deno.desktop.passkeys is a frozen object with capabilities / create / get",
    typeof p === "object" && Object.isFrozen(p) &&
      ["capabilities", "create", "get"].every((k) =>
        typeof p[k] === "function"
      ),
  );
  const caps = await p.capabilities();
  r.set("capabilities", caps);
  r.check(
    "capabilities() reports two booleans",
    typeof caps.platformAuthenticator === "boolean" &&
      typeof caps.securityKeys === "boolean",
    caps,
  );
  if (OS === "linux") {
    r.check(
      "Linux: no authenticators",
      !caps.platformAuthenticator && !caps.securityKeys,
      caps,
    );
  }

  r.check(
    "a non-string options argument is a TypeError",
    (await rejects(() => p.create(42))) instanceof TypeError,
  );
  r.check(
    "a string window is a TypeError",
    (await rejects(() => p.get("{}", { window: "x" }))) instanceof TypeError,
  );
  r.check(
    "a negative window id is a TypeError",
    (await rejects(() => p.get("{}", { window: -1 }))) instanceof TypeError,
  );
  r.check(
    "a window id over u32 is a TypeError",
    (await rejects(() => p.get("{}", { window: 2 ** 32 }))) instanceof
      TypeError,
  );

  const expectNotSupported = OS === "linux";
  const refusal = async (name: string, s: Promise<string>, want: string) => {
    const e = envelope(await s);
    r.check(
      `${name} -> ${expectNotSupported ? "not_supported" : want}`,
      e.ok === false &&
        e.code === (expectNotSupported ? "not_supported" : want),
      e,
    );
  };
  await refusal(
    "a malformed challenge",
    p.create(
      JSON.stringify({
        rp: { id: "example.com", name: "x" },
        user: { id: "dXNlcg", name: "u" },
        challenge: "not base64!",
      }),
    ),
    "unknown",
  );
  await refusal("an empty rpId", p.get(getOpts("")), "invalid_rp");
  await refusal("options that are not JSON", p.get("{"), "unknown");
  const unknownWindow = envelope(
    await p.get(getOpts("example.com"), { window: 999999 }),
  );
  r.check(
    expectNotSupported
      ? "an unknown window -> not_supported"
      : "an unknown window -> unknown, not found",
    unknownWindow.ok === false &&
      (expectNotSupported
        ? unknownWindow.code === "not_supported"
        : unknownWindow.code === "unknown" &&
          /not found/.test(unknownWindow.message ?? "")),
    unknownWindow,
  );

  if (expectNotSupported) {
    r.na(
      "an OS ceremony and the one-at-a-time rule",
      "Linux has no platform passkey API: every request is not_supported (checked above)",
    );
  } else {
    // A real OS request (it fails without a person / entitlement) and one made
    // while it runs, which must be refused as busy.
    const win = new BrowserWindow({
      title: "E2E Passkeys",
      width: 400,
      height: 300,
    });
    await sleep(1500);
    const first = p.get(getOpts("example.com"), { window: win });
    await sleep(100);
    const second = envelope(
      await p.get(getOpts("example.com"), { window: win }),
    );
    r.check(
      "a request while one runs -> unknown, already in progress",
      second.ok === false && second.code === "unknown" &&
        /already in progress/.test(second.message ?? ""),
      second,
    );
    const t0 = Date.now();
    const firstEnv = envelope(
      await Promise.race([
        first,
        sleep(120000).then(() =>
          '{"ok":false,"error":{"code":"(no answer in 120 s)"}}'
        ),
      ]),
    );
    r.set("osRequest", { ...firstEnv, ms: Date.now() - t0 });
    r.check(
      "the OS request ends with a refusal envelope (no person, no entitlement)",
      firstEnv.ok === false && CODES.includes(firstEnv.code),
      firstEnv,
    );
    if (OS === "darwin") {
      r.check(
        "macOS: an unsigned app without associated domains -> invalid_rp",
        firstEnv.code === "invalid_rp",
        firstEnv,
      );
    }
    // Once it ended, the next request is not busy.
    const third = envelope(
      await p.get(getOpts("example.com"), { window: 999999 }),
    );
    r.check(
      "the slot frees once a ceremony ends",
      third.code === "unknown" && /not found/.test(third.message ?? ""),
      third,
    );
  }
  r.na(
    "a completed registration / authentication ceremony",
    "it needs a person at the machine (Touch ID / Windows Hello) and, on macOS, an associated-domains entitlement in a Developer ID signature",
  );

  // Workers can't reach the ops.
  const w = new Worker(new URL("./worker.ts", import.meta.url), {
    type: "module",
  });
  const fromWorker = await new Promise((resolve) => {
    w.onmessage = (e) => resolve(e.data);
    w.onerror = (e) => {
      e.preventDefault();
      resolve(`worker error: ${e.message}`);
    };
    setTimeout(() => resolve("worker timeout"), 15000);
  });
  w.terminate();
  r.check(
    "workers have no Deno.desktop.passkeys",
    (fromWorker as any)?.passkeys === "undefined",
    fromWorker,
  );
}).catch((e) => r.fail("passkeys threw", describeError(e)));

r.finish();
