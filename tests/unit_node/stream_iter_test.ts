// Copyright 2018-2026 the Deno authors. MIT license.

import { assert, assertEquals, assertRejects, assertThrows } from "@std/assert";
import { Readable } from "node:stream";
import {
  bytes,
  bytesSync,
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

// A small compressed chunk can stand for a huge output (a decompression
// bomb). The decompressors hand each full batch to the consumer before the
// engine produces more, so one that stops after the first batch never holds
// the rest in memory. Node 26.5.1 inflated a whole input chunk first.
Deno.test("[node/stream/iter] decompression yields before inflating a whole chunk", async () => {
  const size = 64 * 1024 * 1024;
  const original = new Uint8Array(size);
  const limit = 16 * 1024 * 1024;
  const pairs = [
    [compressGzip, decompressGzip],
    [compressBrotli, decompressBrotli],
    [compressZstd, decompressZstd],
  ] as const;
  for (const [compress, decompress] of pairs) {
    const compressed = await bytes(pull(from(original), compress()));
    assert(compressed.byteLength < 1024 * 1024);
    const before = Deno.memoryUsage().external;
    let grew = -1;
    for await (const batch of pull(from(compressed), decompress())) {
      assert(batch.length > 0);
      grew = Deno.memoryUsage().external - before;
      break;
    }
    assert(grew >= 0 && grew < limit, `${decompress.name} grew ${grew}`);
  }

  const compressed = bytesSync(
    pullSync(fromSync(original), compressDeflateSync()),
  );
  const before = Deno.memoryUsage().external;
  let grew = -1;
  for (const batch of pullSync(fromSync(compressed), decompressDeflateSync())) {
    assert(batch.length > 0);
    grew = Deno.memoryUsage().external - before;
    break;
  }
  assert(grew >= 0 && grew < limit, `decompressDeflateSync grew ${grew}`);
  // Keep the original alive until here, so its release can't hide growth.
  assertEquals(original.byteLength, size);
});

Deno.test("[node/stream/iter] large outputs still round-trip in batches", async () => {
  const original = new Uint8Array(4 * 1024 * 1024);
  for (let i = 0; i < original.length; i++) original[i] = i % 251;
  for (
    const [compress, decompress] of [
      [compressGzip, decompressGzip],
      [compressBrotli, decompressBrotli],
      [compressZstd, decompressZstd],
    ] as const
  ) {
    const out = await bytes(pull(from(original), compress(), decompress()));
    assertEquals(out.byteLength, original.byteLength);
    assertEquals(out, original);
  }
  const out = bytesSync(
    pullSync(
      fromSync(original),
      compressDeflateSync(),
      decompressDeflateSync(),
    ),
  );
  assertEquals(out, original);
});
