// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// libc through FFI: every attempt the enforce tests make is native code,
// which only the kernel sandbox can hold (Deno's own checks never see it).

const isMac = Deno.build.os === "darwin";

export const libc = Deno.dlopen(
  isMac ? "/usr/lib/libSystem.B.dylib" : "libc.so.6",
  {
    open: { parameters: ["buffer", "i32"], result: "i32" },
    creat: { parameters: ["buffer", "u32"], result: "i32" },
    close: { parameters: ["i32"], result: "i32" },
    unlink: { parameters: ["buffer"], result: "i32" },
    rename: { parameters: ["buffer", "buffer"], result: "i32" },
    mkdir: { parameters: ["buffer", "u32"], result: "i32" },
    chmod: { parameters: ["buffer", "u32"], result: "i32" },
    truncate: { parameters: ["buffer", "i64"], result: "i32" },
    utimes: { parameters: ["buffer", "pointer"], result: "i32" },
    socket: { parameters: ["i32", "i32", "i32"], result: "i32" },
    connect: { parameters: ["i32", "buffer", "u32"], result: "i32" },
    dlopen: { parameters: ["buffer", "i32"], result: "pointer" },
    [isMac ? "__error" : "__errno_location"]: {
      parameters: [],
      result: "pointer",
    },
  } as const,
);

export const O_RDONLY = 0;
export const O_WRONLY = 1;
export const O_TRUNC = isMac ? 0x400 : 0x200;

export function cstr(s: string): Uint8Array<ArrayBuffer> {
  return new TextEncoder().encode(s + "\0");
}

export function errno(): number {
  // deno-lint-ignore no-explicit-any
  const sym = (libc.symbols as any)[isMac ? "__error" : "__errno_location"];
  return new Deno.UnsafePointerView(sym()).getInt32();
}

/** Prints `<label>: ok`, `<label>: refused` (EPERM / EACCES) or the errno. */
export function check(label: string, rc: number | bigint) {
  if (Number(rc) >= 0) {
    console.log(`${label}: ok`);
    return;
  }
  const e = errno();
  console.log(`${label}: ${e === 1 || e === 13 ? "refused" : `errno ${e}`}`);
}

/** Opens `path` for reading and closes it again: `<label>: ok/refused`. */
export function openRead(label: string, path: string) {
  const fd = libc.symbols.open(cstr(path), O_RDONLY);
  check(label, fd);
  if (fd >= 0) libc.symbols.close(fd);
}

/** A TCP connect to 127.0.0.1:`port` from native code. */
export function tcpConnect(label: string, port: number) {
  const fd = libc.symbols.socket(2, 1, 0);
  if (fd < 0) {
    check(label, fd);
    return;
  }
  const sa = new Uint8Array(16);
  const view = new DataView(sa.buffer);
  if (isMac) {
    sa[0] = 16;
    sa[1] = 2;
  } else {
    view.setUint16(0, 2, true);
  }
  view.setUint16(2, port, false);
  sa.set([127, 0, 0, 1], 4);
  const rc = libc.symbols.connect(fd, sa, 16);
  check(label, rc);
  libc.symbols.close(fd);
}
