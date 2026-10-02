// Copyright 2018-2026 the Deno authors. MIT license.
// The passkey ops are excluded from workers.
// deno-lint-ignore-file no-explicit-any
const d = (Deno as any).desktop;
(self as any).postMessage({ passkeys: typeof d?.passkeys });
