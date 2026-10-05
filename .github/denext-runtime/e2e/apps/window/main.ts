// Copyright 2018-2026 the Deno authors. MIT license.
// e2e: the window API (BrowserWindow state, events, size limits, bounds,
// placement, chrome, cancelable close, quit, the tray rule, the initial
// window from app.json). A feature the backend reports unsupported in
// windowCapabilities() is checked to be a no-op that says so, not skipped.

// deno-lint-ignore-file no-explicit-any

import {
  BrowserWindow,
  describeError,
  desktop,
  html,
  once,
  OS,
  page,
  Report,
  shared,
  sleep,
  titledWindow,
  waitFor,
} from "../_shared/e2e.ts";
import { macWindowState, userClose } from "../_shared/input.ts";

const r = new Report("window");

let initial: any = null;
let initialAt = 0;
// The initial window's first animation frame: the page asks for one as it
// loads, while the window is still hidden; it runs once the window is
// revealed and on screen (a window opened behind the active app reads as
// occluded, and the engine runs no frames for it).
let frame: any = null;
let frameAt = 0;
Deno.serve(async (req) => {
  const s = shared(req);
  if (s) return s;
  const url = new URL(req.url);
  if (url.pathname === "/initial") {
    const body = await req.json();
    if (initial === null) {
      initial = body;
      initialAt = Date.now();
    }
    return new Response("ok");
  }
  if (url.pathname === "/frame") {
    const body = await req.json();
    if (frame === null) {
      frame = body;
      frameAt = Date.now();
    }
    return new Response("ok");
  }
  return html(page(
    "e2e window",
    "",
    `fetch("/initial", { method: "POST", body: JSON.stringify({ innerWidth, innerHeight, outerWidth, outerHeight }) });
requestAnimationFrame(() => fetch("/frame", { method: "POST", body: JSON.stringify({ visibilityState: document.visibilityState, innerWidth, innerHeight, outerWidth, outerHeight }) }));`,
  ));
});

// The initial window (Deno.serve's) reports its size first; window A is
// created after that, so the report is the initial window's own.
await waitFor(() => initial !== null, 30000);
// A tray-style panel created before any plain window: it can't be the framed
// bootstrap window, so it is a new frameless window, and the bootstrap
// window is still there for the next plain BrowserWindow (the first
// BrowserWindow used to adopt it whatever its options, dropping
// frameless / noActivate / transparent).
const panel = new BrowserWindow({
  frameless: true,
  noActivate: true,
  width: 240,
  height: 160,
});
const adopted = new BrowserWindow();
await sleep(1000);
const panelInfo = {
  panel: panel.windowId,
  adopted: adopted.windowId,
  panelSize: panel.getSize(),
  adoptedSize: adopted.getSize(),
};
panel.close();

const TITLE_A = "E2E Window A";
const TITLE_B = "E2E Window B";
const win = titledWindow(TITLE_A, { width: 640, height: 420 });
const stateEvents: string[] = [];
for (
  const t of [
    "maximize",
    "unmaximize",
    "minimize",
    "restore",
    "enterfullscreen",
    "leavefullscreen",
  ]
) {
  win.addEventListener(t, () => stateEvents.push(t));
}

await sleep(2500);
await r.step("window API", async () => {
  // --- the initial window, sized by app.json's initialWindow ---
  const want = r.params.initialWindow ?? {};
  r.check(
    "the initial window takes its size from app.json initialWindow",
    initial !== null &&
      (Math.abs(initial.innerWidth - want.width) <= 2 ||
        Math.abs(initial.outerWidth - want.width) <= 2) &&
      (Math.abs(initial.innerHeight - want.height) <= 2 ||
        Math.abs(initial.outerHeight - want.height) <= 40),
    { initial, want },
  );

  r.check(
    "a frameless panel is a new window; the next plain one adopts the initial window",
    // WebKitGTK can report the initial page's innerWidth / innerHeight as
    // 0 (it loaded before the window was mapped): its outer size counts
    // then, as for the initialWindow check above.
    panelInfo.panel !== panelInfo.adopted &&
      Math.abs(panelInfo.panelSize[0] - 240) <= 2 &&
      (Math.abs(panelInfo.adoptedSize[0] - initial.innerWidth) <= 2 ||
        Math.abs(panelInfo.adoptedSize[0] - initial.outerWidth) <= 2) &&
      (Math.abs(panelInfo.adoptedSize[1] - initial.innerHeight) <= 40 ||
        Math.abs(panelInfo.adoptedSize[1] - initial.outerHeight) <= 40),
    { panelInfo, initial },
  );

  await waitFor(() => frame !== null, 10000);
  r.check(
    "the initial window draws a frame within 5 s of its page loading",
    frame !== null && frameAt - initialAt <= 5000,
    { frame, afterPageLoadMs: frame ? frameAt - initialAt : null },
  );
  r.check(
    "the initial window's page is visible when it draws",
    frame?.visibilityState === "visible",
    frame,
  );
  if (OS === "darwin") {
    // WKWebView answered 0 until laufey gave WebKit the window frame.
    r.check(
      "the page's outerWidth / outerHeight are the window's, not 0",
      frame !== null && frame.innerWidth > 0 &&
        frame.outerWidth >= frame.innerWidth &&
        frame.outerHeight > frame.innerHeight,
      frame,
    );
  } else {
    r.set("initialFrameSizes", frame);
  }

  // laufey's C ABI can't carry an embedded NUL: refused at the op boundary
  // (it used to abort the whole app).
  const nulErrors = [
    () => win.setTitle("a\0b"),
    () => win.navigate("app://x\0y"),
    () => (navigator as any).clipboard.writeText("a\0b"),
  ].map((f) => {
    try {
      const p = f();
      return p instanceof Promise ? p.then(() => null, (e) => e) : null;
    } catch (e) {
      return e;
    }
  });
  const nulResults = await Promise.all(nulErrors);
  r.check(
    "a string with a NUL is a TypeError, not a crash",
    nulResults.every((e) => e instanceof TypeError),
    nulResults.map((e) => e ? describeError(e) : null),
  );

  const caps = desktop.windowCapabilities();
  r.set("capabilities", caps);
  const keys = [
    "state",
    "stateEvents",
    "sizeConstraints",
    "screens",
    "displayEvents",
    "titleBarHidden",
    "titleBarHiddenInset",
    "windowButtonPosition",
    "mica",
    "acrylic",
    "tabbed",
    "vibrancy",
    "normalBounds",
    "keepAlive",
    "setPosition",
    "fileDrop",
    "fileDropEnterPaths",
    "fileDragOut",
    "fileDialogs",
    "fileDialogFilesAndDirectories",
    "fileDialogModal",
  ];
  r.check(
    "windowCapabilities() reports every key as a boolean",
    keys.every((k) => typeof caps[k] === "boolean"),
    caps,
  );

  // --- screens ---
  const screens = desktop.screens();
  r.set("screens", screens);
  if (caps.screens) {
    r.check(
      "screens(): sane displays, the primary first",
      screens.length > 0 && screens[0].isPrimary &&
        screens.every((s: any) =>
          s.bounds.width > 0 && s.bounds.height > 0 && s.workArea.width > 0 &&
          s.workArea.height <= s.bounds.height && s.scaleFactor >= 1 &&
          typeof s.id === "number"
        ),
      screens,
    );
    r.check(
      "getPrimaryScreen() is screens()[0]",
      JSON.stringify(desktop.getPrimaryScreen()) === JSON.stringify(screens[0]),
    );
    r.check(
      "a window knows its screen",
      win.getScreen()?.id === screens[0].id || screens.length > 1,
      win.getScreen(),
    );
  } else {
    r.check(
      "screens unsupported: screens() is empty and getPrimaryScreen() null",
      screens.length === 0 && desktop.getPrimaryScreen() === null,
    );
  }

  // --- state round trips ---
  if (caps.state) {
    const before = win.getBounds();
    r.set("boundsBefore", before);
    win.maximize();
    const maxEv = await once(win, "maximize", 8000);
    r.check(
      "maximize() maximizes",
      await waitFor(() => win.isMaximized(), 5000),
    );
    if (caps.stateEvents) r.check("a maximize event fires", maxEv !== null);
    if (caps.normalBounds) {
      const normal = win.getNormalBounds();
      r.check(
        "getNormalBounds() while maximized is the bounds before",
        JSON.stringify(normal) === JSON.stringify(before),
        { normal, before },
      );
    }
    win.unmaximize();
    const unmaxEv = await once(win, "unmaximize", 8000);
    r.check(
      "unmaximize() restores",
      await waitFor(() => !win.isMaximized(), 5000),
    );
    if (caps.stateEvents) {
      r.check("an unmaximize event fires", unmaxEv !== null);
    }
    // Let unmaximize's animation finish: AppKit drops a miniaturize asked
    // for during a zoom animation (as it does a fullscreen toggle).
    await sleep(1000);
    win.minimize();
    const minEv = await once(win, "minimize", 8000);
    const minimized = await waitFor(() => win.isMinimized(), 5000);
    r.check(
      "minimize() minimizes",
      minimized,
      minimized ? undefined : macWindowState(TITLE_A),
    );
    if (caps.stateEvents) r.check("a minimize event fires", minEv !== null);
    win.restore();
    const restEv = await once(win, "restore", 8000);
    r.check(
      "restore() un-minimizes",
      await waitFor(() => !win.isMinimized(), 5000),
    );
    if (caps.stateEvents) r.check("a restore event fires", restEv !== null);
    await sleep(1000);
    win.setFullScreen(true);
    const fsEv = await once(win, "enterfullscreen", 10000);
    r.check(
      "setFullScreen(true) enters fullscreen",
      await waitFor(() => win.isFullScreen(), 5000),
    );
    if (caps.stateEvents) {
      r.check("an enterfullscreen event fires", fsEv !== null);
    }
    win.setFullScreen(false);
    const lfsEv = await once(win, "leavefullscreen", 10000);
    r.check(
      "setFullScreen(false) leaves fullscreen",
      await waitFor(() => !win.isFullScreen(), 5000),
    );
    if (caps.stateEvents) {
      r.check("a leavefullscreen event fires", lfsEv !== null);
    }
    await sleep(800);
    r.set("stateEvents", stateEvents);
  } else {
    win.maximize();
    await sleep(500);
    r.check("state unsupported: maximize() is a no-op", !win.isMaximized());
  }

  // --- size limits ---
  if (caps.sizeConstraints) {
    win.setMinimumSize(500, 350);
    win.setMaximumSize(900, 700);
    r.check(
      "getMinimumSize / getMaximumSize read back",
      JSON.stringify(win.getMinimumSize()) === "[500,350]" &&
        JSON.stringify(win.getMaximumSize()) === "[900,700]",
      [win.getMinimumSize(), win.getMaximumSize()],
    );
    win.setSize(100, 100);
    await sleep(700);
    const small = win.getSize();
    win.setSize(3000, 3000);
    await sleep(700);
    const big = win.getSize();
    r.check(
      "setSize clamps to the limits",
      small[0] >= 500 && small[1] >= 350 && big[0] <= 900 && big[1] <= 700,
      { small, big },
    );
    win.setMinimumSize(0, 0);
    win.setMaximumSize(0, 0);
    win.setSize(640, 420);
    await sleep(500);
  }

  // --- placement: a saved position on a display that is gone ---
  if (caps.setPosition && caps.screens) {
    win.setBounds({ x: -50000, y: -50000, width: 640, height: 460 });
    await sleep(800);
    const placed = win.getBounds();
    const wa = desktop.getPrimaryScreen().workArea;
    r.check(
      "setBounds onto a missing display lands on the primary work area",
      placed.x >= wa.x - 1 && placed.y >= wa.y - 1 &&
        placed.x + placed.width <= wa.x + wa.width + 1 &&
        placed.y + placed.height <= wa.y + wa.height + 1,
      { placed, wa },
    );
    win.setBounds({ x: wa.x + 40, y: wa.y + 40 });
    await sleep(500);
    const moved = win.getBounds();
    r.check(
      "setBounds moves the window",
      Math.abs(moved.x - (wa.x + 40)) <= 2 &&
        Math.abs(moved.y - (wa.y + 40)) <= 2,
      moved,
    );
  }
  const cb = win.getContentBounds();
  const [iw, ih] = win.getSize();
  r.check(
    "getContentBounds() is getInnerPosition() + getSize()",
    cb.width === iw && cb.height === ih,
    { cb, size: [iw, ih] },
  );

  // --- chrome: each setter answers what windowCapabilities() promised ---
  const chrome = {
    hiddenInset: win.setTitleBarStyle("hiddenInset"),
    buttons: win.setWindowButtonPosition({ x: 18, y: 18 }),
    buttonsReset: win.setWindowButtonPosition(null),
    defaultStyle: win.setTitleBarStyle("default"),
    vibrancy: win.setVibrancy("sidebar"),
    vibrancyOff: win.setVibrancy(null),
    mica: win.setBackgroundMaterial("mica"),
    none: win.setBackgroundMaterial("none"),
  };
  r.set("chrome", chrome);
  r.check(
    "chrome setters return what windowCapabilities() reports",
    chrome.hiddenInset === caps.titleBarHiddenInset &&
      chrome.buttons === caps.windowButtonPosition &&
      chrome.vibrancy === caps.vibrancy && chrome.mica === caps.mica,
    { chrome, caps },
  );

  // --- a user close, canceled ---
  let canceled = 0;
  const cancel = (e: Event) => {
    canceled++;
    e.preventDefault();
  };
  win.addEventListener("close", cancel);
  const why = await userClose(TITLE_A);
  if (why) {
    r.fail("a user close reaches the window", why);
  } else {
    r.check(
      "a user close fires a close event",
      await waitFor(() => canceled > 0, 10000),
    );
    await sleep(1000);
    r.check(
      "preventDefault() keeps the window open",
      !win.isClosed() && win.isVisible(),
    );
  }

  // --- a close whose listener never answers closes after the timeout ---
  const b = titledWindow(TITLE_B, { width: 300, height: 200, x: 60, y: 80 });
  let bEvent = false;
  b.addEventListener("close", (e: Event) => {
    bEvent = true;
    e.preventDefault();
    const end = Date.now() + 7000;
    while (Date.now() < end) { /* the runtime is blocked */ }
  });
  await sleep(2000);
  const whyB = await userClose(TITLE_B);
  if (whyB) {
    r.fail("a user close reaches window B", whyB);
  } else {
    r.check("B: the close event fired", await waitFor(() => bEvent, 10000));
    await sleep(1000);
    r.check(
      "B: a close the runtime doesn't answer within 5 s closes the window",
      await waitFor(() => b.isClosed(), 5000),
    );
  }

  // --- close() of a window a WebGPU surface holds ---
  try {
    const adapter = await (navigator as any).gpu?.requestAdapter();
    if (!adapter) {
      r.na(
        "close() of a WebGPU window closes it (isClosed)",
        "no WebGPU adapter on this runner",
      );
    } else {
      const g = new BrowserWindow({
        title: "e2e webgpu",
        width: 200,
        height: 200,
      });
      await sleep(500);
      let surface: unknown = null;
      try {
        surface = g.getNativeWindow();
      } catch (e) {
        r.na(
          "close() of a WebGPU window closes it (isClosed)",
          `no native surface on this backend: ${describeError(e)}`,
        );
      }
      g.close();
      if (surface !== null) {
        r.check(
          "close() of a WebGPU window closes it (isClosed)",
          await waitFor(() => g.isClosed(), 5000),
        );
      }
    }
  } catch (e) {
    r.fail("close() of a WebGPU window", describeError(e));
  }

  // --- quit(), canceled ---
  const cancelQuit = (e: Event) => e.preventDefault();
  desktop.addEventListener("beforequit", cancelQuit);
  r.check(
    "quit() canceled by a beforequit listener returns false",
    desktop.quit() === false,
  );
  desktop.removeEventListener("beforequit", cancelQuit);
  r.check(
    "quit() canceled by a window's close listener returns false",
    desktop.quit() === false,
  );
  win.removeEventListener("close", cancel);

  // --- workers get no native classes ---
  const w = new Worker(new URL("./worker.ts", import.meta.url), {
    type: "module",
  });
  const fromWorker: any = await new Promise((resolve) => {
    w.onmessage = (e) => resolve(e.data);
    w.onerror = (e) => {
      e.preventDefault();
      resolve(`worker error: ${e.message}`);
    };
    setTimeout(() => resolve("worker timeout"), 15000);
  });
  w.terminate();
  r.check(
    "workers have no BrowserWindow / Dock / Tray / Notification",
    ["BrowserWindow", "Dock", "Tray", "Notification"].every((k) =>
      fromWorker?.[k] === "undefined"
    ),
    fromWorker,
  );

  // --- the tray rule ---
  r.check(
    "quitOnLastWindowClosed defaults to true",
    desktop.quitOnLastWindowClosed === true,
  );
  // Linux (laufey API 45): with no tray host (this Xvfb session has no
  // StatusNotifierWatcher and no XEmbed tray) the constructor refuses with
  // the probe's reason instead of returning a tray no one can see.
  const pending = desktop.platformFeatures();
  r.check(
    "platformFeatures() answers off the JavaScript thread (a promise)",
    pending instanceof Promise,
  );
  const features = await pending;
  r.check(
    "platformFeatures() answers for this OS",
    features?.os === (OS === "darwin" ? "macos" : OS),
    features,
  );
  r.check(
    "platformFeatures() reports the notification server",
    features != null && "notificationServer" in features &&
      "notificationReason" in features,
    features,
  );
  r.check(
    "Deno.desktop has the platformfeatureschanged event handler",
    "onplatformfeatureschanged" in desktop,
  );
  let tray: any = null;
  try {
    tray = new (Deno as any).Tray();
  } catch (e) {
    if (features?.trayHost === false) {
      r.check(
        "no tray host: new Deno.Tray() throws NotSupported with the reason",
        e instanceof Deno.errors.NotSupported &&
          typeof features.trayReason === "string" &&
          String((e as Error).message).includes(features.trayReason),
        describeError(e),
      );
      r.na("the tray rule", "no tray host in this session");
    } else {
      r.fail("new Deno.Tray()", describeError(e));
    }
  }
  if (!tray && features?.trayHost) {
    r.fail("new Deno.Tray() with a tray host", "no tray");
  }
  if (tray) {
    r.check(
      "creating a Tray turns quitOnLastWindowClosed off",
      desktop.quitOnLastWindowClosed === false,
    );
    tray.destroy();
    r.check(
      "destroying the tray leaves the setting alone",
      desktop.quitOnLastWindowClosed === false,
    );
    desktop.quitOnLastWindowClosed = true;
  }
});

r.done();
// The runner checks that quit() ends the process.
const quitting = desktop.quit();
r.check("quit() with nothing canceling returns true", quitting === true);
await sleep(30000);
r.fail("quit() did not end the process within 30 s");
Deno.exit(3);
