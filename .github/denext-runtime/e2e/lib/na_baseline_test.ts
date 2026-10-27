// Copyright 2018-2026 the Deno authors. MIT license.
// The e2e n/a baseline comparison (na_baseline.ts) and the committed
// baseline's shape:
//
//   deno test -A --no-config --no-lock .github/denext-runtime/e2e/lib/

import { baselineKey, compareNa, type NaBaseline } from "./na_baseline.ts";

function assertEquals(actual: unknown, expected: unknown, what: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: got ${a}, want ${e}`);
}

const baseline: NaBaseline = {
  "$comment": "x",
  "x86_64-unknown-linux-gnu/cef/default": {
    menus: ["launch: a click on the body fires click"],
    window: ["launch: share() shows the OS share sheet"],
  },
};
const key = baselineKey("x86_64-unknown-linux-gnu", "cef", "default");

Deno.test("an expected n/a passes", () => {
  const r = compareNa(baseline, key, ["menus", "window"], [
    { area: "menus", name: "launch: a click on the body fires click" },
    { area: "window", name: "launch: share() shows the OS share sheet" },
  ]);
  assertEquals(r.unexpected, [], "unexpected");
  assertEquals(r.noLongerNa, [], "noLongerNa");
});

Deno.test("a new n/a is unexpected", () => {
  const r = compareNa(baseline, key, ["menus", "window"], [
    { area: "menus", name: "launch: a click on the body fires click" },
    { area: "menus", name: "launch: a notification shows", reason: "r" },
    { area: "window", name: "launch: share() shows the OS share sheet" },
  ]);
  assertEquals(
    r.unexpected,
    [{ area: "menus", name: "launch: a notification shows", reason: "r" }],
    "unexpected",
  );
});

Deno.test("the same name in another area is unexpected", () => {
  const r = compareNa(baseline, key, ["origin"], [
    { area: "origin", name: "launch: share() shows the OS share sheet" },
  ]);
  assertEquals(r.unexpected.length, 1, "unexpected");
});

Deno.test("a key the baseline lacks expects no n/a", () => {
  const other = baselineKey(
    "x86_64-unknown-linux-gnu",
    "cef",
    "keyring-locked",
  );
  const r = compareNa(baseline, other, ["menus"], [
    { area: "menus", name: "launch: a click on the body fires click" },
  ]);
  assertEquals(r.unexpected.length, 1, "unexpected");
});

Deno.test("a check that now runs is reported, only for areas that ran", () => {
  const r = compareNa(baseline, key, ["menus"], []);
  assertEquals(
    r.noLongerNa,
    [{ area: "menus", name: "launch: a click on the body fires click" }],
    "noLongerNa",
  );
});

Deno.test("the committed expected-na.json is well formed", async () => {
  const file = new URL("../expected-na.json", import.meta.url);
  const data = JSON.parse(await Deno.readTextFile(file)) as NaBaseline;
  const areas = new Set([
    "origin",
    "appid",
    "deeplink",
    "window",
    "dnd",
    "passkeys",
    "sld",
    "menus",
    "asmt",
    "update",
    "keyring",
    "keyringpersist",
    "exit",
  ]);
  for (const [k, v] of Object.entries(data)) {
    if (k === "$comment") continue;
    if (
      !/^[a-z0-9_]+-[a-z0-9_]+-[a-z0-9_]+(-[a-z0-9_]+)?\/(webview|cef)\/[a-z-]+$/
        .test(k)
    ) {
      throw new Error(`bad key ${k}`);
    }
    if (typeof v !== "object") throw new Error(`${k}: not an object`);
    for (const [area, names] of Object.entries(v)) {
      if (!areas.has(area)) throw new Error(`${k}: unknown area ${area}`);
      if (!Array.isArray(names) || names.some((n) => typeof n !== "string")) {
        throw new Error(`${k}/${area}: not a list of check names`);
      }
    }
  }
});
