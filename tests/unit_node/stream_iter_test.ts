// Copyright 2018-2026 the Deno authors. MIT license.

import { assert, assertEquals, assertRejects, assertThrows } from "@std/assert";
import { Readable } from "node:stream";
import {
  bytes,
  from,
  fromSync,
  pull,
  pullSync,
  text,
  textSync,
} from "node:stream/iter";
import {
  compressBrotli,
  compressDeflateSync,
  compressGzip,
  compressZstd,
  decompressBrotli,
  decompressDeflateSync,
  decompressGzip,
  decompressZstd,
} from "node:zlib/iter";
import { gunzipSync } from "node:zlib";

const input = "stream/iter ".repeat(10_000);

// node:zlib/iter's transforms drive the native zlib, Brotli and Zstd handles
// directly; each round-trips through its decompressor, and (for gzip) through
// node:zlib too.
Deno.test("[node/stream/iter] zlib/iter transforms round-trip", async () => {
  const pairs = [
    [compressGzip, decompressGzip],
    [compressBrotli, decompressBrotli],
    [compressZstd, decompressZstd],
  ] as const;
  for (const [compress, decompress] of pairs) {
    const out = await text(pull(from(input), compress(), decompress()));
    assertEquals(out, input);
  }

  const gzipped = await bytes(pull(from(input), compressGzip()));
  assertEquals(new TextDecoder().decode(gunzipSync(gzipped)), input);
});

Deno.test("[node/stream/iter] sync zlib/iter transforms round-trip", () => {
  const out = textSync(
    pullSync(fromSync(input), compressDeflateSync(), decompressDeflateSync()),
  );
  assertEquals(out, input);
});

Deno.test("[node/stream/iter] a corrupt stream rejects with the engine's error", async () => {
  const garbage = new Uint8Array(64).fill(7);
  const err = await assertRejects(() =>
    text(pull(from(garbage), decompressGzip()))
  ) as NodeJS.ErrnoException;
  assertEquals(err.code, "Z_DATA_ERROR");

  const syncErr = assertThrows(() =>
    textSync(pullSync(fromSync(garbage), decompressDeflateSync()))
  ) as NodeJS.ErrnoException;
  assertEquals(syncErr.code, "Z_DATA_ERROR");
});

Deno.test("[node/stream/iter] Readable streams are async streamables", async () => {
  const readable = Readable.from(["a", "b", "c"]);
  const key = Symbol.for("Stream.toAsyncStreamable");
  assert(
    typeof (readable as unknown as Record<symbol, unknown>)[key] === "function",
  );
  assertEquals(await text(from(readable)), "abc");
});

Deno.test("[node/stream/iter] Brotli and Zstd dictionaries are refused", async () => {
  for (const compress of [compressBrotli, compressZstd]) {
    const err = await assertRejects(() =>
      text(pull(from("x"), compress({ dictionary: new Uint8Array(8) })))
    ) as NodeJS.ErrnoException;
    assertEquals(err.code, "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM");
  }
});
