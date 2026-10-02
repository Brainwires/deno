// Copyright 2018-2026 the Deno authors. MIT license.
// e2e: menu accelerators and context-menu close, live / scheduled /
// actionable notifications and their responses.
//
// Clicks on notifications are made the way the OS makes them where a runner
// can: on Linux by a stand-in notification server (linux/
// notification-server.py: `[[invoke:<key>]]` in a body is the user clicking
// that action), on Windows through the app's own toast activator
// (windows/toast-click.ps1: CoCreateInstance + Activate, what the shell
// does). macOS needs a person to click a banner, so those are n/a there.
//
// params.mode "cold" (Windows): the launch COM started for a click while the
// app wasn't running; it only reports launchNotificationResponses.

// deno-lint-ignore-file no-explicit-any

import {
  describeError,
  desktop,
  errorOf,
  html,
  once,
  OS,
  page,
  Report,
  shared,
  sleep,
  titledWindow,
  waitFor,
  within,
} from "../_shared/e2e.ts";
import {
  activate,
  canPressKeys,
  type Key,
  pressKeys,
} from "../_shared/input.ts";

const r = new Report("menus");
const identifier: string = r.params.identifier;
const responses: any[] = [];

if (r.params.mode === "cold") {
  // COM hands the click over once the app registered its activator, which
  // may be after this code first reads the inbox: then it is an event (still
  // marked as the launch).
  const inbox = desktop.launchNotificationResponses;
  const got: any[] = [...inbox];
  r.set("launchNotificationResponses", [...inbox]);
  desktop.addEventListener(
    "notificationresponse",
    (e: CustomEvent) => got.push(e.detail),
  );
  r.check(
    "launchNotificationResponses is a snapshot taken on first read (later clicks are events)",
    desktop.launchNotificationResponses === inbox && Object.isFrozen(inbox),
  );
  await waitFor(() => got.length > 0, 30000);
  await sleep(1000);
  r.set("responses", got);
  r.set("args", Deno.args);
  r.check(
    "the click that launched the app arrives once, as the launch, with its tag, action and data",
    got.length === 1 && got[0].tag === "e2e-cold" &&
      got[0].action === "cold-action" &&
      JSON.stringify(got[0].data) === '{"c":1}' && got[0].launch === true,
    got,
  );
  r.finish();
}

desktop.addEventListener("notificationresponse", (e: CustomEvent) => {
  responses.push(e.detail);
  r.set("responses", responses);
});

Deno.serve((req) => shared(req) ?? html(page("e2e menus")));
const TITLE = "E2E Menus";
const win = titledWindow(TITLE);
await sleep(3000);
const cannotPress = await canPressKeys();

/** Click a notification as the OS would (null) or say why it can't. */
async function osClick(
  tag: string,
  title: string,
  action: string | null,
  data?: string,
): Promise<string | null> {
  void title;
  if (OS === "windows") {
    const args = [
      "-NoProfile",
      "-ExecutionPolicy",
      "Bypass",
      "-File",
      r.params.toastClick,
      "-Aumid",
      identifier,
      "-Tag",
      tag,
    ];
    if (action) args.push("-Action", action);
    if (data) args.push("-Data", data);
    const o = await new Deno.Command("powershell", {
      args,
      stdout: "piped",
      stderr: "piped",
    }).output();
    return o.success
      ? null
      : new TextDecoder().decode(o.stderr) + new TextDecoder().decode(o.stdout);
  }
  if (OS === "linux") return null; // the body asked the stand-in server to click
  return "macOS delivers a click only from a person clicking the banner (no API or automation posts one for an unsigned app)";
}
// On Linux the click is requested in the body; elsewhere it is a no-op text.
const clickBody = (key: string) => OS === "linux" ? `[[invoke:${key}]]` : "e2e";

await r.step("menus", async () => {
  const caps = desktop.menuCapabilities();
  r.set("menuCapabilities", caps);
  r.check(
    "menuCapabilities() reports six booleans",
    [
      "appMenu",
      "accelerators",
      "contextMenu",
      "contextClosed",
      "icons",
      "tooltips",
    ].every((k) => typeof caps[k] === "boolean"),
    caps,
  );
  r.check(
    "application menus and their accelerators are supported",
    caps.appMenu === true && caps.accelerators === true,
    caps,
  );

  const clicks: string[] = [];
  win.addEventListener(
    "menuclick",
    (e: CustomEvent) => clicks.push(e.detail.id),
  );
  r.mark("application menu");
  win.setApplicationMenu([
    {
      submenu: {
        label: "E2E",
        items: [
          {
            item: {
              label: "Fire",
              id: "fire",
              accelerator: "CommandOrControl+Shift+K",
              enabled: true,
            },
          },
          {
            item: {
              label: "Bad accel",
              id: "bad",
              accelerator: "Ctrl+Nope",
              enabled: true,
            },
          },
        ],
      },
    },
  ]);
  if (cannotPress) {
    r.na("an application-menu accelerator fires its item", cannotPress);
  } else {
    await sleep(800);
    win.focus();
    await activate(TITLE);
    await sleep(500);
    r.mark("accelerator press");
    await pressKeys(["Primary", "Shift", "K"]);
    r.check(
      "an application-menu accelerator fires its item (menuclick)",
      await waitFor(() => clicks.includes("fire"), 8000),
      clicks,
    );
  }

  const menu = [
    { item: { label: "Alpha", id: "ctx-a", enabled: true } },
    { item: { label: "Beta", id: "ctx-b", enabled: true } },
  ];
  const closes: (string | null)[] = [];
  const ctxClicks: string[] = [];
  win.addEventListener(
    "contextmenuclose",
    (e: CustomEvent) => closes.push(e.detail.id),
  );
  win.addEventListener(
    "contextmenuclick",
    (e: CustomEvent) => ctxClicks.push(e.detail.id),
  );
  if (!caps.contextClosed) {
    const t0 = Date.now();
    const got = await Promise.race([
      win.showContextMenu(40, 40, menu),
      sleep(5000).then(() => "(pending)"),
    ]);
    r.check(
      "no close reporting here: showContextMenu resolves null at once",
      got === null && Date.now() - t0 < 4000,
      got,
    );
    await pressKeys(["Escape"]).catch(() => {});
    return;
  }
  if (cannotPress) {
    r.na("a context menu dismissed / chosen from the keyboard", cannotPress);
    return;
  }
  const choose = async (keys: Key[][], label: string) => {
    r.mark(`${label}: focus`);
    win.focus();
    r.mark(`${label}: activate`);
    await activate(TITLE);
    r.mark(`${label}: showContextMenu`);
    const p = win.showContextMenu(40, 40, menu);
    r.mark(`${label}: shown`);
    await sleep(2000);
    for (const k of keys) {
      r.mark(`${label}: press ${k.join("+")}`);
      await pressKeys(k);
      await sleep(600);
    }
    r.mark(`${label}: waiting`);
    return await Promise.race([
      p,
      sleep(8000).then(() => `(${label}: still open after 8 s)`),
    ]);
  };
  r.mark("context menu: Escape");
  const dismissed = await choose([["Escape"]], "Escape");
  r.check(
    "Escape dismisses the context menu: it resolves null",
    dismissed === null,
    dismissed,
  );
  r.check(
    "…and contextmenuclose fires with id null",
    await waitFor(() => closes.length === 1 && closes[0] === null, 3000),
    closes,
  );
  r.mark("context menu: Down+Return");
  const chosen = await choose([["Down"], ["Return"]], "Down+Return");
  r.check("choosing an item resolves with its id", chosen === "ctx-a", chosen);
  r.check(
    "…after a contextmenuclick and a contextmenuclose with that id",
    await waitFor(
      () =>
        ctxClicks.includes("ctx-a") && closes.length === 2 &&
        closes[1] === "ctx-a",
      3000,
    ),
    { ctxClicks, closes },
  );
});

await r.step("notifications", async () => {
  const N = (globalThis as any).Notification;
  const caps = desktop.notifications.capabilities();
  r.set("notificationCapabilities", caps);
  r.check(
    "notifications.capabilities() reports six booleans",
    ["show", "schedule", "schedulePersists", "actions", "clicks", "coldStart"]
      .every((k) => typeof caps[k] === "boolean"),
    caps,
  );
  r.check(
    "Notification.maxActions is 5 with action buttons, else 0",
    N.maxActions === (caps.actions ? 5 : 0),
    N.maxActions,
  );
  const perm = await desktop.notifications.requestPermission(
    OS === "darwin" ? { provisional: true } : undefined,
  )
    .catch((e: Error) => `rejected: ${describeError(e)}`);
  r.set("permission", perm);
  r.check(
    "requestPermission() answers with a status",
    ["granted", "denied", "prompt", "unsupported"].includes(perm),
    perm,
  );
  if (OS === "linux") {
    r.check(
      "Linux with a notification server: granted, show / schedule / actions / clicks on, no cold start",
      perm === "granted" && caps.show && caps.schedule && caps.actions &&
        caps.clicks && !caps.coldStart && !caps.schedulePersists,
      caps,
    );
  }
  if (OS === "windows") {
    r.check(
      "Windows: show / schedule (persisting) / actions / clicks / cold start",
      caps.show && caps.schedule && caps.schedulePersists && caps.actions &&
        caps.clicks && caps.coldStart,
      caps,
    );
  }

  // Argument checks.
  r.check(
    "an action without a title is a TypeError",
    (() => {
      try {
        new N("x", { actions: [{ action: "a" }] });
        return false;
      } catch (e) {
        return e instanceof TypeError;
      }
    })(),
  );
  r.check(
    "schedule() without `at` is a TypeError",
    (await errorOf(desktop.notifications.schedule({ title: "x" }))) instanceof
      TypeError,
  );
  r.check(
    "schedule() with more than 4 KiB of data is a TypeError",
    (await errorOf(
      desktop.notifications.schedule({
        title: "x",
        at: Date.now() + 60000,
        tag: "big",
        data: "x".repeat(5000),
      }),
    )) instanceof TypeError,
  );

  if (!caps.show) {
    r.na(
      "a live notification: show / click / action / close",
      `the backend reports show: false here (permission ${perm})`,
    );
  } else {
    // A live notification with an action button: the action event.
    r.mark("live notification");
    const a = new N("E2E action", {
      body: clickBody("yes"),
      tag: "e2e-action",
      data: { n: 1 },
      actions: [{ action: "yes", title: "Yes" }],
    });
    const clicked: string[] = [];
    a.addEventListener("click", () => clicked.push("click"));
    let aClosedAt = 0;
    a.addEventListener("close", () => (aClosedAt ||= Date.now()));
    const actionP = once(a, "action", 15000);
    const shown = await once(a, "show", 10000);
    r.check("a live notification fires show", shown !== null);
    r.check(
      "data stays on the object",
      JSON.stringify(a.data) === '{"n":1}',
      a.data,
    );
    const why = await osClick("e2e-action", "E2E action", "yes", '{"n":1}');
    if (why) {
      r.na("a click on an action button fires action (not click)", why);
    } else {
      const ev: any = await within(actionP, 10000);
      const event = "value" in ev ? ev.value : null;
      const resp = () => responses.find((x) => x.tag === "e2e-action");
      if (!event && aClosedAt && await waitFor(() => !!resp(), 5000)) {
        // The shell retired the toast before the click reached it (a hosted
        // runner's session hides toasts within ~2 s and reports them
        // closed): the click is a response, as for any notification no live
        // object owns.
        r.check(
          "a click on a toast the shell already retired is a notificationresponse with its action and data",
          JSON.stringify(resp()) ===
            JSON.stringify({
              tag: "e2e-action",
              action: "yes",
              data: { n: 1 },
              launch: false,
            }),
          resp(),
        );
        r.na(
          "a click on a live toast's action button fires action (not click)",
          "the hosted runner's shell retires every toast before a click can reach it (it reports the toast closed within ~2 s)",
        );
      } else {
        r.check(
          "a click on an action button fires action with its id, not click",
          event?.action === "yes" && clicked.length === 0,
          event
            ? { action: event.action, clicked }
            : { event: "none", closedAt: aClosedAt, responses },
        );
      }
    }
    // The body.
    const b = new N("E2E click", {
      body: clickBody("default"),
      tag: "e2e-click",
    });
    const clickP = once(b, "click", 15000);
    let bClosedAt = 0;
    b.addEventListener("close", () => (bClosedAt ||= Date.now()));
    await once(b, "show", 10000);
    const whyB = await osClick("e2e-click", "E2E click", null);
    if (whyB) r.na("a click on the body fires click", whyB);
    else {
      const got = await within(clickP, 10000);
      if (
        !("value" in got && got.value) && bClosedAt &&
        await waitFor(() => responses.some((x) => x.tag === "e2e-click"), 5000)
      ) {
        r.check(
          "a body click on a toast the shell already retired is a notificationresponse",
          true,
        );
        r.na(
          "a click on a live toast's body fires click",
          "the hosted runner's shell retires every toast before a click can reach it",
        );
      } else {
        r.check(
          "a click on the body fires click",
          "value" in got && got.value !== null,
          { closedAt: bClosedAt, responses },
        );
      }
    }
    // Dismissed.
    if (OS === "linux") {
      const c = new N("E2E dismiss", {
        body: "[[dismiss]]",
        tag: "e2e-dismiss",
      });
      const closeP = once(c, "close", 10000);
      r.check(
        "a notification the user dismisses fires close",
        (await closeP) !== null,
      );
    } else {
      r.na(
        "a notification the user dismisses fires close",
        OS === "windows"
          ? "dismissing a toast needs the shell's UI (no activation API covers it)"
          : "it needs a person at the machine",
      );
    }
    r.mark("closing live notifications");
    a.close();
    b.close();
  }

  r.mark("scheduling");
  if (!caps.schedule) {
    const e = await errorOf(
      desktop.notifications.schedule({ title: "x", at: Date.now() + 60000 }),
    );
    r.check(
      "no scheduling here: schedule() rejects NotSupported",
      e instanceof (Deno as any).errors.NotSupported,
      e && String(e),
    );
  } else {
    const at = Date.now() + 10 * 60_000;
    const tag = await desktop.notifications.schedule({
      title: "E2E later",
      body: "later",
      at,
      tag: "e2e-later",
      data: { k: "v" },
      actions: [{ action: "snooze", title: "Snooze" }],
    });
    r.check("schedule() resolves with the tag", tag === "e2e-later", tag);
    const auto = await desktop.notifications.schedule({
      title: "E2E auto tag",
      at: Date.now() + 10 * 60_000,
    });
    r.check(
      "schedule() without a tag makes up a UUID",
      /^[0-9a-f-]{36}$/.test(auto),
      auto,
    );
    r.mark("getScheduled");
    // macOS adds a request to UNUserNotificationCenter asynchronously:
    // the pending list catches up within moments.
    let list: any[] = [];
    await waitFor(async () => {
      list = await desktop.notifications.getScheduled();
      return list.some((n: any) => n.tag === "e2e-later") &&
        list.some((n: any) => n.tag === auto);
    }, 5000);
    const mine = list.find((n: any) => n.tag === "e2e-later");
    r.set("scheduled", list);
    r.check(
      "getScheduled() lists it with its title, body, time, data and actions",
      mine && mine.title === "E2E later" && mine.body === "later" &&
        mine.at instanceof Date &&
        Math.abs(mine.at.getTime() - at) < 2000 &&
        JSON.stringify(mine.data) === '{"k":"v"}' &&
        mine.actions?.[0]?.action === "snooze",
      mine,
    );
    r.mark("cancel");
    desktop.notifications.cancel("e2e-later");
    desktop.notifications.cancel(auto);
    await sleep(500);
    const after = await desktop.notifications.getScheduled();
    r.check(
      "cancel(tag) removes it",
      !after.some((n: any) => n.tag === "e2e-later" || n.tag === auto),
      after,
    );

    r.mark("scheduling one due in 3 s");
    // One due in 3 s, delivered by the scheduler, then clicked: no live
    // Notification owns it, so it is a notificationresponse.
    await desktop.notifications.schedule({
      title: "E2E due",
      body: clickBody("default"),
      at: Date.now() + 3000,
      tag: "e2e-due",
      data: { due: true },
    });
    if (OS === "linux") {
      r.check(
        "a scheduled notification is delivered at its time and its click is a notificationresponse",
        await waitFor(
          () => responses.some((x) => x.tag === "e2e-due"),
          20000,
        ) &&
          JSON.stringify(responses.find((x) => x.tag === "e2e-due")) ===
            JSON.stringify({
              tag: "e2e-due",
              action: null,
              data: { due: true },
              launch: false,
            }),
        responses,
      );
    } else if (OS === "windows") {
      await sleep(6000);
      const why = await osClick("e2e-due", "E2E due", null, '{"due":true}');
      if (why) r.fail("toast activation", why);
      else {
        r.check(
          "a click on a toast no live Notification owns is a notificationresponse",
          await waitFor(
            () => responses.some((x) => x.tag === "e2e-due"),
            10000,
          ) &&
            JSON.stringify(responses.find((x) => x.tag === "e2e-due")) ===
              JSON.stringify({
                tag: "e2e-due",
                action: null,
                data: { due: true },
                launch: false,
              }),
          responses,
        );
      }
    } else {
      r.na(
        "a scheduled notification's click is a notificationresponse",
        "macOS delivers a click only from a person clicking the banner",
      );
    }
    desktop.notifications.cancel("e2e-due");
  }
});

r.finish();
