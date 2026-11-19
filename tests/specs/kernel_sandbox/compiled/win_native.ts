// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// Windows: kernel32 / the C runtime through FFI. Every attempt is native
// code, which only the OS holds (Deno's own checks never see it).
export const kernel32 = Deno.dlopen(
  "kernel32.dll",
  {
    CreateFileW: {
      parameters: ["buffer", "u32", "u32", "pointer", "u32", "u32", "pointer"],
      result: "pointer",
    },
    CloseHandle: { parameters: ["pointer"], result: "i32" },
    DeleteFileW: { parameters: ["buffer"], result: "i32" },
    GetLastError: { parameters: [], result: "u32" },
  } as const,
);

export const crt = Deno.dlopen(
  "ucrtbase.dll",
  {
    system: { parameters: ["buffer"], result: "i32" },
  } as const,
);

export function wstr(s: string): Uint8Array<ArrayBuffer> {
  const out = new Uint16Array(s.length + 1);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return new Uint8Array(out.buffer);
}

export function cstr(s: string): Uint8Array<ArrayBuffer> {
  return new TextEncoder().encode(s + "\0");
}

const GENERIC_READ = 0x80000000;
const GENERIC_WRITE = 0x40000000;
const SHARE_ALL = 7;
const OPEN_EXISTING = 3;
const CREATE_ALWAYS = 2;
const INVALID = 0xffffffffffffffffn;

function report(label: string, ok: boolean) {
  // 5: ERROR_ACCESS_DENIED
  const err = ok ? 0 : kernel32.symbols.GetLastError();
  console.log(
    `${label}: ${ok ? "ok" : err === 5 ? "refused" : `error ${err}`}`,
  );
}

function open(label: string, path: string, access: number, how: number) {
  const handle = kernel32.symbols.CreateFileW(
    wstr(path),
    access,
    SHARE_ALL,
    null,
    how,
    0x80,
    null,
  );
  const ok = handle !== null &&
    BigInt(Deno.UnsafePointer.value(handle)) !== INVALID;
  report(label, ok);
  if (ok) kernel32.symbols.CloseHandle(handle);
}

export const openRead = (label: string, path: string) =>
  open(label, path, GENERIC_READ, OPEN_EXISTING);
export const create = (label: string, path: string) =>
  open(label, path, GENERIC_WRITE, CREATE_ALWAYS);
export const openWrite = (label: string, path: string) =>
  open(label, path, GENERIC_WRITE, OPEN_EXISTING);
export const remove = (label: string, path: string) =>
  report(label, kernel32.symbols.DeleteFileW(wstr(path)) !== 0);
