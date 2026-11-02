// Copyright 2018-2026 the Deno authors. MIT license.
// e2e, E2E_DISPLAY=wayland only: the window really is a native Wayland one, seen
// from the compositor's side (sway's tree), and it follows what the compositor
// tells it. The other areas ask the app; this one asks the other end, so a host
// that fell back to something else can't pass here. XWayland is off in this
// session too (linux/sway.conf), so that fallback would not even get a window.

// deno-lint-ignore-file no-explicit-any

import {
  BrowserWindow,
  desktop,
  html,
  once,
  page,
  Report,
  shared,
  sleep,
  titledWindow,
  waitFor,
} from "../_shared/e2e.ts";
import { swayMsg, swayWindow, userClose } from "../_shared/input.ts";

const r = new Report("wayland");

Deno.serve((req) => shared(req) ?? html(page("e2e wayland")));

const TITLE = "E2E Wayland";
await sleep(1500);
const win = titledWindow(TITLE, { width: 640, height: 420 });
await sleep(2000);

await r.step("the session", () => {
  r.check(
    "the app sees a Wayland display and no X display",
    !!Deno.env.get("WAYLAND_DISPLAY") && !Deno.env.get("DISPLAY"),
    {
      WAYLAND_DISPLAY: Deno.env.get("WAYLAND_DISPLAY"),
      DISPLAY: Deno.env.get("DISPLAY"),
    },
  );
});

// The compositor's rect is the whole surface; getSize() is the content, so it is
// smaller by a client-side title bar and frame at most.
const close = (rect: any, [w, h]: number[]) =>
  rect && rect.width >= w && rect.height >= h && rect.width - w <= 40 &&
  rect.height - h <= 60;

await r.step("the compositor's view", async () => {
  const node = await swayWindow(TITLE);
  r.set("node", node);
  r.check(
    "the compositor lists the window, visible",
    node?.visible === true,
    node,
  );
  r.check(
    "it is a native xdg_shell toplevel, not XWayland",
    node?.shell === "xdg_shell",
    node?.shell,
  );
  r.check(
    "its app_id is the app's identifier",
    node?.app_id === r.params.identifier,
    { app_id: node?.app_id, want: r.params.identifier },
  );
  r.check(
    "the compositor's rect is the app's size plus its own decorations",
    close(node?.rect, win.getSize()),
    { rect: node?.rect, size: win.getSize() },
  );

  win.setTitle(`${TITLE} renamed`);
  r.check(
    "setTitle() reaches the compositor",
    await waitFor(async () => !!(await swayWindow(`${TITLE} renamed`)), 5000),
  );
  win.setTitle(TITLE);
  await waitFor(async () => !!(await swayWindow(TITLE)), 5000);
});

await r.step("the compositor drives the window", async () => {
  const before = win.getSize();
  const resized = once(win, "resize", 8000);
  await swayMsg(TITLE, "resize set 900 600");
  await resized;
  r.check(
    "a compositor resize reaches getSize()",
    await waitFor(async () => {
      const node = await swayWindow(TITLE);
      // GTK takes a client-side title bar's height off the size it was given
      // (900 x 575), which a floating window may do.
      return node?.rect?.width === 900 &&
        Math.abs(node?.rect?.height - 600) <= 40 &&
        close(node.rect, win.getSize());
    }, 5000),
    { before, after: win.getSize(), node: (await swayWindow(TITLE))?.rect },
  );

  const entered = once(win, "enterfullscreen", 8000);
  await swayMsg(TITLE, "fullscreen enable");
  r.check(
    "compositor fullscreen fires enterfullscreen",
    (await entered) !== null,
  );
  r.check(
    "...and isFullScreen() follows",
    await waitFor(() => win.isFullScreen(), 5000),
  );
  const left = once(win, "leavefullscreen", 8000);
  await swayMsg(TITLE, "fullscreen disable");
  r.check("leaving it fires leavefullscreen", (await left) !== null);
  r.check(
    "...and isFullScreen() follows",
    await waitFor(() => !win.isFullScreen(), 5000),
  );
});

await r.step("what Wayland can't do is reported as such", async () => {
  const caps = desktop.windowCapabilities();
  r.set("capabilities", caps);
  r.check(
    "windowCapabilities() says a window can't place itself",
    caps.setPosition === false,
    caps.setPosition,
  );
  if (caps.layerShell) {
    const overlay = new BrowserWindow({
      overlay: true,
      frameless: true,
      width: 240,
      height: 160,
      title: "E2E Overlay",
    });
    await sleep(1500);
    r.check("an overlay window is created, visible", overlay.isVisible());
    r.check(
      "it is a layer surface: the compositor has no toplevel for it",
      (await swayWindow("E2E Overlay")) === null,
    );
    overlay.close();
  } else {
    r.na(
      "an overlay window is a layer surface",
      "windowCapabilities().layerShell is false on this host",
    );
  }
});

await r.step("a close from the compositor", async () => {
  const closing = once(win, "close", 10000);
  const why = await userClose(TITLE);
  r.check("the compositor's close reaches the window", why === null, why);
  r.check("it fires a close event", (await closing) !== null);
});

r.finish();
