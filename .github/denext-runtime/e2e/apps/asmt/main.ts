// Copyright 2018-2026 the Deno authors. MIT license.
// e2e: Deno.desktop.runOnMainThread and Deno.desktop.authSession (laufey API
// 42, cancel() 43). runOnMainThread is proven to run on the UI thread (and not the
// JavaScript thread) with each OS's own thread id; authSession runs a real
// ASWebAuthenticationSession on macOS against a loopback identity provider
// (an ephemeral session, so no prompt), then busy and the anchor window
// closing (cancelled), then the app cancelling (cancel(), after which the
// next session completes); Windows and Linux have no OS auth session and must
// say so (not_supported).

// deno-lint-ignore-file no-explicit-any

import {
  describeError,
  desktop,
  html,
  page,
  Report,
  shared,
  sleep,
  titledWindow,
  within,
} from "../_shared/e2e.ts";

const r = new Report("asmt");
const TITLE = "E2E Asmt";
const rejects = async (f: () => unknown) => {
  try {
    await f();
    return "resolved";
  } catch (e) {
    return describeError(e);
  }
};

Deno.serve((req) => shared(req) ?? html(page("e2e asmt")));

// --- native functions for runOnMainThread ---------------------------------
const os = Deno.build.os;
type Ptr = Deno.PointerValue;
function nativeSymbols() {
  if (os === "windows") {
    const k32 = Deno.dlopen("kernel32.dll", {
      GetModuleHandleW: { parameters: ["buffer"], result: "pointer" },
      GetProcAddress: { parameters: ["pointer", "buffer"], result: "pointer" },
      GetCurrentThreadId: { parameters: [], result: "u32" },
    });
    const u32 = Deno.dlopen("user32.dll", {
      FindWindowW: { parameters: ["pointer", "buffer"], result: "pointer" },
      GetWindowThreadProcessId: {
        parameters: ["pointer", "pointer"],
        result: "u32",
      },
    });
    const wide = (s: string) => {
      const b = new Uint16Array(s.length + 1);
      for (let i = 0; i < s.length; i++) b[i] = s.charCodeAt(i);
      return new Uint8Array(b.buffer);
    };
    const cstr = (s: string) => new TextEncoder().encode(s + "\0");
    const lookup = (mod: string, name: string): Ptr =>
      k32.symbols.GetProcAddress(
        k32.symbols.GetModuleHandleW(wide(mod)),
        cstr(name),
      );
    return {
      threadFn: lookup("kernel32.dll", "GetCurrentThreadId"),
      identity: lookup("ntdll.dll", "_abs64") ??
        lookup("ucrtbase.dll", "llabs"),
      here: () => BigInt(k32.symbols.GetCurrentThreadId()),
      uiThreadOwner: () => {
        const hwnd = u32.symbols.FindWindowW(null, wide(TITLE));
        return hwnd
          ? BigInt(u32.symbols.GetWindowThreadProcessId(hwnd, null))
          : -1n;
      },
    };
  }
  const libc = os === "darwin" ? "/usr/lib/libSystem.B.dylib" : "libc.so.6";
  const lib = Deno.dlopen(
    libc,
    {
      dlsym: { parameters: ["pointer", "buffer"], result: "pointer" },
      ...(os === "darwin"
        ? { pthread_main_np: { parameters: [], result: "i32" } }
        : { gettid: { parameters: [], result: "i32" } }),
    } as const,
  );
  // RTLD_DEFAULT: (void*)-2 on macOS, NULL with glibc.
  const handle = os === "darwin"
    ? Deno.UnsafePointer.create(0xfffffffffffffffen)
    : null;
  const sym = (name: string) =>
    lib.symbols.dlsym(handle, new TextEncoder().encode(name + "\0"));
  const s = lib.symbols as any;
  return {
    threadFn: sym(os === "darwin" ? "pthread_main_np" : "gettid"),
    identity: sym("llabs"),
    here: () => BigInt(os === "darwin" ? s.pthread_main_np() : s.gettid()),
    uiThreadOwner: () => os === "darwin" ? 1n : BigInt(Deno.pid),
  };
}

// --- a loopback identity provider (raw TCP: Deno.serve is the app's) ------
function startIdp(): string {
  const listener = Deno.listen({ hostname: "127.0.0.1", port: 0 });
  (async () => {
    for await (const conn of listener) {
      (async () => {
        const buf = new Uint8Array(8192);
        // A session the app cancels (cancel()) or whose sheet closes can
        // drop the connection mid-request: that is not a failure here.
        const n = await conn.read(buf).catch(() => null);
        if (n === null) {
          try {
            conn.close();
          } catch { /* closed */ }
          return;
        }
        const head = new TextDecoder().decode(buf.subarray(0, n));
        const path = head.split(" ")[1] ?? "/";
        const state = new URL(path, "http://x").searchParams.get("state");
        const res = path.startsWith("/redirect")
          ? `HTTP/1.1 302 Found\r\nLocation: dnxasmt://cb?code=probe-code&state=${state}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n`
          : "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 20\r\nConnection: close\r\n\r\n<p>signing in…</p>";
        try {
          await conn.write(new TextEncoder().encode(res));
        } catch { /* closed */ }
        try {
          conn.close();
        } catch { /* closed */ }
      })();
    }
  })();
  return `http://127.0.0.1:${(listener.addr as Deno.NetAddr).port}`;
}

const titled = titledWindow(TITLE);
await sleep(3000);

await r.step("runOnMainThread", async () => {
  r.check(
    "Deno.desktop.runOnMainThread is a function",
    typeof desktop.runOnMainThread === "function",
  );
  const nat = nativeSymbols();
  r.check(
    "the native test functions resolve",
    nat.threadFn !== null && nat.identity !== null,
  );
  const onUi = await desktop.runOnMainThread(nat.threadFn);
  const ui = onUi & 0xffffffffn;
  const here = nat.here();
  const owner = nat.uiThreadOwner();
  r.set("threads", { ui: `${ui}`, js: `${here}`, owner: `${owner}` });
  r.check(
    "fn runs on the UI thread (the one that owns the windows)",
    ui === owner,
    { ui: `${ui}`, owner: `${owner}` },
  );
  r.check("…not on the JavaScript thread", ui !== here, {
    ui: `${ui}`,
    js: `${here}`,
  });
  r.check(
    "the context reaches fn and its return value comes back",
    (await desktop.runOnMainThread(
      nat.identity,
      Deno.UnsafePointer.create(123456789n),
    )) === 123456789n,
  );
  const fnPtr = new Deno.UnsafeFnPointer(
    nat.identity as Deno.PointerObject<any>,
    {
      parameters: ["pointer"],
      result: "pointer",
    } as const,
  );
  r.check(
    "a Deno.UnsafeFnPointer is accepted",
    (await desktop.runOnMainThread(fnPtr, Deno.UnsafePointer.create(42n))) ===
      42n,
  );
  // JavaScript would make the UI thread wait for the JavaScript thread.
  const cb = new Deno.UnsafeCallback(
    { parameters: ["pointer"], result: "pointer" } as const,
    (p) => p,
  );
  const cbError = await desktop.runOnMainThread(cb, null).then(
    () => null,
    (e: Error) => e,
  );
  r.check(
    "a Deno.UnsafeCallback is refused with a TypeError",
    cbError instanceof TypeError,
    String(cbError),
  );
  cb.close();
  const many = await Promise.all(
    Array.from(
      { length: 50 },
      (_, i) =>
        desktop.runOnMainThread(
          nat.identity,
          Deno.UnsafePointer.create(BigInt(i + 1)),
        ),
    ),
  );
  r.check(
    "50 calls at once each return their own value",
    many.every((v: bigint, i: number) => v === BigInt(i + 1)),
  );
  for (
    const [name, f] of [
      ["no fn", () => desktop.runOnMainThread()],
      ["a null fn", () => desktop.runOnMainThread(null)],
      ["a number fn", () => desktop.runOnMainThread(5)],
      ["a number context", () => desktop.runOnMainThread(nat.identity, 5)],
    ] as [string, () => unknown][]
  ) {
    r.check(
      `${name} is a TypeError`,
      (await rejects(f)).startsWith("TypeError"),
      await rejects(f),
    );
  }
});

await r.step("authSession", async () => {
  const a = desktop.authSession;
  r.check(
    "Deno.desktop.authSession is frozen with capabilities / start / cancel",
    typeof a === "object" && Object.isFrozen(a) &&
      JSON.stringify(Object.keys(a).sort()) ===
        '["cancel","capabilities","start"]',
  );
  // laufey API 43: cancel() with nothing running is a no-op answering false
  // (always so where no session can run).
  r.check(
    "cancel() with no session running answers false",
    a.cancel() === false,
  );
  const caps = a.capabilities();
  r.set("capabilities", caps);
  const os = Deno.build.os;
  r.check(
    os === "darwin"
      ? "macOS: supported, ephemeral"
      : "Windows / Linux: nothing supported",
    os === "darwin"
      ? caps.supported && caps.ephemeral
      : !caps.supported && !caps.ephemeral && !caps.httpsCallback,
    caps,
  );
  for (
    const [name, opts] of [
      ["no options", undefined],
      ["no url", { callbackScheme: "x" }],
      ["both callbacks", {
        url: "https://e.com",
        callbackScheme: "x",
        callbackUrl: "https://e.com/cb",
      }],
      ["no callback", { url: "https://e.com" }],
      ["an http callbackUrl", {
        url: "https://e.com",
        callbackUrl: "http://e.com/cb",
      }],
      ["a scheme with ://", {
        url: "https://e.com",
        callbackScheme: "myapp://cb",
      }],
      ["a non-boolean ephemeral", {
        url: "https://e.com",
        callbackScheme: "x",
        ephemeral: 1,
      }],
      ["a string window", {
        url: "https://e.com",
        callbackScheme: "x",
        window: "w",
      }],
    ] as [string, unknown][]
  ) {
    const got = await rejects(() => a.start(opts));
    r.check(
      `start() with ${name} is a TypeError`,
      got.startsWith("TypeError"),
      got,
    );
  }
  if (!caps.supported) {
    const got = await rejects(() =>
      a.start({ url: "https://example.com/authorize", callbackScheme: "x" })
    );
    r.check(
      "start() rejects AuthSessionError(not_supported)",
      got.startsWith("AuthSessionError(not_supported)"),
      got,
    );
    r.na(
      "a real OS auth session",
      "Windows and Linux have no OS auth session (RFC 8252: the system browser plus a loopback or claimed-scheme redirect)",
    );
    return;
  }
  const inv = await rejects(() =>
    a.start({ url: "ftp://x", callbackScheme: "dnxasmt", ephemeral: true })
  );
  r.check(
    "a non-http(s) url is AuthSessionError(invalid)",
    inv.startsWith("AuthSessionError(invalid)"),
    inv,
  );
  const unk = await rejects(() =>
    a.start({
      url: "https://example.com",
      callbackScheme: "dnxasmt",
      ephemeral: true,
      window: 999999,
    })
  );
  r.check(
    "an unknown window is AuthSessionError(invalid)",
    unk.startsWith("AuthSessionError(invalid)"),
    unk,
  );
  const idp = startIdp();
  const round = await within(
    a.start({
      url: `${idp}/redirect?state=s1`,
      callbackScheme: "dnxasmt",
      ephemeral: true,
    }).catch((e: Error) => describeError(e)),
    60000,
  );
  r.check(
    "a real ephemeral session ends at the callback with the provider's code and state",
    "value" in round &&
      (round.value as any)?.url === "dnxasmt://cb?code=probe-code&state=s1",
    "value" in round ? round.value : "no answer in 60 s",
  );
  const win = titledWindow("E2E Asmt anchor", { width: 500, height: 400 });
  await sleep(1500);
  const pending = rejects(() =>
    a.start({
      url: `${idp}/wait`,
      callbackScheme: "dnxasmt",
      ephemeral: true,
      window: win,
    })
  );
  await sleep(1500);
  const busy = await rejects(() =>
    a.start({ url: `${idp}/wait`, callbackScheme: "dnxasmt", ephemeral: true })
  );
  r.check(
    "a second session meanwhile is AuthSessionError(busy)",
    busy.startsWith("AuthSessionError(busy)"),
    busy,
  );
  win.close();
  const closed = await within(pending, 20000);
  r.check(
    "closing the anchor window cancels the session (AuthSessionError(cancelled))",
    "value" in closed &&
      String(closed.value).startsWith("AuthSessionError(cancelled)"),
    "value" in closed
      ? closed.value
      : "still pending 20 s after the window closed",
  );
  // The app gives up (laufey API 43): cancel() closes the sheet, the session
  // ends cancelled exactly once, a second cancel() finds nothing, and the
  // next session is not busy: it completes a real round trip.
  await sleep(1000);
  const running = rejects(() =>
    a.start({ url: `${idp}/wait`, callbackScheme: "dnxasmt", ephemeral: true })
  );
  await sleep(1500);
  const cancelled = a.cancel();
  const again = a.cancel();
  const ended = await within(running, 20000);
  r.check(
    "cancel() ends the running session as AuthSessionError(cancelled), once",
    cancelled === true && again === false && "value" in ended &&
      String(ended.value).startsWith("AuthSessionError(cancelled)"),
    {
      cancelled,
      again,
      ended: "value" in ended
        ? ended.value
        : "still pending 20 s after cancel()",
    },
  );
  const next = await within(
    a.start({
      url: `${idp}/redirect?state=s2`,
      callbackScheme: "dnxasmt",
      ephemeral: true,
    }).catch((e: Error) => describeError(e)),
    60000,
  );
  r.check(
    "the session after a cancel() completes (not busy)",
    "value" in next &&
      (next.value as any)?.url === "dnxasmt://cb?code=probe-code&state=s2",
    "value" in next ? next.value : "no answer in 60 s",
  );
  r.check("cancel() after it completed answers false", a.cancel() === false);
});

await r.step("workers and permissions", async () => {
  const w = new Worker(new URL("./worker.ts", import.meta.url), {
    type: "module",
  });
  const got: any = await new Promise((resolve) => {
    w.onmessage = (e) => resolve(e.data);
    w.onerror = (e) => {
      e.preventDefault();
      resolve(`worker error: ${e.message}`);
    };
    setTimeout(() => resolve("worker timeout"), 15000);
  });
  w.terminate();
  r.check(
    "workers have neither authSession nor runOnMainThread (nor the cancel op)",
    got?.authSession === "undefined" && got?.runOnMainThread === "undefined" &&
      got?.cancelOp === "undefined",
    got,
  );
  const nat = nativeSymbols();
  await Deno.permissions.revoke({ name: "ffi" });
  const denied = await rejects(() =>
    desktop.runOnMainThread(nat.identity, null)
  );
  r.check(
    "runOnMainThread needs --allow-ffi (NotCapable once revoked)",
    denied.startsWith("NotCapable"),
    denied,
  );
});

void titled;
r.finish();
