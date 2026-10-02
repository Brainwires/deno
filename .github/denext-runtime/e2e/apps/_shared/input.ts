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
    } as const,
  );
  return win;
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

/** Bring the window titled `title` to the front (best effort). */
export async function activate(title: string): Promise<void> {
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
    // Alt press counts (the usual workaround for SetForegroundWindow).
    const w = winLibs().symbols;
    const h = w.FindWindowW(null, wide(title));
    if (h) {
      w.keybd_event(0x12, 0, 0, 0n);
      w.SetForegroundWindow(h);
      w.keybd_event(0x12, 0, 2, 0n);
    }
  }
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
