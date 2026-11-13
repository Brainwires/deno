// Copyright 2018-2026 the Deno authors. MIT license.
// e2e: embedded web views (laufey API 58): BrowserWindow.createWebView(),
// Deno.WebView's methods and events, its isolation from the app (no bridge,
// no bindings, its own storage partition), the URL rules, the per-window
// limit and the close semantics. Where webViewCapabilities() reports no
// engine, createWebView() is checked to refuse with "unsupported".

// deno-lint-ignore-file no-explicit-any

import {
  describeError,
  desktop,
  errorOf,
  html,
  page,
  Report,
  shared,
  sleep,
  titledWindow,
  waitFor,
} from "../_shared/e2e.ts";

const r = new Report("webview");

const TITLE_A = "E2E WebView A";
const TITLE_B = "E2E WebView B";
const ENGINES = ["cef", "wkwebview", "webview2", "webkitgtk"];
const EVENTS = [
  "title",
  "url",
  "load",
  "console",
  "focus-change",
  "new-window",
  "closed",
];
const appOrigin = Deno.env.get("DENO_DESKTOP_APP_ORIGIN") ?? "app://localhost";

// The app's own page (the host window), and a route only the app's own
// documents should read: it answers any origin (CORS *), so a web view that
// reached the app's scheme handler would read it.
Deno.serve((req) => {
  const s = shared(req);
  if (s) return s;
  if (new URL(req.url).pathname === "/__e2e/secret") {
    return new Response("secret", {
      headers: { "access-control-allow-origin": "*" },
    });
  }
  return html(page("e2e webview"));
});

// The web views' pages, on a plain local http server. Page A logs to the
// console, links to B, and reports what it can reach of the app.
let probe: any = null;
const PROBE_SCRIPT = `
console.log("wv-a-console");
const out = { bindings: typeof window.bindings };
try {
  await window.bindings.e2eWvProbe();
  out.call = "called";
} catch (e) {
  out.call = "refused: " + String(e?.message ?? e);
}
try {
  const res = await fetch(${JSON.stringify(appOrigin)} + "/__e2e/secret", {
    cache: "no-store",
    signal: AbortSignal.timeout(5000),
  });
  out.appFetch = "read: " + (await res.text());
} catch (e) {
  out.appFetch = "refused: " + String(e?.message ?? e);
}
await fetch("/probe", { method: "POST", body: JSON.stringify(out) });
`;
const local = Deno.serve(
  { hostname: "127.0.0.1", port: 0, onListen() {} },
  async (req) => {
    const path = new URL(req.url).pathname;
    if (path === "/a") {
      return html(page(TITLE_A, `<a id="to-b" href="/b">B</a>`, PROBE_SCRIPT));
    }
    if (path === "/b") return html(page(TITLE_B));
    if (path === "/probe") {
      const body = await req.json();
      if (probe === null) probe = body;
      return new Response("ok");
    }
    return new Response("not found", { status: 404 });
  },
);
const BASE = `http://127.0.0.1:${local.addr.port}`;
const URL_A = `${BASE}/a`;
const URL_B = `${BASE}/b`;

interface Logged {
  type: string;
  detail: any;
  custom: boolean;
}

/** Every event `view` fires, in order. */
function record(view: any): Logged[] {
  const log: Logged[] = [];
  for (const type of EVENTS) {
    view.addEventListener(type, (e: Event) => {
      log.push({
        type,
        detail: (e as CustomEvent).detail,
        custom: e instanceof CustomEvent,
      });
    });
  }
  return log;
}

/** The first event of `type` after `from` that `match` accepts. */
async function next(
  log: Logged[],
  from: number,
  type: string,
  match: (detail: any) => boolean = () => true,
  ms = 20000,
): Promise<Logged | null> {
  let found: Logged | null = null;
  await waitFor(() => {
    found = log.slice(from).find((e) => e.type === type && match(e.detail)) ??
      null;
    return found !== null;
  }, ms);
  return found;
}

const tiny = { x: 0, y: 0, width: 1, height: 1 };
const near = (a: number, b: number) => Math.abs(a - b) <= 1;
const sameBounds = (a: any, b: any) =>
  a != null && near(a.x, b.x) && near(a.y, b.y) && near(a.width, b.width) &&
  near(a.height, b.height);
const isWebViewError = (e: any, code: string) =>
  e?.name === "WebViewError" && e?.code === code;

const win = titledWindow("E2E WebView Host", { width: 800, height: 600 });
// The bridge's own probe: allowed for any document, so only the absence of
// the bridge keeps a web view's page from calling it.
let probeCalls = 0;
win.bind("e2eWvProbe", () => {
  probeCalls++;
  return "reached";
}, { origins: "*" });
await sleep(2500);

const caps = desktop.webViewCapabilities();
r.set("caps", caps);
r.check(
  "webViewCapabilities() reports every key with its type",
  caps !== null && typeof caps === "object" &&
    ["supported", "capture", "evaluate", "persistentPartitions"]
      .concat("permissionPrompts")
      .every((k) => typeof caps[k] === "boolean") &&
    (caps.engine === null || ENGINES.includes(caps.engine)) &&
    (caps.engine === null) === !caps.supported,
  caps,
);
r.check(
  "Deno.WebView is not constructible",
  (() => {
    try {
      new (Deno as any).WebView();
      return false;
    } catch (e) {
      return e instanceof TypeError;
    }
  })(),
);

if (!caps.supported) {
  const e = await errorOf(() =>
    win.createWebView({ url: URL_A, bounds: { x: 0, y: 0, w: 1 } as any })
  );
  r.check(
    "bad options are refused before anything else: invalid",
    isWebViewError(e, "invalid"),
    e && describeError(e),
  );
  const unsupported = await errorOf(() =>
    win.createWebView({ url: URL_A, bounds: tiny })
  );
  r.check(
    "no engine: createWebView() throws WebViewError unsupported",
    isWebViewError(unsupported, "unsupported"),
    unsupported && describeError(unsupported),
  );
} else {
  await r.step("web view", () => runSupported());
}

async function runSupported() {
  // --- create, load, title, url, console ---
  const bounds = { x: 10, y: 10, width: 400, height: 300 };
  const view = win.createWebView({ url: URL_A, bounds });
  const log = record(view);
  // Through `unknown`, so the checks don't narrow `view` itself.
  const made: unknown = view;
  const isWebView = made instanceof (Deno as any).WebView;
  const isTarget = made instanceof EventTarget;
  r.check(
    "createWebView() returns a Deno.WebView of the window",
    isWebView && isTarget && view.window === win &&
      typeof view.id === "number" && view.closed === false,
    { id: view.id, closed: view.closed },
  );
  const complete = await next(
    log,
    0,
    "load",
    (d) => d.state === "complete" && String(d.url).startsWith(URL_A),
  );
  const states = log.filter((e) => e.type === "load").map((e) =>
    e.detail.state
  );
  r.set("loadStates", states);
  r.check(
    "load events reach complete (after loading)",
    complete !== null && states.indexOf("loading") !== -1 &&
      states.indexOf("loading") < states.indexOf("complete"),
    states,
  );
  r.check(
    "events are CustomEvents with parsed details",
    log.length > 0 &&
      log.every((e) => e.custom && typeof e.detail === "object"),
    log.slice(0, 5),
  );
  const title = await next(log, 0, "title", (d) => d.title === TITLE_A);
  r.check("a title event names page A", title !== null, log);
  const urlA = await next(
    log,
    0,
    "url",
    (d) => String(d.url).startsWith(URL_A),
  );
  r.check(
    "a url event for page A, canGoBack false",
    urlA !== null && urlA.detail.canGoBack === false &&
      typeof urlA.detail.canGoForward === "boolean",
    urlA?.detail,
  );
  const consoleEvent = await next(
    log,
    0,
    "console",
    (d) => String(d.message).includes("wv-a-console"),
  );
  r.check(
    "page A's console.log is a console event, level log",
    consoleEvent !== null && consoleEvent.detail.level === "log",
    consoleEvent?.detail,
  );

  // --- isolation: no bridge, no bindings, not the app's scheme ---
  await waitFor(() => probe !== null, 15000);
  r.set("probe", probe);
  r.check(
    "the web view's page has no bindings (window.bindings undefined)",
    probe !== null && probe.bindings === "undefined" &&
      String(probe.call).startsWith("refused") && probeCalls === 0,
    { probe, probeCalls },
  );
  if (!/^https?:/.test(appOrigin)) {
    r.check(
      "the web view's page can't read the app's origin",
      probe !== null && String(probe.appFetch).startsWith("refused"),
      { appOrigin, appFetch: probe?.appFetch },
    );
  }

  // --- navigate, back ---
  let mark = log.length;
  view.navigate(URL_B);
  const urlB = await next(
    log,
    mark,
    "url",
    (d) => String(d.url).startsWith(URL_B),
  );
  r.check(
    "navigate(B): a url event with canGoBack true",
    urlB !== null && urlB.detail.canGoBack === true,
    urlB?.detail,
  );
  await next(
    log,
    mark,
    "load",
    (d) => d.state === "complete" && String(d.url).startsWith(URL_B),
  );
  mark = log.length;
  view.back();
  const back = await next(
    log,
    mark,
    "url",
    (d) => String(d.url).startsWith(URL_A),
  );
  r.check(
    "back(): a url event for page A, canGoForward true",
    back !== null && back.detail.canGoForward === true,
    back?.detail,
  );
  await next(
    log,
    mark,
    "load",
    (d) => d.state === "complete" && String(d.url).startsWith(URL_A),
    10000,
  );
  await next(log, mark, "title", (d) => d.title === TITLE_A, 5000);

  // --- state, setBounds, setVisible ---
  const state = view.state();
  r.set("state", state);
  r.check(
    "state() has every key with its type",
    typeof state.url === "string" && typeof state.title === "string" &&
      ["loading", "interactive", "complete", "failed"].includes(state.load) &&
      typeof state.canGoBack === "boolean" &&
      typeof state.canGoForward === "boolean" &&
      typeof state.visible === "boolean" &&
      ["x", "y", "width", "height"].every((k) =>
        typeof state.bounds?.[k] === "number"
      ),
    state,
  );
  r.check(
    "state() after back(): page A, complete, can go forward, visible, at its bounds",
    String(state.url).startsWith(URL_A) && state.title === TITLE_A &&
      state.load === "complete" && state.canGoForward === true &&
      state.visible === true && sameBounds(state.bounds, bounds),
    state,
  );
  const moved = { x: 20, y: 30, width: 320, height: 240 };
  view.setBounds(moved);
  await sleep(300);
  r.check(
    "setBounds() shows in state()",
    sameBounds(view.state().bounds, moved),
    view.state().bounds,
  );
  view.setVisible(false);
  await sleep(300);
  const hidden = view.state().visible;
  view.setVisible(true);
  await sleep(300);
  r.check(
    "setVisible() shows in state()",
    hidden === false && view.state().visible === true,
    { hidden, shown: view.state().visible },
  );
  const badBounds = await errorOf(() =>
    view.setBounds({ x: NaN, y: 0, width: 1, height: 1 })
  );
  r.check(
    "setBounds() with a non-finite number: invalid",
    isWebViewError(badBounds, "invalid"),
    badBounds && describeError(badBounds),
  );

  // --- evaluate ---
  if (caps.evaluate) {
    const t = await view.evaluate("document.title");
    r.check('evaluate("document.title") is page A\'s title', t === TITLE_A, t);
    const two = await view.evaluate("1+1");
    r.check('evaluate("1+1") is 2', two === 2, two);
    const obj = await view.evaluate("Promise.resolve({a:1})");
    r.check(
      "evaluate() awaits a promise: {a:1}",
      JSON.stringify(obj) === JSON.stringify({ a: 1 }),
      obj,
    );
    const undef = await view.evaluate("undefined");
    r.check('evaluate("undefined") is null', undef === null, undef);
    const threw = await errorOf(view.evaluate("throw new Error('x')"));
    r.check(
      "evaluate() of a throw rejects with code error",
      isWebViewError(threw, "error") && String(threw?.message).includes("x"),
      threw && describeError(threw),
    );
    const fn = await errorOf(view.evaluate("(() => 1)"));
    r.check(
      "evaluate() of a function rejects with code not_serializable",
      isWebViewError(fn, "not_serializable"),
      fn && describeError(fn),
    );
    const bridge = await view.evaluate("typeof globalThis.bindings");
    r.check(
      "evaluate() sees no bindings either",
      bridge === "undefined",
      bridge,
    );
  } else {
    const e = await errorOf(view.evaluate("1+1"));
    r.check(
      "evaluate() without the capability rejects with a WebViewError",
      e?.name === "WebViewError",
      e && describeError(e),
    );
  }

  // --- capture ---
  const PNG_SIGNATURE = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
  if (caps.capture) {
    const bytes = await view.capture();
    r.check(
      "capture() returns PNG bytes",
      bytes instanceof Uint8Array && bytes.length > 8 &&
        PNG_SIGNATURE.every((b, i) => bytes[i] === b),
      bytes instanceof Uint8Array
        ? Array.from(bytes.slice(0, 8))
        : String(bytes),
    );
    const jpeg = await view.capture({ format: "jpeg", quality: 50 });
    r.check(
      'capture({ format: "jpeg" }) returns JPEG bytes',
      jpeg instanceof Uint8Array && jpeg[0] === 0xff && jpeg[1] === 0xd8,
      jpeg instanceof Uint8Array ? Array.from(jpeg.slice(0, 4)) : String(jpeg),
    );
  } else {
    const e = await errorOf(view.capture());
    r.check(
      "capture() without the capability rejects with a WebViewError",
      e?.name === "WebViewError",
      e && describeError(e),
    );
  }

  // --- URL rules ---
  for (const url of ["file:///etc/hosts", "javascript:1"]) {
    const e = await errorOf(() => view.navigate(url));
    r.check(
      `navigate(${JSON.stringify(url)}) throws not_allowed_url`,
      isWebViewError(e, "not_allowed_url"),
      e && describeError(e),
    );
  }
  const dataUrl = await errorOf(() =>
    win.createWebView({ url: "data:text/html,x", bounds: tiny })
  );
  r.check(
    "createWebView() with a data: URL throws not_allowed_url",
    isWebViewError(dataUrl, "not_allowed_url"),
    dataUrl && describeError(dataUrl),
  );
  for (
    const [name, options] of [
      ["no bounds", { url: URL_B }],
      ["a bad partition", { url: URL_B, bounds: tiny, partition: ":x" }],
      ["a bad allowPopups", { url: URL_B, bounds: tiny, allowPopups: "x" }],
    ] as const
  ) {
    const e = await errorOf(() => win.createWebView(options as any));
    r.check(
      `createWebView() with ${name} throws invalid`,
      isWebViewError(e, "invalid"),
      e && describeError(e),
    );
  }

  // --- partitions ---
  if (caps.evaluate) {
    await view.evaluate("localStorage.setItem('wv-k', 'default'), 1");
  }
  const eph = win.createWebView({
    url: URL_A,
    bounds: { x: 420, y: 10, width: 200, height: 150 },
    partition: "ephemeral:t",
  });
  const ephLog = record(eph);
  const ephLoaded = await next(
    ephLog,
    0,
    "load",
    (d) => d.state === "complete" && String(d.url).startsWith(URL_A),
  );
  r.check(
    'a view in partition "ephemeral:t" loads',
    ephLoaded !== null,
    ephLog.filter((e) => e.type === "load").map((e) => e.detail),
  );
  if (caps.evaluate) {
    const own = await eph.evaluate("localStorage.getItem('wv-k')");
    const shared = await view.evaluate("localStorage.getItem('wv-k')");
    r.check(
      "partitions don't share storage",
      own === null && shared === "default",
      { own, shared },
    );
  }
  eph.close();
  await next(ephLog, 0, "closed", () => true, 10000);

  // --- the per-window limit: 32 views, the 33rd refused ---
  const extra: any[] = [];
  let refused: any = null;
  for (let i = 0; i < 40 && refused === null; i++) {
    try {
      extra.push(
        win.createWebView({ url: URL_B, bounds: tiny, visible: false }),
      );
    } catch (e) {
      refused = e;
    }
  }
  r.check(
    "the 33rd view of a window throws limit",
    isWebViewError(refused, "limit") && extra.length === 31,
    { created: extra.length + 1, error: refused && describeError(refused) },
  );
  let extraClosed = 0;
  for (const v of extra) {
    v.addEventListener("closed", () => extraClosed++);
    v.close();
  }
  await waitFor(() => extraClosed === extra.length, 15000);
  r.check(
    "closing them fires closed on each",
    extraClosed === extra.length,
    { extraClosed, of: extra.length },
  );

  // --- close ---
  let closedCount = 0;
  view.addEventListener("closed", () => closedCount++);
  view.close();
  view.close();
  r.check("close() marks it closed at once", view.closed === true);
  await waitFor(() => closedCount > 0, 10000);
  await sleep(1500);
  const lastType = log[log.length - 1]?.type;
  r.check(
    "close(): closed fires exactly once, last",
    closedCount === 1 && lastType === "closed",
    { closedCount, lastType },
  );
  const navClosed = await errorOf(() => view.navigate(URL_B));
  const stateClosed = await errorOf(() => view.state());
  const evalClosed = await errorOf(view.evaluate("1"));
  const captureClosed = await errorOf(view.capture());
  const setterThrew = await errorOf(() => {
    view.setBounds(moved);
    view.setVisible(false);
    view.back();
    view.reload();
  });
  r.check(
    "a closed view: navigate / state throw closed, evaluate / capture reject closed, setters are no-ops",
    isWebViewError(navClosed, "closed") &&
      isWebViewError(stateClosed, "closed") &&
      isWebViewError(evalClosed, "closed") &&
      isWebViewError(captureClosed, "closed") && setterThrew === null,
    [navClosed, stateClosed, evalClosed, captureClosed, setterThrew].map((e) =>
      e && describeError(e)
    ),
  );

  // --- a window's close closes its views ---
  const win2 = titledWindow("E2E WebView Second", { width: 500, height: 400 });
  await sleep(1500);
  const counts = [0, 0];
  const views = [0, 1].map((i) => {
    const v = win2.createWebView({
      url: URL_B,
      bounds: { x: i * 100, y: 0, width: 90, height: 90 },
    });
    v.addEventListener("closed", () => counts[i]++);
    return v;
  });
  await sleep(1500);
  win2.close();
  await waitFor(() => counts.every((c) => c > 0), 15000);
  await sleep(1500);
  r.check(
    "closing the window fires closed once on each of its views",
    counts.every((c) => c === 1) && views.every((v) => v.closed),
    counts,
  );
  const afterWindow = await errorOf(() =>
    win2.createWebView({ url: URL_B, bounds: tiny })
  );
  r.check(
    "createWebView() on a closed window throws closed",
    isWebViewError(afterWindow, "closed"),
    afterWindow && describeError(afterWindow),
  );
}

r.done();
const quitting = desktop.quit();
r.check("quit() with nothing canceling returns true", quitting === true);
await sleep(30000);
r.fail("quit() did not end the process within 30 s");
Deno.exit(3);
