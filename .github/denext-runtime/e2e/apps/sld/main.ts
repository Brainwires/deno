// Copyright 2018-2026 the Deno authors. MIT license.
// e2e: global shortcuts, launch at login and DevTools control.
//
// Launched twice: with DevTools on and off (`inspectable` in
// laufey-launch.json; params.devtools says which). The shortcut is pressed
// for real (XTEST / keybd_event / CoreGraphics; see ../_shared/input.ts) and
// must reach both the callback and the "shortcut" event. Launch at login is
// checked against the OS's own record of it (the XDG autostart entry, the
// HKCU Run value, SMAppService's status).

// deno-lint-ignore-file no-explicit-any

import {
  BrowserWindow,
  desktop,
  errorOf,
  html,
  OS,
  page,
  Report,
  sleep,
  waitFor,
} from "../_shared/e2e.ts";
import { activate, canPressKeys, pressKeys } from "../_shared/input.ts";

const r = new Report("sld");
const expectDevtools = r.params.devtools !== false;
const identifier: string = r.params.identifier;

Deno.serve(() => html(page("e2e sld")));
const TITLE = "E2E SLD";
const win = new BrowserWindow({ title: TITLE, width: 600, height: 400 });
await sleep(2500);

await r.step("shortcuts", async () => {
  const sc: any = desktop.shortcuts;
  r.check(
    "the surface exists",
    typeof sc.register === "function" && "onshortcut" in sc &&
      sc instanceof EventTarget &&
      typeof desktop.launchAtLogin?.get === "function" &&
      typeof desktop.devtools?.toggle === "function" &&
      typeof win.closeDevtools === "function" &&
      typeof win.toggleDevtools === "function" &&
      typeof win.isDevtoolsOpen === "function" &&
      typeof win.isDevtoolsEnabled === "function",
  );
  const caps = sc.capabilities();
  r.set("shortcutCapabilities", caps);
  r.check(
    "canonicalize(garbage) is null",
    sc.canonicalize("Ctrl+Nope") === null,
  );
  const bad: any = await errorOf(sc.register("K"));
  r.check(
    "a key without a modifier is a TypeError (invalid)",
    bad instanceof TypeError && (bad as any).code === "invalid",
    bad && String(bad),
  );
  if (!caps.globalShortcuts) {
    const e = await errorOf(sc.register("CommandOrControl+Alt+Shift+F9"));
    r.check(
      "no global shortcuts here: register rejects NotSupported (not_supported)",
      e instanceof (Deno as any).errors.NotSupported &&
        e?.code === "not_supported",
      e && String(e),
    );
    return;
  }
  const ACCEL = "CommandOrControl+Alt+Shift+F9";
  const want = OS === "darwin" ? "Alt+Shift+Super+F9" : "Ctrl+Alt+Shift+F9";
  const pressed: string[] = [];
  const events: string[] = [];
  sc.addEventListener(
    "shortcut",
    (e: CustomEvent) => events.push(e.detail.accelerator),
  );
  const canonical = await sc.register(ACCEL, (a: string) => pressed.push(a));
  r.check(
    "register resolves with the canonical form",
    canonical === want && canonical === sc.canonicalize(ACCEL),
    canonical,
  );
  r.check(
    "list / isRegistered (any spelling)",
    sc.list().includes(canonical) && sc.isRegistered(ACCEL.toLowerCase()),
    sc.list(),
  );
  const dup = await errorOf(sc.register(ACCEL));
  r.check(
    "registering it again is AlreadyExists (already_registered)",
    dup instanceof (Deno as any).errors.AlreadyExists &&
      dup?.code === "already_registered",
    dup && String(dup),
  );
  r.check(
    "unregister releases it",
    sc.unregister(canonical) && !sc.isRegistered(ACCEL) &&
      sc.list().length === 0,
  );
  r.check("a second unregister is false", !sc.unregister(ACCEL));
  await sc.register(ACCEL, (a: string) => pressed.push(a));
  const cannot = await canPressKeys();
  if (cannot) {
    r.na("a real key press reaches the callback and the event", cannot);
  } else {
    await activate(TITLE);
    await pressKeys(["Primary", "Alt", "Shift", "F9"]);
    r.check(
      "a real key press calls the callback and fires the event, once each",
      await waitFor(() => pressed.length === 1 && events.length === 1, 8000) &&
        pressed[0] === canonical && events[0] === canonical,
      { pressed, events },
    );
  }
  sc.unregisterAll();
  r.check("unregisterAll empties the list", sc.list().length === 0);
  if (!cannot) {
    await pressKeys(["Primary", "Alt", "Shift", "F9"]);
    await sleep(1500);
    r.check(
      "after unregisterAll a press does nothing",
      pressed.length <= 1,
      pressed,
    );
  }
});

/** What the OS itself says about the app's login item. */
async function osLoginRecord(): Promise<string> {
  if (OS === "windows") {
    const o = await new Deno.Command("reg", {
      args: [
        "query",
        "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
        "/v",
        identifier,
      ],
      stdout: "piped",
      stderr: "piped",
    }).output();
    return o.success ? "present" : "absent";
  }
  if (OS === "linux") {
    const base = Deno.env.get("XDG_CONFIG_HOME") ??
      `${Deno.env.get("HOME")}/.config`;
    try {
      const t = await Deno.readTextFile(
        `${base}/autostart/${identifier}.desktop`,
      );
      return t.includes(Deno.execPath())
        ? "present"
        : `present, other exec: ${t}`;
    } catch {
      return "absent";
    }
  }
  return "(SMAppService: the API's own status)";
}

await r.step("launch at login", async () => {
  const before = await desktop.launchAtLogin.get();
  r.set("loginBefore", before);
  r.check(
    "get() reports a state",
    ["enabled", "disabled", "requires-approval", "not-supported"].includes(
      before,
    ),
    before,
  );
  r.check(
    "set() takes a boolean",
    (await errorOf(desktop.launchAtLogin.set("yes"))) instanceof TypeError,
  );
  if (before === "not-supported") {
    r.na(
      "launchAtLogin.set round trip",
      "the OS reports launch at login not supported here",
    );
    return;
  }
  if (!expectDevtools) return; // once per backend is enough
  const on = await desktop.launchAtLogin.set(true).catch((e: Error) =>
    `rejected: ${e}`
  );
  r.set("loginOn", on);
  r.check(
    "set(true) enables it (or awaits the user's approval)",
    on === "enabled" || on === "requires-approval",
    on,
  );
  r.check("get() agrees", (await desktop.launchAtLogin.get()) === on);
  if (OS !== "darwin") {
    r.check(
      "the OS records the login item",
      (await osLoginRecord()) === "present",
      await osLoginRecord(),
    );
  }
  const off = await desktop.launchAtLogin.set(false).catch((e: Error) =>
    `rejected: ${e}`
  );
  r.check("set(false) disables it", off === "disabled", off);
  r.check(
    "get() agrees after off",
    (await desktop.launchAtLogin.get()) === "disabled",
  );
  if (OS !== "darwin") {
    r.check(
      "the OS record is gone",
      (await osLoginRecord()) === "absent",
      await osLoginRecord(),
    );
  }
});

await r.step("devtools", async () => {
  r.check(
    "devtools.enabled matches the launch setting",
    desktop.devtools.enabled === expectDevtools,
    desktop.devtools.enabled,
  );
  r.check(
    "the engine's own setting matches",
    await waitFor(() => win.isDevtoolsEnabled() === expectDevtools, 10000),
    win.isDevtoolsEnabled(),
  );
  r.check("closed at start", !win.isDevtoolsOpen());
  if (expectDevtools) {
    win.openDevtools();
    r.check(
      "openDevtools opens them",
      await waitFor(() => win.isDevtoolsOpen(), 15000),
    );
    win.closeDevtools();
    r.check(
      "closeDevtools closes them",
      await waitFor(() => !win.isDevtoolsOpen(), 15000),
    );
    desktop.devtools.toggle(win);
    r.check(
      "devtools.toggle opens them",
      await waitFor(() => desktop.devtools.isOpen(win), 15000),
    );
    win.toggleDevtools();
    r.check(
      "toggleDevtools closes them",
      await waitFor(() => !win.isDevtoolsOpen(), 15000),
    );
  } else {
    win.openDevtools();
    win.toggleDevtools();
    desktop.devtools.open(win);
    await sleep(3000);
    r.check("with DevTools off nothing opens them", !win.isDevtoolsOpen());
  }
  r.check(
    "devtools.open(non-window) is a TypeError",
    (await errorOf(() => desktop.devtools.open({}))) instanceof TypeError,
  );
});

r.finish();
