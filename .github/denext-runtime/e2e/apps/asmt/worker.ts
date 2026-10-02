// Copyright 2018-2026 the Deno authors. MIT license.
// The auth-session and main-thread ops are excluded from workers.
// deno-lint-ignore-file no-explicit-any
const d = (Deno as any).desktop;
(self as any).postMessage({
  authSession: typeof d?.authSession,
  runOnMainThread: typeof d?.runOnMainThread,
  cancelOp: typeof (Deno as any)[(Deno as any).internal]?.core?.ops
    ?.op_desktop_auth_session_cancel,
});
