// Copyright 2018-2026 the Deno authors. MIT license.
// The auth-session and main-thread ops are excluded from workers.
// deno-lint-ignore-file no-explicit-any
const d = (Deno as any).desktop;
(self as any).postMessage({
  authSession: typeof d?.authSession,
  runOnMainThread: typeof d?.runOnMainThread,
});
