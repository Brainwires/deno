// Copyright 2018-2026 the Deno authors. MIT license.
// What a person at the machine would do, done from inside the test app:
// press a key combination and click a window's close button.
//
// - Key presses: XTEST through `xdotool` on Linux, `keybd_event` on Windows,
//   CoreGraphics events on macOS (which needs the Accessibility grant of the
//   process that started the app; `canPressKeys()` says whether it is there).
// - A user close: `wmctrl -c` (the window manager's close, WM_DELETE_WINDOW)
//   on Linux, WM_CLOSE posted to the top-level window on Windows,
//   `-[NSWindow performClose:]` (what the close button calls) scheduled on
//   the main thread on macOS.

// deno-lint-ignore-file no-explicit-any

import { OS } from "./e2e.ts";

/** A key combination: modifiers plus one key. `Primary` is Command on macOS
 * and Control elsewhere (an accelerator's `CommandOrControl`). */
export type Key =
  | "Primary"
  | "Control"
  | "Alt"
  | "Shift"
  | "Super"
  | "F9"
  | "K"
  | "Escape"
  | "Down"
  | "Return";

async function run(cmd: string, args: string[]): Promise<string> {
  const out = await new Deno.Command(cmd, {
    args,
    stdout: "piped",
    stderr: "piped",
  }).output();
  const text = new TextDecoder().decode(out.stdout) +
    new TextDecoder().decode(out.stderr);
  if (!out.success) throw new Error(`${cmd} ${args.join(" ")}: ${text}`);
  return text;
}

// --- macOS ---------------------------------------------------------------

let mac: any;
function macLibs() {
  if (mac) return mac;
  const cg = Deno.dlopen(
    "/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics",
    {
      CGEventCreateKeyboardEvent: {
        parameters: ["pointer", "u16", "bool"],
        result: "pointer",
      },
      CGEventSetFlags: { parameters: ["pointer", "u64"], result: "void" },
      CGEventPost: { parameters: ["u32", "pointer"], result: "void" },
    } as const,
  );
  const cf = Deno.dlopen(
    "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation",
    { CFRelease: { parameters: ["pointer"], result: "void" } } as const,
  );
  const ax = Deno.dlopen(
    "/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices",
    { AXIsProcessTrusted: { parameters: [], result: "bool" } } as const,
  );
  const objc = Deno.dlopen(
    "/usr/lib/libobjc.A.dylib",
    {
      objc_getClass: { parameters: ["buffer"], result: "pointer" },
      sel_registerName: { parameters: ["buffer"], result: "pointer" },
      msg: {
        name: "objc_msgSend",
        parameters: ["pointer", "pointer"],
        result: "pointer",
      },
      msgIndex: {
        name: "objc_msgSend",
        parameters: ["pointer", "pointer", "usize"],
        result: "pointer",
      },
      msgCount: {
        name: "objc_msgSend",
        parameters: ["pointer", "pointer"],
        result: "usize",
      },
      msgU8: {
        name: "objc_msgSend",
        parameters: ["pointer", "pointer"],
        result: "u8",
      },
      msgPerformOnMain: {
        name: "objc_msgSend",
        parameters: ["pointer", "pointer", "pointer", "pointer", "bool"],
        result: "void",
      },
    } as const,
  );
  // AppKit must be loaded for NSApplication (it is: the host is an AppKit
  // app); dlopen it anyway so objc_getClass finds the class.
  Deno.dlopen("/System/Library/Frameworks/AppKit.framework/AppKit", {});
  mac = { cg, cf, ax, objc };
  return mac;
}

const cstr = (s: string) => new TextEncoder().encode(s + "\0");

const MAC_KEYCODES: Record<string, number> = {
  F9: 101,
  K: 40,
  Escape: 53,
  Down: 125,
  Return: 36,
};
const MAC_FLAGS: Record<string, bigint> = {
  Primary: 0x100000n,
  Super: 0x100000n,
  Shift: 0x20000n,
  Control: 0x40000n,
  Alt: 0x80000n,
};

function macPress(keys: Key[]) {
  const { cg, cf } = macLibs();
  const mods = keys.filter((k) => k in MAC_FLAGS);
  const key = keys.find((k) => !(k in MAC_FLAGS))!;
  const flags = mods.reduce((f, m) => f | MAC_FLAGS[m], 0n);
  for (const down of [true, false]) {
    const ev = cg.symbols.CGEventCreateKeyboardEvent(
      null,
      MAC_KEYCODES[key],
      down,
    );
    cg.symbols.CGEventSetFlags(ev, flags);
    cg.symbols.CGEventPost(0, /* kCGHIDEventTap */ ev);
    cf.symbols.CFRelease(ev);
  }
}

/** The NSWindow titled `title`, or null. */
function macWindow(title: string): Deno.PointerValue {
  const { objc } = macLibs();
  const s = objc.symbols;
  const sel = (n: string) => s.sel_registerName(cstr(n));
  const app = s.msg(
    s.objc_getClass(cstr("NSApplication")),
    sel("sharedApplication"),
  );
  const windows = s.msg(app, sel("windows"));
  const n = Number(s.msgCount(windows, sel("count")));
  for (let i = 0; i < n; i++) {
    const w = s.msgIndex(windows, sel("objectAtIndex:"), BigInt(i));
    const t = s.msg(w, sel("title"));
    const p = t ? s.msg(t, sel("UTF8String")) : null;
    if (p && Deno.UnsafePointerView.getCString(p) === title) return w;
  }
  return null;
}

// --- Windows -------------------------------------------------------------

let win: any;
function winLibs() {
  if (win) return win;
  win = Deno.dlopen(
    "user32.dll",
    {
      keybd_event: {
        parameters: ["u8", "u8", "u32", "usize"],
        result: "void",
      },
      FindWindowW: { parameters: ["pointer", "buffer"], result: "pointer" },
      PostMessageW: {
        parameters: ["pointer", "u32", "usize", "isize"],
        result: "i32",
      },
      SetForegroundWindow: { parameters: ["pointer"], result: "i32" },
      GetForegroundWindow: { parameters: [], result: "pointer" },
      GetWindowThreadProcessId: {
        parameters: ["pointer", "buffer"],
        result: "u32",
      },
      AttachThreadInput: { parameters: ["u32", "u32", "i32"], result: "i32" },
      BringWindowToTop: { parameters: ["pointer"], result: "i32" },
      IsWindowVisible: { parameters: ["pointer"], result: "i32" },
      IsWindowEnabled: { parameters: ["pointer"], result: "i32" },
      IsIconic: { parameters: ["pointer"], result: "i32" },
      GetClassNameW: {
        parameters: ["pointer", "buffer", "i32"],
        result: "i32",
      },
      GetWindowTextW: {
        parameters: ["pointer", "buffer", "i32"],
        result: "i32",
      },
      OpenInputDesktop: {
        parameters: ["u32", "i32", "u32"],
        result: "pointer",
      },
      CloseDesktop: { parameters: ["pointer"], result: "i32" },
    } as const,
  );
  return win;
}

let k32: any;
function kernel32() {
  if (k32) return k32;
  k32 = Deno.dlopen(
    "kernel32.dll",
    {
      GetCurrentThreadId: { parameters: [], result: "u32" },
      OpenProcess: { parameters: ["u32", "i32", "u32"], result: "pointer" },
      QueryFullProcessImageNameW: {
        parameters: ["pointer", "u32", "buffer", "buffer"],
        result: "i32",
      },
      CloseHandle: { parameters: ["pointer"], result: "i32" },
      ProcessIdToSessionId: { parameters: ["u32", "buffer"], result: "i32" },
      WTSGetActiveConsoleSessionId: { parameters: [], result: "u32" },
    } as const,
  );
  return k32;
}

function wideString(buf: Uint8Array, chars: number): string {
  return String.fromCharCode(
    ...new Uint16Array(buf.buffer, 0, Math.max(0, chars)),
  );
}

/** A window's title, class, owning pid and that process's executable. */
function describeWindow(h: Deno.PointerValue) {
  if (!h) return null;
  const u = winLibs().symbols;
  const k = kernel32().symbols;
  const text = new Uint8Array(512);
  const cls = new Uint8Array(512);
  const pidBuf = new Uint32Array(1);
  const tid = u.GetWindowThreadProcessId(h, new Uint8Array(pidBuf.buffer));
  const pid = pidBuf[0];
  let exe: string | null = null;
  // PROCESS_QUERY_LIMITED_INFORMATION
  const ph = k.OpenProcess(0x1000, 0, pid);
  if (ph) {
    const name = new Uint8Array(1024);
    const len = new Uint32Array([512]);
    if (k.QueryFullProcessImageNameW(ph, 0, name, new Uint8Array(len.buffer))) {
      exe = wideString(name, len[0]);
    }
    k.CloseHandle(ph);
  }
  return {
    title: wideString(text, u.GetWindowTextW(h, text, 256)),
    class: wideString(cls, u.GetClassNameW(h, cls, 256)),
    pid,
    tid,
    exe,
  };
}

const wide = (s: string) => {
  const b = new Uint16Array(s.length + 1);
  for (let i = 0; i < s.length; i++) b[i] = s.charCodeAt(i);
  return new Uint8Array(b.buffer);
};

const WIN_VK: Record<string, number> = {
  Primary: 0x11,
  Control: 0x11,
  Alt: 0x12,
  Shift: 0x10,
  Super: 0x5B,
  F9: 0x78,
  K: 0x4B,
  Escape: 0x1B,
  Down: 0x28,
  Return: 0x0D,
};

function winPress(keys: Key[]) {
  const u = winLibs().symbols;
  for (const k of keys) u.keybd_event(WIN_VK[k], 0, 0, 0n);
  for (const k of [...keys].reverse()) u.keybd_event(WIN_VK[k], 0, 2, 0n);
}

// --- Linux ---------------------------------------------------------------

const XDO: Record<string, string> = {
  Primary: "ctrl",
  Control: "ctrl",
  Alt: "alt",
  Shift: "shift",
  Super: "super",
  F9: "F9",
  K: "k",
  Escape: "Escape",
  Down: "Down",
  Return: "Return",
};

// --- the API -------------------------------------------------------------

/** Whether this process may synthesize key presses (null = yes; else the
 * reason it can't). */
export async function canPressKeys(): Promise<string | null> {
  if (OS === "darwin") {
    try {
      return macLibs().ax.symbols.AXIsProcessTrusted()
        ? null
        : "the process that started the app has no Accessibility grant, which macOS requires to post key events";
    } catch (e) {
      return `no CoreGraphics / ApplicationServices: ${e}`;
    }
  }
  if (OS === "linux") {
    try {
      await run("xdotool", ["version"]);
      return Deno.env.get("DISPLAY") ? null : "no X display (Wayland or none)";
    } catch {
      return "xdotool is not installed";
    }
  }
  return null;
}

/** Press and release `keys` (modifiers first). */
export async function pressKeys(keys: Key[]): Promise<void> {
  if (OS === "darwin") return macPress(keys);
  if (OS === "windows") return winPress(keys);
  await run("xdotool", [
    "key",
    "--clearmodifiers",
    keys.map((k) => XDO[k]).join("+"),
  ]);
}

/** Bring the window titled `title` to the front. Resolves with whether it
 * is known to be there: on Windows, whether it became the foreground window
 * (synthesized keys go to the foreground window, so a press made while
 * another window has it is lost); elsewhere true after the attempt. */
export async function activate(title: string): Promise<boolean> {
  if (OS === "linux") {
    await run("xdotool", [
      "search",
      "--name",
      `^${title}$`,
      "windowactivate",
      "--sync",
    ])
      .catch(() => {});
  } else if (OS === "windows") {
    // Windows lets a process take the foreground only right after input; an
    // Alt press counts (the usual workaround for SetForegroundWindow). The
    // shell can still refuse or hand the foreground back a moment later
    // (a hosted runner's shell is busy right after a launch), so ask until
    // the window is the foreground one, for up to 5 s.
    // Every other attempt also joins the foreground window's input queue
    // (AttachThreadInput), which the shell honours when the Alt press alone
    // is not enough.
    const w = winLibs().symbols;
    const deadline = Date.now() + 5000;
    for (let attempt = 0;; attempt++) {
      const h = w.FindWindowW(null, wide(title));
      if (h) {
        const fg = w.GetForegroundWindow();
        const fgTid = fg ? w.GetWindowThreadProcessId(fg, null) : 0;
        const ownTid = kernel32().symbols.GetCurrentThreadId();
        const attach = attempt % 2 === 1 && fgTid !== 0 && fgTid !== ownTid;
        if (attach) w.AttachThreadInput(ownTid, fgTid, 1);
        w.keybd_event(0x12, 0, 0, 0n);
        w.BringWindowToTop(h);
        w.SetForegroundWindow(h);
        // A key between the Alt press and its release, so the release is not
        // an Alt tap: a tap that reaches the window (now the foreground one)
        // opens its menu bar's keyboard mode, where the accelerator pressed
        // next is a menu key and never fires (seen on a Windows on Arm VM's
        // console session, WebView2). F24 is bound to nothing.
        w.keybd_event(0x87, 0, 0, 0n);
        w.keybd_event(0x87, 0, 2, 0n);
        w.keybd_event(0x12, 0, 2, 0n);
        if (attach) w.AttachThreadInput(ownTid, fgTid, 0);
        await new Promise((r) => setTimeout(r, 100));
        const now = w.GetForegroundWindow();
        if (now && Deno.UnsafePointer.equals(now, h)) return true;
      }
      if (Date.now() >= deadline) return false;
      await new Promise((r) => setTimeout(r, 150));
    }
  }
  return true;
}

/** Windows, after `activate(title)` failed: why this session would not
 * give the window the foreground, when that is the session's doing and not
 * the window's. Returns `{ refusal }` (a runner limitation, with what was
 * detected) or `{ refusal: null }` (the window itself looks wrong: missing,
 * hidden, disabled or minimized, so the failure is the app's) and the
 * evidence either way. */
export function foregroundDiagnosis(title: string): {
  refusal: string | null;
  evidence: Record<string, unknown>;
} {
  if (OS !== "windows") return { refusal: null, evidence: {} };
  const u = winLibs().symbols;
  const k = kernel32().symbols;
  const h = u.FindWindowW(null, wide(title));
  const fg = u.GetForegroundWindow();
  const sid = new Uint32Array(1);
  const haveSid = k.ProcessIdToSessionId(Deno.pid, new Uint8Array(sid.buffer));
  const consoleSession = k.WTSGetActiveConsoleSessionId();
  // DESKTOP_READOBJECTS: can this process open the input desktop at all?
  const desk = u.OpenInputDesktop(0, 0, 0x0001);
  if (desk) u.CloseDesktop(desk);
  const evidence: Record<string, unknown> = {
    window: h
      ? {
        ...describeWindow(h),
        visible: u.IsWindowVisible(h) !== 0,
        enabled: u.IsWindowEnabled(h) !== 0,
        minimized: u.IsIconic(h) !== 0,
      }
      : null,
    foreground: fg ? describeWindow(fg) : null,
    session: haveSid ? sid[0] : null,
    consoleSession,
    inputDesktop: desk !== null,
  };
  const win = evidence.window as Record<string, unknown> | null;
  if (!win || !win.visible || !win.enabled || win.minimized) {
    return { refusal: null, evidence };
  }
  if (!desk) {
    return {
      refusal:
        "this session can't open the input desktop (OpenInputDesktop failed), so no window of it can hold the foreground",
      evidence,
    };
  }
  if (haveSid && consoleSession !== 0xFFFFFFFF && sid[0] !== consoleSession) {
    return {
      refusal: `this process runs in session ${
        sid[0]
      }, not the console session ${consoleSession}, so Windows keeps the foreground away from its windows`,
      evidence,
    };
  }
  if (!fg) {
    return {
      refusal:
        "no window holds the foreground in this session (GetForegroundWindow is NULL), and Windows refused it to the app's visible, enabled window",
      evidence,
    };
  }
  const f = evidence.foreground as { pid?: number; exe?: string | null };
  if (f.pid !== undefined && f.pid !== win.pid) {
    return {
      refusal: `Windows kept the foreground on another process's window (${
        f.exe ?? `pid ${f.pid}`
      }) and refused SetForegroundWindow to the app's visible, enabled window, even with an Alt press and AttachThreadInput`,
      evidence,
    };
  }
  return { refusal: null, evidence };
}

/** Windows: whether the window titled `title` is the foreground window
 * (null elsewhere). */
export function isForeground(title: string): boolean | null {
  if (OS !== "windows") return null;
  const w = winLibs().symbols;
  const h = w.FindWindowW(null, wide(title));
  const fg = w.GetForegroundWindow();
  return !!h && !!fg && Deno.UnsafePointer.equals(fg, h);
}

/** macOS: the NSWindow titled `title`'s style mask and state, for a
 * failure report (null elsewhere, or when there is no such window). */
export function macWindowState(title: string): Record<string, unknown> | null {
  if (OS !== "darwin") return null;
  const s = macLibs().objc.symbols;
  const w = macWindow(title);
  if (!w) return null;
  const sel = (n: string) => s.sel_registerName(cstr(n));
  const mask = Number(s.msgCount(w, sel("styleMask")));
  return {
    styleMask: mask,
    miniaturizable: (mask & 4) !== 0,
    isMiniaturized: s.msgU8(w, sel("isMiniaturized")) !== 0,
    isVisible: s.msgU8(w, sel("isVisible")) !== 0,
    isKeyWindow: s.msgU8(w, sel("isKeyWindow")) !== 0,
  };
}

/** Windows: whether a top-level window titled `title` exists (null
 * elsewhere). */
export function topLevelExists(title: string): boolean | null {
  if (OS !== "windows") return null;
  return winLibs().symbols.FindWindowW(null, wide(title)) !== null;
}

/** Ask to close the window titled `title` the way its close button does.
 * Resolves with null, or why it couldn't. */
export async function userClose(title: string): Promise<string | null> {
  if (OS === "windows") {
    const w = winLibs().symbols;
    const h = w.FindWindowW(null, wide(title));
    if (!h) return `no top-level window titled ${title}`;
    w.PostMessageW(h, 0x0010, /* WM_CLOSE */ 0n, 0n);
    return null;
  }
  if (OS === "linux") {
    try {
      await run("wmctrl", ["-c", title]);
      return null;
    } catch (e) {
      return String(e);
    }
  }
  const { objc } = macLibs();
  const s = objc.symbols;
  const w = macWindow(title);
  if (!w) return `no NSWindow titled ${title}`;
  s.msgPerformOnMain(
    w,
    s.sel_registerName(
      cstr("performSelectorOnMainThread:withObject:waitUntilDone:"),
    ),
    s.sel_registerName(cstr("performClose:")),
    null,
    false,
  );
  return null;
}
