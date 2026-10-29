// Copyright 2018-2026 the Deno authors. MIT license.
import { randomFillSync, randomUUID, timingSafeEqual } from "node:crypto";
import { Buffer } from "node:buffer";
import {
  assert,
  assertEquals,
  assertMatch,
  assertThrows,
} from "../../unit/test_util.ts";
import { assertNotEquals } from "@std/assert";

const UUID_REGEX =
  /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

Deno.test("[node/crypto.getRandomUUID] works the same way as Web Crypto API", () => {
  assertEquals(randomUUID().length, crypto.randomUUID().length);
  assertEquals(typeof randomUUID(), typeof crypto.randomUUID());
});

Deno.test("[node/crypto.randomUUID] supports disableEntropyCache", () => {
  assertMatch(randomUUID({ disableEntropyCache: true }), UUID_REGEX);
});

Deno.test("[node/crypto.randomUUIDv7] a version 7 UUID led by the time", async () => {
  // Node 26 API; the bundled @types/node predates it.
  const { randomUUIDv7 } = (await import("node:crypto")) as unknown as {
    randomUUIDv7: (options?: unknown) => string;
  };
  const UUID_V7_REGEX =
    /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
  const before = Date.now();
  const uuid = randomUUIDv7();
  const after = Date.now();
  assertMatch(uuid, UUID_V7_REGEX);
  const ms = parseInt(uuid.slice(0, 8) + uuid.slice(9, 13), 16);
  assert(ms >= before && ms <= after, `${ms} not in [${before}, ${after}]`);
  assertMatch(randomUUIDv7({ disableEntropyCache: true }), UUID_V7_REGEX);
  assertNotEquals(randomUUIDv7(), randomUUIDv7());
  assertThrows(() => randomUUIDv7(1), TypeError);
  assertThrows(() => randomUUIDv7({ disableEntropyCache: "" }), TypeError);
});

Deno.test("[node/crypto.randomFillSync] supported arguments", () => {
  const buf = new Uint8Array(10);

  assert(randomFillSync(buf));
  assert(randomFillSync(buf, 0));
  // @ts-ignore: arraybuffer arguments are valid.
  assert(randomFillSync(buf.buffer));
  assert(randomFillSync(new DataView(buf.buffer)));
});

Deno.test("[node/crypto.randomFillSync] array buffer view", () => {
  const buf = new Uint8Array(32);
  const view = new Uint8Array(buf.buffer, 8, 16);

  assert(randomFillSync(view));
  assertEquals(view.length, 16);
  assertNotEquals(view, new Uint8Array(16));
  assertEquals(buf.subarray(0, 8), new Uint8Array(8));
  assertEquals(buf.subarray(24, 32), new Uint8Array(8));
});

Deno.test("[node/crypto.timingSafeEqual] compares equal Buffer with different byteOffset", () => {
  const a = Buffer.from([212, 213]);
  const b = Buffer.from([0, 0, 0, 0, 0, 0, 0, 0, 212, 213]).subarray(8);

  assert(timingSafeEqual(a, b));
});

Deno.test("[node/crypto.timingSafeEqual] RangeError on Buffer with different byteLength", () => {
  const a = Buffer.from([212, 213]);
  const b = Buffer.from([0, 0, 0, 0, 0, 0, 0, 0, 212, 213, 0]);

  assertThrows(() => timingSafeEqual(a, b), RangeError);
});
