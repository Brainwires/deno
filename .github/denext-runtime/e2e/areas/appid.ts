// Copyright 2018-2026 the Deno authors. MIT license.
// The app identifier: web storage lives in a per-app data directory that
// persists across launches and is not shared with another app at the same
// origin.

import {
  type AreaReport,
  clearResults,
  type Env,
  launchAndCollect,
  packageApp,
  writeParams,
} from "../lib/runner.ts";

export async function run(env: Env, rep: AreaReport) {
  const scheme = `e2eappid${env.nonce}`;
  const origin = `${scheme}://app`;
  const ids = {
    a: `dev.denext.e2e${env.nonce}.appa`,
    b: `dev.denext.e2e${env.nonce}.appb`,
  };
  const apps = {
    a: await packageApp(env, {
      app: "appid",
      tag: "a",
      name: "E2EAppIdA",
      identifier: ids.a,
      appJson: { origin },
      launch: { appId: ids.a, customSchemes: [scheme] },
    }),
    b: await packageApp(env, {
      app: "appid",
      tag: "b",
      name: "E2EAppIdB",
      identifier: ids.b,
      appJson: { origin },
      launch: { appId: ids.b, customSchemes: [scheme] },
    }),
  };
  await clearResults("appid");
  const go = async (which: "a" | "b", label: string) => {
    await writeParams("appid", { identifier: ids[which] });
    return await launchAndCollect(env, rep, label, apps[which]);
  };
  const a1 = await go("a", "A, first launch");
  const a2 = await go("a", "A, second launch");
  const b1 = await go("b", "B, first launch");
  const pa1 = a1?.data.page, pa2 = a2?.data.page, pb1 = b1?.data.page;
  rep.check(
    "A's first launch starts with empty storage",
    pa1?.localPrevious === null && pa1?.idbPrevious === null,
    pa1,
  );
  rep.check(
    "A's localStorage persists across launches",
    !!pa1 && pa2?.localPrevious === pa1.value,
    { first: pa1?.value, second: pa2?.localPrevious },
  );
  rep.check(
    "A's IndexedDB persists across launches",
    !!pa1 && pa2?.idbPrevious === pa1.value,
    { first: pa1?.value, second: pa2?.idbPrevious },
  );
  rep.check(
    "B (same origin, other identifier) does not see A's storage",
    !!pb1 && pb1.localPrevious === null && pb1.idbPrevious === null,
    pb1,
  );
  rep.check(
    "both apps ran at the same origin",
    pa1?.origin === origin && pb1?.origin === origin,
    [pa1?.origin, pb1?.origin],
  );
}
