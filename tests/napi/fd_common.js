// Copyright 2018-2026 the Deno authors. MIT license.

import fs from "node:fs";

function errorCode(fn) {
  try {
    fn();
    return "no error";
  } catch (e) {
    return e.code;
  }
}

// node:fs on the pipe the addon opened: fstat, read, then close the write
// end, after which the (non-blocking) read end reads EOF only if the close
// reached the real descriptor; a read with it still open is EAGAIN.
export function readAddonPipe(lib, text) {
  const [readFd, writeFd] = lib.openPipe(text);
  const fifo = fs.fstatSync(readFd).isFIFO();
  const buf = new Uint8Array(64);
  const n = fs.readSync(readFd, buf, 0, buf.length, null);
  const read = new TextDecoder().decode(buf.subarray(0, n));
  const beforeClose = errorCode(() => fs.readSync(readFd, buf));
  fs.closeSync(writeFd);
  const eof = fs.readSync(readFd, buf);
  fs.closeSync(readFd);
  return { fifo, read, beforeClose, eof };
}

export const expected = (text) => ({
  fifo: true,
  read: text,
  beforeClose: "EAGAIN",
  eof: 0,
});
