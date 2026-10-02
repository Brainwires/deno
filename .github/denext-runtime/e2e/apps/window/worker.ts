// Copyright 2018-2026 the Deno authors. MIT license.
// The native desktop classes need the desktop backend, which only the main
// scope has: a worker gets none of them (constructing one used to panic).
// deno-lint-ignore-file no-explicit-any
const ops = (Deno as any)[(Deno as any).internal]?.core?.ops ?? {};
(self as any).postMessage({
  BrowserWindow: typeof ops.BrowserWindow,
  Dock: typeof ops.Dock,
  Tray: typeof ops.Tray,
  Notification: typeof ops.Notification,
});
